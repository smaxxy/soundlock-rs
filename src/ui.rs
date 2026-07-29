use crate::audio::session::SessionInfo;
use crate::config::{Config, OperationMode};
use crate::{AppState, audio};
use cpal::Device;
use cpal::traits::{DeviceTrait, HostTrait};
use egui::*;
use std::sync::{Arc, Mutex};

pub struct SettingsWindow {
    state: Arc<Mutex<AppState>>,
    config: Arc<Mutex<Config>>,
    sessions: Vec<SessionInfo>,
    last_refresh: std::time::Instant,
    input_devices: Vec<(Device, String)>,
    output_devices: Vec<(Device, String)>,
    selected_input_idx: usize,
    selected_output_idx: usize,
    retry_start: bool,
    retry_stop: bool,
    fonts_loaded: bool,
}

impl SettingsWindow {
    pub fn new(state: Arc<Mutex<AppState>>, config: Arc<Mutex<Config>>) -> Self {
        let sessions = Self::enumerate_sessions();
        let (input_devices, output_devices) = Self::get_devices();

        let (selected_input_idx, selected_output_idx) = {
            match config.try_lock() {
                Ok(cfg) => (
                    cfg.target_input_device_id
                        .as_ref()
                        .and_then(|id| find_device_index(&input_devices, id))
                        .unwrap_or(usize::MAX),
                    cfg.target_output_device_id
                        .as_ref()
                        .and_then(|id| find_device_index(&output_devices, id))
                        .unwrap_or(usize::MAX),
                ),
                Err(_) => {
                    log::error!("Config lock poisoned during init");
                    (usize::MAX, usize::MAX)
                }
            }
        };

        Self {
            state,
            config,
            sessions,
            last_refresh: std::time::Instant::now(),
            input_devices,
            output_devices,
            selected_input_idx,
            selected_output_idx,
            retry_start: false,
            retry_stop: false,
            fonts_loaded: false,
        }
    }

    fn enumerate_sessions() -> Vec<SessionInfo> {
        match crate::audio::session::enumerate_sessions() {
            Ok(sessions) => {
                log::debug!("Found {} audio sessions", sessions.len());
                sessions
            }
            Err(e) => {
                log::error!("Failed to enumerate sessions: {}", e);
                vec![]
            }
        }
    }

