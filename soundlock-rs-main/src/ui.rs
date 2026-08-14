use crate::config::Config;
use crate::tray_state;
use crate::{AppState, audio};
use cpal::Device;
use cpal::traits::{DeviceTrait, HostTrait};
use egui::*;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Instant;

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
    last_retry_start: Instant,
    last_retry_stop: Instant,
    last_refresh_click: Instant,
    pending_devices: Arc<Mutex<Option<(Vec<(Device, String)>, Vec<(Device, String)>)>>>,
    last_save_time: Instant,
    pending_save: bool,
    // 托盘相关
    is_window_visible: bool,
    last_tray_check: Instant,
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
            last_retry_start: Instant::now(),
            last_retry_stop: Instant::now(),
            last_refresh_click: Instant::now(),
            pending_devices,
            last_save_time: Instant::now(),
            pending_save: false,
            is_window_visible: true,
            last_tray_check: Instant::now(),
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
        let (input_id, output_id) = {
            match self.config.try_lock() {
                Ok(cfg) => (
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
        if input_id.is_none() || output_id.is_none() {
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
        crate::diagnostics::ui_tick();
        // 托盘退出请求
        if tray_state::SHOULD_EXIT.load(Ordering::SeqCst) {
            self.on_exit_save();
            std::process::exit(0);
        }

       // 托盘恢复窗口请求
if tray_state::WINDOW_VISIBLE.swap(false, Ordering::SeqCst) {
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);

    self.is_window_visible = true;
    ctx.request_repaint();
}

        // 拦截关闭按钮，隐藏窗口
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            self.is_window_visible = false;
        }

        // 窗口隐藏时保持 UI 线程活跃（每秒唤醒一次）
        if !self.is_window_visible && self.last_tray_check.elapsed() > std::time::Duration::from_secs(1) {
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
            self.last_tray_check = Instant::now();
        }

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

        let now = Instant::now();
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

        let (mut threshold_db, mut attack_ms, mut release_ms) = {
            match self.config.try_lock() {
                Ok(config) => (config.threshold_db, config.attack_ms, config.release_ms),
                Err(_) => {
                    ctx.request_repaint();
                    return;
                }
            }
        };

        CentralPanel::default().show(ctx, |ui| {
            ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(10.0, 10.0);
                    ui.heading("Sound Lock 全频降音");
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

                    // 设备选择
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.label("音频设备");
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let can_refresh = self.last_refresh_click.elapsed() > std::time::Duration::from_secs(1);
                                if ui.add_enabled(can_refresh, egui::Button::new("🔄 刷新设备")).clicked() {
                                    self.last_refresh_click = Instant::now();
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
                                        let name = d.description().ok().map(|desc| desc.name().to_string()).unwrap_or_else(|| format!("设备 {}", idx));
                                        ui.selectable_value(&mut self.selected_input_idx, idx, name);
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
                                        let name = d.description().ok().map(|desc| desc.name().to_string()).unwrap_or_else(|| format!("设备 {}", idx));
                                        ui.selectable_value(&mut self.selected_output_idx, idx, name);
                                    }
                                });
                        });
                    });

                    ui.separator();
                    ui.label("最大音量阈值：");
                    ui.horizontal(|ui| {
                        ui.add(Slider::new(&mut threshold_db, -60.0..=0.0).text("dB").max_decimals(1));
                        ui.label(format!("{:.1} dB", threshold_db));
                    });
                    ui.label("触发时间：");
                    ui.horizontal(|ui| {
                        ui.add(Slider::new(&mut attack_ms, 1..=300).text("毫秒").max_decimals(0));
                        ui.label(format!("{} ms", attack_ms));
                    });
                    ui.label("释放时间：");
                    ui.horizontal(|ui| {
                        ui.add(Slider::new(&mut release_ms, 1..=300).text("毫秒").max_decimals(0));
                        ui.label(format!("{} ms", release_ms));
                    });
                    ui.separator();

                    ui.horizontal(|ui| {
                        let btn_size = Vec2::new(140.0, 40.0);
                        if !is_limiting {
                            if ui.add_sized(btn_size, Button::new("启动限制").fill(Color32::from_rgb(0, 150, 0))).clicked() {
                                self.start_limiting(ctx);
                            }
                        } else {
                            if ui.add_sized(btn_size, Button::new("停止").fill(Color32::from_rgb(200, 0, 0))).clicked() {
                                self.stop_limiting();
                            }
                        }
                        if ui.add_sized(btn_size, Button::new("保存设置")).clicked() {
                            match self.config.try_lock() {
                                Ok(cfg) => Self::save_config(cfg.clone()),
                                Err(_) => log::error!("Cannot save config: lock poisoned"),
                            }
                        }
                    });
                    ui.separator();
                    ui.add_space(15.0);
                    ui.with_layout(Layout::bottom_up(Align::Center), |ui| {
                        ui.hyperlink_to("GitHub", "https://github.com/winsrewu/soundlock-rs");
                    });
                });
        });

        // 写回配置
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
            if attack_ms != config.attack_ms {
                config.attack_ms = attack_ms;
                changed = true;
            }
            if release_ms != config.release_ms {
                config.release_ms = release_ms;
                changed = true;
            }
            let new_input_id = self.input_devices.get(self.selected_input_idx).map(|(_, id)| id.clone());
            if new_input_id != config.target_input_device_id {
                config.target_input_device_id = new_input_id;
                changed = true;
            }
            let new_output_id = self.output_devices.get(self.selected_output_idx).map(|(_, id)| id.clone());
            if new_output_id != config.target_output_device_id {
                config.target_output_device_id = new_output_id;
                changed = true;
            }
            if changed {
                self.pending_save = true;
            }
            let now = Instant::now();
            if self.pending_save && now.duration_since(self.last_save_time) > std::time::Duration::from_millis(500) {
                Self::save_config(config.clone());
                self.pending_save = false;
                self.last_save_time = now;
            }
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.on_exit_save();
    }
}

fn find_device_index(devices: &[(Device, String)], device_id: &str) -> Option<usize> {
    devices.iter().position(|(_, id)| id == device_id)
}