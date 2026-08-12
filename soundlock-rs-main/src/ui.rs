use crate::config::{Config, LimiterMode, OperationMode};
use crate::{AppState, audio};
use cpal::Device;
use cpal::traits::{DeviceTrait, HostTrait};
use egui::*;
use std::sync::{Arc, Mutex};

pub struct SettingsWindow {
    state: Arc<Mutex<AppState>>,
    config: Arc<Mutex<Config>>,
    input_devices: Vec<(Device, String)>,
    output_devices: Vec<(Device, String)>,
    selected_input_idx: usize,
    selected_output_idx: usize,
    retry_start: bool,
    retry_stop: bool,
    fonts_loaded: bool,
    last_retry_start: std::time::Instant,
    last_retry_stop: std::time::Instant,
    last_refresh_click: std::time::Instant,

    pending_devices: Arc<Mutex<Option<(Vec<(Device, String)>, Vec<(Device, String)>)>>>,

    last_save_time: std::time::Instant,
    pending_save: bool,
}

impl SettingsWindow {
    pub fn new(state: Arc<Mutex<AppState>>, config: Arc<Mutex<Config>>) -> Self {
        let pending_devices = Arc::new(Mutex::new(None));
        let pending_clone = Arc::clone(&pending_devices);
        std::thread::spawn(move || {
            let devices = Self::get_devices();
            if let Ok(mut guard) = pending_clone.lock() {
                *guard = Some(devices);
            }
        });

        Self {
            state,
            config,
            input_devices: Vec::new(),
            output_devices: Vec::new(),
            selected_input_idx: usize::MAX,
            selected_output_idx: usize::MAX,
            retry_start: false,
            retry_stop: false,
            fonts_loaded: false,
            last_retry_start: std::time::Instant::now(),
            last_retry_stop: std::time::Instant::now(),
            last_refresh_click: std::time::Instant::now(),
            pending_devices,
            last_save_time: std::time::Instant::now(),
            pending_save: false,
        }
    }