    fn get_devices() -> (Vec<(Device, String)>, Vec<(Device, String)>) {
        let host = cpal::default_host();

        let input_devices = host
            .input_devices()
            .map(|devices| {
                devices
                    .filter_map(|d| {
                        d.id().ok().map(|id| (d, id.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default();

        let output_devices = host
            .output_devices()
            .map(|devices| {
                devices
                    .filter_map(|d| {
                        d.id().ok().map(|id| (d, id.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default();

        (input_devices, output_devices)
    }

    fn refresh_if_needed(&mut self) {
        let now = std::time::Instant::now();
        if now.duration_since(self.last_refresh) > std::time::Duration::from_secs(2) {
            self.sessions = Self::enumerate_sessions();
            let (input, output) = Self::get_devices();
            self.input_devices = input;
            self.output_devices = output;

            if let Ok(cfg) = self.config.try_lock() {
                self.selected_input_idx = cfg
                    .target_input_device_id
                    .as_ref()
                    .and_then(|id| find_device_index(&self.input_devices, id))
                    .unwrap_or(usize::MAX);
                self.selected_output_idx = cfg
                    .target_output_device_id
                    .as_ref()
                    .and_then(|id| find_device_index(&self.output_devices, id))
                    .unwrap_or(usize::MAX);
            }
            self.last_refresh = now;
        }
    }

    fn refresh_sessions(&mut self) {
        self.sessions = Self::enumerate_sessions();
        self.last_refresh = std::time::Instant::now();
    }

    fn save_config(&self, config: &Config) {
        match config.save() {
            Ok(_) => log::info!("Config saved"),
            Err(e) => log::error!("Failed to save config: {}", e),
        }
    }

    fn start_limiting(&mut self) {
        let (mode, pid, input_id, output_id) = {
            match self.config.try_lock() {
                Ok(cfg) => {
                    let mode = cfg.operation_mode;
                    let pid = cfg.target_pid;
                    let input_id = cfg.target_input_device_id.clone();
                    let output_id = cfg.target_output_device_id.clone();
                    self.save_config(&*cfg);
                    (mode, pid, input_id, output_id)
                }
                Err(_) => {
                    log::error!("Config lock poisoned");
                    return;
                }
            }
        };

        if mode == OperationMode::WindowsAPI && pid.is_none() {
            log::warn!("Cannot start limiting: no application selected");
            return;
        }
        if mode == OperationMode::Cable && (input_id.is_none() || output_id.is_none()) {
            log::warn!("Cannot start limiting: no audio devices selected");
            return;
        }

        match self.state.try_lock() {
            Ok(mut state) => {
                state.is_limiting = true;
                self.retry_start = false;
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                log::warn!("State lock busy, will retry start next frame");
                self.retry_start = true;
                return;
            }
            Err(_) => {
                log::error!("State lock poisoned");
                return;
            }
        }

        log::info!("Starting limiter");
        audio::start_limiter(Arc::clone(&self.state), Arc::clone(&self.config));
    }

    fn stop_limiting(&mut self) {
        log::info!("Stopping limiter");
        match self.state.try_lock() {
            Ok(mut state) => {
                state.is_limiting = false;
                self.retry_stop = false;
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                log::warn!("State lock busy, will retry stop next frame");
                self.retry_stop = true;
            }
            Err(e) => {
                log::error!("Failed to stop limiter: {:?}", e);
                self.retry_stop = false;
            }
        }
    }
}

impl eframe::App for SettingsWindow {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        if !self.fonts_loaded {
            let mut fonts = egui::FontDefinitions::default();
            if let Ok(font_data) = std::fs::read("C:\\Windows\\Fonts\\msyh.ttc") {
                fonts.font_data.insert("MicrosoftYaHei".to_owned(), 
                    egui::FontData::from_owned(font_data));
                fonts.families.entry(egui::FontFamily::Proportional)
                    .or_default()
                    .insert(0, "MicrosoftYaHei".to_owned());
                ctx.set_fonts(fonts);
            } else if let Ok(font_data) = std::fs::read("C:\\Windows\\Fonts\\simsun.ttc") {
                fonts.font_data.insert("SimSun".to_owned(), 
                    egui::FontData::from_owned(font_data));
                fonts.families.entry(egui::FontFamily::Proportional)
                    .or_default()
                    .insert(0, "SimSun".to_owned());
                ctx.set_fonts(fonts);
            } else {
                log::warn!("未找到中文字体");
            }
            self.fonts_loaded = true;
        }

        self.refresh_if_needed();

        if self.retry_start {
            self.start_limiting();
        }

        if self.retry_stop {
            self.stop_limiting();
        }

        let is_limiting = match self.state.try_lock() {
            Ok(state) => state.is_limiting,
            Err(_) => {
                ctx.request_repaint();
                return;
            }
        };

        let (
            mut threshold_db,
            mut selected_pid,
            mut attack_ms,
            mut release_ms,
            mut scan_interval_ms,
            mut volume_change_percentage_threshold,
            mut operation_mode,
            mut crossover_freq,   // 新增
        ) = match self.config.try_lock() {
            Ok(config) => (
                config.threshold_db,
                config.target_pid,
                config.attack_ms,
                config.release_ms,
                config.scan_interval_ms,
                config.volume_change_percentage_threshold,
                config.operation_mode,
                config.crossover_freq,
            ),
            Err(_) => {
                ctx.request_repaint();
                return;
            }
        };

        CentralPanel::default().show(ctx, |ui| {
            ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(10.0, 10.0);

                    ui.heading("Sound Lock 设置");
                    ui.separator();

                    ui.horizontal(|ui| {
                        ui.label("状态：");
                        let (color, text) = if is_limiting {
                            (Color32::from_rgb(0, 200, 0), "运行中")
                        } else {
                            (Color32::GRAY, "未运行")
                        };
                        ui.colored_label(color, text);
                    });

                    ui.separator();

                    ui.horizontal(|ui| {
                        ui.radio_value(&mut operation_mode, OperationMode::WindowsAPI, "Windows API 模式");
                        ui.radio_value(&mut operation_mode, OperationMode::Cable, "虚拟声卡模式");
                    });

                    ui.separator();

                    if operation_mode == OperationMode::Cable {
                        ui.group(|ui| {
                            ui.label("音频设备");

                            ui.horizontal(|ui| {
                                ui.label("输入：");
                                let selected_text = if self.selected_input_idx == usize::MAX {
                                    "未选择".to_owned()
                                } else {
                                    self.input_devices
                                        .get(self.selected_input_idx)
                                        .and_then(|(d, _)| d.description().ok())
                                        .map(|d| d.name().to_string())
                                        .unwrap_or_else(|| "未知设备".to_string())
                                };

                                egui::ComboBox::from_id_salt("input_device_combo")
                                    .selected_text(&selected_text)
                                    .show_ui(ui, |ui| {
                                        for (idx, (d, _)) in self.input_devices.iter().enumerate() {
                                            let name = d
                                                .description()
                                                .ok()
                                                .map(|desc| desc.name().to_string())
                                                .unwrap_or_else(|| format!("设备 {}", idx));
                                            ui.selectable_value(
                                                &mut self.selected_input_idx,
                                                idx,
                                                name,
                                            );
                                        }
                                    });
                            });

                            ui.horizontal(|ui| {
                                ui.label("输出：");
                                let selected_text = if self.selected_output_idx == usize::MAX {
                                    "未选择".to_owned()
                                } else {
                                    self.output_devices
                                        .get(self.selected_output_idx)
                                        .and_then(|(d, _)| d.description().ok())
                                        .map(|d| d.name().to_string())
                                        .unwrap_or_else(|| "未知设备".to_string())
                                };

                                egui::ComboBox::from_id_salt("output_device_combo")
                                    .selected_text(&selected_text)
                                    .show_ui(ui, |ui| {
                                        for (idx, (d, _)) in self.output_devices.iter().enumerate() {
                                            let name = d
                                                .description()
                                                .ok()
                                                .map(|desc| desc.name().to_string())
                                                .unwrap_or_else(|| format!("设备 {}", idx));
                                            ui.selectable_value(
                                                &mut self.selected_output_idx,
                                                idx,
                                                name,
                                            );
                                        }
                                    });
                            });
                        });

                        // 分频点滑块（仅 Cable 模式显示）
                        ui.separator();
                        ui.label("分频点（低音保留频率）：");
                        ui.horizontal(|ui| {
                            ui.add(
                                Slider::new(&mut crossover_freq, 100.0..=900.0)
                                    .text("Hz")
                                    .max_decimals(0),
                            );
                            ui.label(format!("{:.0} Hz", crossover_freq));
                        });
                    } else {
                        ui.label("选择要限制音量的应用：");
                        ui.horizontal(|ui| {
                            if ui.button("刷新列表").clicked() {
                                self.refresh_sessions();
                            }
                        });

                        ui.group(|ui| {
                            ui.set_min_height(150.0);
                            ScrollArea::vertical().show(ui, |ui| {
                                if self.sessions.is_empty() {
                                    ui.label("没有找到正在播放音频的应用");
                                }
                                for session in &self.sessions {
                                    let is_selected = selected_pid == Some(session.pid);
                                    ui.horizontal(|ui| {
                                        let response =
                                            ui.radio_value(&mut selected_pid, Some(session.pid), "");
                                        ui.label(format!("{} (PID: {})", session.name, session.pid));
                                        if is_selected {
                                            response.highlight();
                                        }
                                    });
                                }
                            });
                        });
                    }

                    ui.separator();

                    ui.label("最大音量阈值：");
                    ui.horizontal(|ui| {
                        ui.add(
                            Slider::new(&mut threshold_db, -60.0..=0.0)
                                .text("dB")
                                .max_decimals(1),
                        );
                        ui.label(format!("{:.1} dB", threshold_db));
                    });

                    ui.separator();

                    ui.label("触发时间：");
                    ui.horizontal(|ui| {
                        ui.add(
                            Slider::new(&mut attack_ms, 1..=300)
                                .text("毫秒")
                                .max_decimals(0),
                        );
                        ui.label(format!("{} ms", attack_ms));
                    });

                    ui.label("释放时间：");
                    ui.horizontal(|ui| {
                        ui.add(
                            Slider::new(&mut release_ms, 1..=300)
                                .text("毫秒")
                                .max_decimals(0),
                        );
                        ui.label(format!("{} ms", release_ms));
                    });

                    if operation_mode == OperationMode::WindowsAPI {
                        ui.label("音量扫描间隔：");
                        ui.horizontal(|ui| {
                            ui.add(
                                Slider::new(&mut scan_interval_ms, 1..=300)
                                    .text("毫秒")
                                    .max_decimals(0),
                            );
                            ui.label(format!("{} ms", scan_interval_ms));
                        });

                        ui.label("音量变化阈值：");
                        ui.horizontal(|ui| {
                            ui.add(
                                Slider::new(&mut volume_change_percentage_threshold, 0.01..=0.10)
                                    .max_decimals(3),
                            );
                            ui.label(format!("{:.3}", volume_change_percentage_threshold));
                        });
                    }

                    ui.separator();

                    ui.horizontal(|ui| {
                        let btn_size = Vec2::new(140.0, 40.0);

                        if !is_limiting {
                            let start_btn = ui.add_sized(
                                btn_size,
                                Button::new("启动限制").fill(Color32::from_rgb(0, 150, 0)),
                            );
                            if start_btn.clicked() {
                                if selected_pid.is_some() || operation_mode == OperationMode::Cable {
                                    self.start_limiting();
                                } else {
                                    ctx.send_viewport_cmd(egui::ViewportCommand::Title(
                                        "请先选择一个应用".to_string(),
                                    ));
                                }
                            }
                        } else {
                            let stop_btn = ui.add_sized(
                                btn_size,
                                Button::new("停止").fill(Color32::from_rgb(200, 0, 0)),
                            );
                            if stop_btn.clicked() {
                                self.stop_limiting();
                            }
                        }

                        if ui.add_sized(btn_size, Button::new("保存设置")).clicked() {
                            match self.config.try_lock() {
                                Ok(cfg) => self.save_config(&*cfg),
                                Err(_) => log::error!("Cannot save config: lock poisoned"),
                            }
                        }
                    });

                    ui.separator();
                    ui.add_space(15.0);

                    ui.with_layout(Layout::bottom_up(Align::Center), |ui| {
                        ui.horizontal(|ui| {
                            ui.hyperlink_to("GitHub", "https://github.com/winsrewu/soundlock-rs");
                        });
                    });
                });
        });

        // 写回配置
        let mut need_stop = false;
        {
            let mut config = match self.config.try_lock() {
                Ok(c) => c,
                Err(_) => {
                    ctx.request_repaint();
                    return;
                }
            };

            let mut changed = false;

            if threshold_db != config.threshold_db {
                config.threshold_db = threshold_db;
                changed = true;
            }
            if selected_pid != config.target_pid {
                config.target_pid = selected_pid;
                changed = true;
            }
            if attack_ms != config.attack_ms {
                config.attack_ms = attack_ms;
                changed = true;
            }
            if release_ms != config.release_ms {
                config.release_ms = release_ms;
                changed = true;
            }
            if scan_interval_ms != config.scan_interval_ms {
                config.scan_interval_ms = scan_interval_ms;
                changed = true;
            }
            if volume_change_percentage_threshold != config.volume_change_percentage_threshold {
                config.volume_change_percentage_threshold = volume_change_percentage_threshold;
                changed = true;
            }
            if operation_mode != config.operation_mode {
                config.operation_mode = operation_mode;
                need_stop = true;
                changed = true;
            }
            if crossover_freq != config.crossover_freq {
                config.crossover_freq = crossover_freq;
                changed = true;
            }

            let new_input_id = self
                .input_devices
                .get(self.selected_input_idx)
                .map(|(_, id)| id.clone());
            if new_input_id != config.target_input_device_id {
                config.target_input_device_id = new_input_id;
                need_stop = true;
                changed = true;
            }

            let new_output_id = self
                .output_devices
                .get(self.selected_output_idx)
                .map(|(_, id)| id.clone());
            if new_output_id != config.target_output_device_id {
                config.target_output_device_id = new_output_id;
                need_stop = true;
                changed = true;
            }

            if changed {
                self.save_config(&*config);
            }
        }

        if need_stop {
            self.stop_limiting();
        }
    }
}

fn find_device_index(devices: &[(Device, String)], device_id: &str) -> Option<usize> {
    devices.iter().position(|(_, id)| id == device_id)
}