    fn get_devices() -> (Vec<(Device, String)>, Vec<(Device, String)>) {
        let host = cpal::default_host();
        let input_devices = host
            .input_devices()
            .map(|devices| {
                devices
                    .filter_map(|d| d.id().ok().map(|id| (d, id.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        let output_devices = host
            .output_devices()
            .map(|devices| {
                devices
                    .filter_map(|d| d.id().ok().map(|id| (d, id.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        (input_devices, output_devices)
    }


    fn update_selected_indices(&mut self) {
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
    }

    fn save_config(config: Config) {
        std::thread::spawn(move || {
            if let Err(e) = config.save() {
                log::error!("Failed to save config: {}", e);
            }
        });
    }

    fn start_limiting(&mut self, ctx: &Context) {
        let (mode, _pid, input_id, output_id) = {
            match self.config.try_lock() {
                Ok(cfg) => (
                    cfg.operation_mode,
                    cfg.target_pid,
                    cfg.target_input_device_id.clone(),
                    cfg.target_output_device_id.clone(),
                ),
                Err(_) => {
                    log::error!("Config lock poisoned");
                    self.retry_start = true;
                    return;
                }
            }
        };

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
                self.retry_start = true;
                return;
            }
            Err(_) => {
                log::error!("State lock poisoned");
                return;
            }
        }

        let state = Arc::clone(&self.state);
        let config = Arc::clone(&self.config);
        let ctx = ctx.clone();

        std::thread::spawn(move || {
            audio::start_limiter(state, config);
            ctx.request_repaint();
        });
    }

    fn stop_limiting(&mut self) {
        match self.state.try_lock() {
            Ok(mut state) => {
                state.is_limiting = false;
                self.retry_stop = false;
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                self.retry_stop = true;
            }
            Err(e) => {
                log::error!("Failed to stop limiter: {:?}", e);
                self.retry_stop = false;
            }
        }
    }

    fn on_exit_save(&mut self) {
        if self.pending_save {
            if let Ok(cfg) = self.config.try_lock() {
                Self::save_config(cfg.clone());
            }
            self.pending_save = false;
        }
    }
}

impl eframe::App for SettingsWindow {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        let devices_updated = {
            if let Ok(mut pending) = self.pending_devices.try_lock() {
                if let Some((input, output)) = pending.take() {
                    self.input_devices = input;
                    self.output_devices = output;
                    true
                } else {
                    false
                }
            } else {
                false
            }
        };
        if devices_updated {
            self.update_selected_indices();
            ctx.request_repaint();
        }

        if !self.fonts_loaded {
            let mut fonts = egui::FontDefinitions::default();
            if let Ok(font_data) = std::fs::read("C:\\Windows\\Fonts\\msyh.ttc") {
                fonts
                    .font_data
                    .insert("MicrosoftYaHei".to_owned(), egui::FontData::from_owned(font_data).into());
                fonts
                    .families
                    .entry(egui::FontFamily::Proportional)
                    .or_default()
                    .insert(0, "MicrosoftYaHei".to_owned());
                ctx.set_fonts(fonts);
            } else if let Ok(font_data) = std::fs::read("C:\\Windows\\Fonts\\simsun.ttc") {
                fonts
                    .font_data
                    .insert("SimSun".to_owned(), egui::FontData::from_owned(font_data).into());
                fonts
                    .families
                    .entry(egui::FontFamily::Proportional)
                    .or_default()
                    .insert(0, "SimSun".to_owned());
                ctx.set_fonts(fonts);
            } else {
                log::warn!("未找到中文字体");
            }
            self.fonts_loaded = true;
        }

        let now = std::time::Instant::now();
        if self.retry_start && now.duration_since(self.last_retry_start) > std::time::Duration::from_secs(1) {
            self.start_limiting(ctx);
            self.last_retry_start = now;
        }
        if self.retry_stop && now.duration_since(self.last_retry_stop) > std::time::Duration::from_secs(1) {
            self.stop_limiting();
            self.last_retry_stop = now;
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
            mut _scan_interval_ms,
            mut _volume_change_percentage_threshold,
            mut operation_mode,
            mut crossover_freq,
            mut limiter_mode,
            mut crest_strong,
            mut crest_mild,
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
                config.limiter_mode,
                config.crest_strong,
                config.crest_mild,
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
                        ui.radio_value(&mut operation_mode, OperationMode::Cable, "虚拟声卡模式");
                    });

                    ui.separator();

                    if operation_mode == OperationMode::Cable {
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                ui.label("音频设备");
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    let can_refresh = self.last_refresh_click.elapsed() > std::time::Duration::from_secs(1);
                                    if ui.add_enabled(can_refresh, egui::Button::new("🔄 刷新设备")).clicked() {
                                        self.last_refresh_click = std::time::Instant::now();
                                        let pending = Arc::clone(&self.pending_devices);
                                        let ctx = ctx.clone();
                                        std::thread::spawn(move || {
                                            let devices = SettingsWindow::get_devices();
                                            match pending.lock() {
                                                Ok(mut guard) => *guard = Some(devices),
                                                Err(e) => log::error!("刷新设备失败，锁异常: {}", e),
                                            }
                                            ctx.request_repaint();
                                        });
                                    }
                                });
                            });

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

                        ui.horizontal(|ui| {
                            ui.radio_value(&mut limiter_mode, LimiterMode::Fullband, "全频降");
                            ui.radio_value(&mut limiter_mode, LimiterMode::Multiband, "分块降");
                            // ui.radio_value(&mut limiter_mode, LimiterMode::Adaptive, "智能自适应");
                        });

                        if limiter_mode == LimiterMode::Multiband {
                            ui.label("分频点（低音保留频率）：");
                            ui.add(Slider::new(&mut crossover_freq, 0.0..=2000.0).text("Hz"));
                        } 
                        // else if limiter_mode == LimiterMode::Adaptive {
                        //     ui.label("强压缩阈值（枪声识别灵敏度）：");
                        //     ui.add(Slider::new(&mut crest_strong, 0.0..=10.0).text("倍"));
                        //     ui.label("轻微压缩阈值：");
                        //     ui.add(Slider::new(&mut crest_mild, 0.0..=10.0).text("倍"));
                        // }
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
                                    self.start_limiting(ctx);
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
                                Ok(cfg) => {
                                    Self::save_config(cfg.clone());
                                }
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

            macro_rules! update_field {
                ($field:ident, $val:expr, $stop:expr) => {
                    if $val != config.$field {
                        config.$field = $val;
                        changed = true;
                        need_stop |= $stop;
                    }
                };
            }

            update_field!(threshold_db, threshold_db, false);
            update_field!(target_pid, selected_pid, true);
            update_field!(attack_ms, attack_ms, false);
            update_field!(release_ms, release_ms, false);
            update_field!(scan_interval_ms, _scan_interval_ms, false);
            update_field!(volume_change_percentage_threshold, _volume_change_percentage_threshold, false);
            update_field!(crossover_freq, crossover_freq, false);
            update_field!(limiter_mode, limiter_mode, false);
            update_field!(crest_strong, crest_strong, false);
            update_field!(crest_mild, crest_mild, false);

            if operation_mode != config.operation_mode {
                config.operation_mode = operation_mode;
                changed = true;
                need_stop = true;
            }

            let new_input_id = self
                .input_devices
                .get(self.selected_input_idx)
                .map(|(_, id)| id.clone());
            if new_input_id != config.target_input_device_id {
                config.target_input_device_id = new_input_id;
                changed = true;
                need_stop = true;
            }
            let new_output_id = self
                .output_devices
                .get(self.selected_output_idx)
                .map(|(_, id)| id.clone());
            if new_output_id != config.target_output_device_id {
                config.target_output_device_id = new_output_id;
                changed = true;
                need_stop = true;
            }

            if changed {
                self.pending_save = true;
            }
            let now = std::time::Instant::now();
            if self.pending_save && now.duration_since(self.last_save_time) > std::time::Duration::from_millis(500) {
                let cfg_clone = config.clone();
                Self::save_config(cfg_clone);
                self.pending_save = false;
                self.last_save_time = now;
            }
        }

        if need_stop && is_limiting {
            self.stop_limiting();
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.on_exit_save();
    }
}

fn find_device_index(devices: &[(Device, String)], device_id: &str) -> Option<usize> {
    devices.iter().position(|(_, id)| id == device_id)
}