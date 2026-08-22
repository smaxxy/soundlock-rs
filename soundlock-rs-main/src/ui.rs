use crate::config::{Config, RuntimeLimiterParams};
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
    runtime_params: Arc<RuntimeLimiterParams>,
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
    last_config_change: Instant,
    pending_save: bool,
    show_raw_recorder: bool,
}

impl SettingsWindow {
    pub fn new(
        state: Arc<Mutex<AppState>>,
        config: Arc<Mutex<Config>>,
        runtime_params: Arc<RuntimeLimiterParams>,
    ) -> Self {
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
            runtime_params,
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
            last_config_change: Instant::now(),
            pending_save: false,
            show_raw_recorder: false,
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
    if let Err(e) = config.save() {
        log::error!("Failed to save config: {}", e);
    }
}
    fn start_limiting(&mut self, ctx: &Context) {
        let (input_id, output_id) = {
            match self.config.try_lock() {
                Ok(cfg) => {
                    // 启动前发布完整参数，保证 Session 使用本帧最新配置。
                    self.runtime_params.publish_config(&cfg);

                    (
                        cfg.target_input_device_id.clone(),
                        cfg.target_output_device_id.clone(),
                    )
                }

                Err(std::sync::TryLockError::WouldBlock) => {
                    self.retry_start = true;
                    ctx.request_repaint_after(
                        std::time::Duration::from_secs(1),
                    );
                    return;
                }

                Err(_) => {
                    log::error!("Config lock poisoned");
                    self.retry_start = false;
                    return;
                }
            }
        };

        if input_id.is_none() || output_id.is_none() {
            log::warn!("Cannot start limiting: no audio devices selected");
            self.retry_start = false;
            return;
        }

        match self.state.try_lock() {
            Ok(mut state) => {
                // 防止重复启动。
                if state.is_limiting {
                    self.retry_start = false;
                    return;
                }

                state.is_limiting = true;
                self.retry_start = false;
            }

            Err(std::sync::TryLockError::WouldBlock) => {
                self.retry_start = true;
                ctx.request_repaint_after(
                    std::time::Duration::from_secs(1),
                );
                return;
            }

            Err(_) => {
                log::error!("State lock poisoned");
                self.retry_start = false;
                return;
            }
        }

        audio::start_limiter(
            Arc::clone(&self.state),
            Arc::clone(&self.config),
            Arc::clone(&self.runtime_params),
        );

        ctx.request_repaint();
    }

    fn stop_limiting(&mut self, ctx: &Context) {
        match self.state.try_lock() {
            Ok(mut state) => {
                state.is_limiting = false;
                self.retry_stop = false;
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                self.retry_stop = true;
                ctx.request_repaint_after(
                    std::time::Duration::from_secs(1),
                );
            }
            Err(e) => {
                log::error!("Failed to stop limiter: {:?}", e);
                self.retry_stop = false;
            }
        }
    }

    fn on_exit_save(&mut self) {
        if !self.pending_save {
            return;
        }

        let config = match self.config.try_lock() {
            Ok(cfg) => cfg.clone(),

            Err(std::sync::TryLockError::WouldBlock) => {
                // 退出时允许阻塞取锁，保证最后一次修改不会丢失。
                match self.config.lock() {
                    Ok(cfg) => cfg.clone(),

                    Err(poisoned) => {
                        log::warn!(
                            "Config mutex poisoned during exit save; using recovered value"
                        );
                        poisoned.into_inner().clone()
                    }
                }
            }

            Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                log::warn!(
                    "Config mutex poisoned during exit save; using recovered value"
                );
                poisoned.into_inner().clone()
            }
        };

        // 退出时同步保存，避免窗口释放后仍有配置等待写盘。
        if let Err(e) = config.save() {
            log::error!("Failed to save config on exit: {}", e);
        }

        self.pending_save = false;
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

        let now = Instant::now();
        if self.retry_start && now.duration_since(self.last_retry_start) > std::time::Duration::from_secs(1) {
            self.start_limiting(ctx);
            self.last_retry_start = now;
        }
        if self.retry_stop && now.duration_since(self.last_retry_stop) > std::time::Duration::from_secs(1) {
            self.stop_limiting(ctx);
            self.last_retry_stop = now;
        }

        let is_limiting = match self.state.try_lock() {
            Ok(state) => state.is_limiting,
            Err(_) => {
                ctx.request_repaint();
                return;
            }
        };

        let recorder_status = audio::recorder::status();
        let recorder_in_flight = recorder_status.requested
            || matches!(
                recorder_status.state,
                audio::recorder::RecordingState::Starting
                    | audio::recorder::RecordingState::Recording
                    | audio::recorder::RecordingState::Saving
            );

        if recorder_in_flight {
            self.show_raw_recorder = true;
        }

        if recorder_in_flight
            || (self.show_raw_recorder
                && is_limiting
                && !recorder_status.session_available
                && matches!(
                    recorder_status.state,
                    audio::recorder::RecordingState::Idle
                ))
        {
            ctx.request_repaint_after(
                std::time::Duration::from_millis(100),
            );
        }

        let (
            mut threshold_db,
            mut attack_ms,
            mut release_ms,
            mut pre_gain_db,
            mut peak_release_ms,
        ) = {
            match self.config.try_lock() {
                Ok(config) => (
                    config.threshold_db,
                    config.attack_ms,
                    config.release_ms,
                    config.pre_gain_db,
                    config.peak_release_ms,
                ),
                Err(_) => {
                    ctx.request_repaint_after(
                        std::time::Duration::from_millis(16),
                    );
                    return;
                }
            }
        };

        // 按钮动作延后至配置写回后，确保使用本帧最新值。
        let mut start_clicked = false;
        let mut stop_clicked = false;
        let mut save_clicked = false;

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

                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.label("音频设备");
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let can_refresh = self.last_refresh_click.elapsed() > std::time::Duration::from_secs(1);
                                if ui
                                    .add_enabled(
                                        can_refresh && !is_limiting,
                                        egui::Button::new("🔄 刷新设备"),
                                    )
                                    .clicked()
                                {
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
                            ui.add_enabled_ui(!is_limiting, |ui| {
                                egui::ComboBox::from_id_salt("input_device_combo")
                                    .selected_text(&selected_text)
                                    .show_ui(ui, |ui| {
                                        for (idx, (d, _)) in
                                            self.input_devices.iter().enumerate()
                                        {
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
                            ui.add_enabled_ui(!is_limiting, |ui| {
                                egui::ComboBox::from_id_salt("output_device_combo")
                                    .selected_text(&selected_text)
                                    .show_ui(ui, |ui| {
                                        for (idx, (d, _)) in
                                            self.output_devices.iter().enumerate()
                                        {
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
                    });

                    ui.separator();

                    ui.label("持续响度阈值（RMS）：");
                    ui.horizontal(|ui| {
                        ui.add(
                            Slider::new(&mut threshold_db, -60.0..=0.0)
                                .text("dB")
                                .max_decimals(1),
                        );
                        ui.label(format!("{:.1} dB", threshold_db));
                    });

                    ui.label("RMS 触发时间：");
                    ui.horizontal(|ui| {
                        ui.add(
                            Slider::new(&mut attack_ms, 1..=300)
                                .text("毫秒")
                                .max_decimals(0),
                        );
                        ui.label(format!("{} ms", attack_ms));
                    });

                    ui.label("RMS 释放时间：");
                    ui.horizontal(|ui| {
                        ui.add(
                            Slider::new(&mut release_ms, 1..=300)
                                .text("毫秒")
                                .max_decimals(0),
                        );
                        ui.label(format!("{} ms", release_ms));
                    });

                    ui.label("小声音增强（Pre-Gain）：");
                    ui.horizontal(|ui| {
                        ui.add(
                            Slider::new(&mut pre_gain_db, 0.0..=12.0)
                                .text("dB")
                                .max_decimals(1),
                        );
                        ui.label(format!("+{:.1} dB", pre_gain_db));
                    });

                    ui.label("Peak 释放时间：");
                    ui.horizontal(|ui| {
                        ui.add(
                            Slider::new(&mut peak_release_ms, 20..=150)
                                .text("毫秒")
                                .max_decimals(0),
                        );
                        ui.label(format!("{} ms", peak_release_ms));
                    });

                    ui.separator();

                    let mut crosshair_enabled =
                        crate::tray_state::CROSSHAIR_ENABLED.load(Ordering::SeqCst);

                    ui.horizontal(|ui| {
                        ui.label("屏幕准星：");

                        if ui.checkbox(&mut crosshair_enabled, "开启").changed() {
                            crate::tray_state::CROSSHAIR_ENABLED
                                .store(crosshair_enabled, Ordering::SeqCst);
                        }
                    });

                    if crosshair_enabled {
                        let current_color =
                            crate::tray_state::CROSSHAIR_COLOR.load(Ordering::SeqCst);

                        let r = ((current_color >> 16) & 0xFF) as u8;
                        let g = ((current_color >> 8) & 0xFF) as u8;
                        let b = (current_color & 0xFF) as u8;

                        let mut color = Color32::from_rgb(r, g, b);

                        ui.horizontal(|ui| {
                            ui.label("准星颜色：");

                            if ui.color_edit_button_srgba(&mut color).changed() {
                                let rgb = ((color.r() as u32) << 16)
                                    | ((color.g() as u32) << 8)
                                    | color.b() as u32;

                                crate::tray_state::CROSSHAIR_COLOR
                                    .store(rgb, Ordering::SeqCst);
                            }
                        });

                        let mut crosshair_size =
                            crate::tray_state::CROSSHAIR_SIZE.load(Ordering::SeqCst);

                        ui.horizontal(|ui| {
                            ui.label("准星大小：");

                            if ui
                                .add(
                                    Slider::new(&mut crosshair_size, 4..=60)
                                        .suffix(" px"),
                                )
                                .changed()
                            {
                                crate::tray_state::CROSSHAIR_SIZE
                                    .store(crosshair_size, Ordering::SeqCst);
                            }
                        });
                    }

                    ui.separator();

                    ui.horizontal(|ui| {
                        let btn_size = Vec2::new(140.0, 40.0);
                        if !is_limiting {
                            if ui
                                .add_sized(
                                    btn_size,
                                    Button::new("启动限制")
                                        .fill(Color32::from_rgb(0, 150, 0)),
                                )
                                .clicked()
                            {
                                start_clicked = true;
                            }
                        } else if ui
                            .add_sized(
                                btn_size,
                                Button::new("停止")
                                    .fill(Color32::from_rgb(200, 0, 0)),
                            )
                            .clicked()
                        {
                            stop_clicked = true;
                        }

                        if ui
                            .add_sized(btn_size, Button::new("保存设置"))
                            .clicked()
                        {
                            save_clicked = true;
                        }
                    });

                    if self.show_raw_recorder {
                        let recording_snapshot = audio::recorder::snapshot();
                        ui.group(|ui| {
                            ui.label("原始游戏声音录制（Limiter 处理前）");

                        let recorder_busy = recording_snapshot.requested
                            || matches!(
                                recording_snapshot.state,
                                audio::recorder::RecordingState::Starting
                                    | audio::recorder::RecordingState::Recording
                            );
                        let recorder_saving = matches!(
                            recording_snapshot.state,
                            audio::recorder::RecordingState::Saving
                        );

                        ui.horizontal(|ui| {
                            let record_button_size = Vec2::new(180.0, 40.0);

                            if recorder_busy {
                                if ui
                                    .add_sized(
                                        record_button_size,
                                        Button::new("停止并保存")
                                            .fill(Color32::from_rgb(190, 45, 45)),
                                    )
                                    .clicked()
                                {
                                    audio::recorder::request_stop();
                                    ctx.request_repaint_after(
                                        std::time::Duration::from_millis(16),
                                    );
                                }
                            } else if recorder_saving {
                                ui.add_enabled(
                                    false,
                                    Button::new("正在保存 WAV…")
                                        .min_size(record_button_size),
                                );
                            } else if ui
                                .add_enabled(
                                    is_limiting
                                        && recording_snapshot.session_available,
                                    Button::new("开始录制原始声音")
                                        .min_size(record_button_size)
                                        .fill(Color32::from_rgb(30, 120, 190)),
                                )
                                .clicked()
                            {
                                if let Err(message) =
                                    audio::recorder::request_start()
                                {
                                    log::warn!(
                                        "Cannot start raw recording: {}",
                                        message
                                    );
                                }
                                ctx.request_repaint_after(
                                    std::time::Duration::from_millis(16),
                                );
                            }

                            if ui.button("打开录音文件夹").clicked() {
                                let directory =
                                    audio::recorder::recordings_dir();
                                std::thread::spawn(move || {
                                    if let Err(error) =
                                        std::fs::create_dir_all(&directory)
                                    {
                                        log::error!(
                                            "Failed to create recording directory: {}",
                                            error
                                        );
                                    } else if let Err(error) =
                                        open::that(&directory)
                                    {
                                        log::error!(
                                            "Failed to open recording directory: {}",
                                            error
                                        );
                                    }
                                });
                            }
                        });

                        let elapsed =
                            recording_snapshot.elapsed_seconds.max(0.0) as u64;
                        let elapsed_text =
                            format!("{:02}:{:02}", elapsed / 60, elapsed % 60);

                        match recording_snapshot.state {
                            audio::recorder::RecordingState::Starting => {
                                ui.colored_label(
                                    Color32::YELLOW,
                                    "正在创建录音文件…",
                                );
                            }
                            audio::recorder::RecordingState::Recording => {
                                ui.colored_label(
                                    Color32::from_rgb(0, 200, 0),
                                    format!(
                                        "● 正在录制 {elapsed_text}（最长 {} 秒）",
                                        audio::recorder::MAX_RECORDING_SECONDS
                                    ),
                                );
                            }
                            audio::recorder::RecordingState::Saving => {
                                ui.label("正在排空录音缓冲并完成 WAV 文件…");
                            }
                            audio::recorder::RecordingState::Saved => {
                                if let Some(path) =
                                    recording_snapshot.last_path.as_ref()
                                {
                                    ui.colored_label(
                                        Color32::from_rgb(0, 180, 0),
                                        format!("已保存：{}", path.display()),
                                    );
                                }
                            }
                            audio::recorder::RecordingState::Error => {
                                ui.colored_label(
                                    Color32::RED,
                                    recording_snapshot
                                        .last_error
                                        .as_deref()
                                        .unwrap_or(
                                            "原始录音失败，请停止并重新启动限制后重试",
                                        ),
                                );
                            }
                            audio::recorder::RecordingState::Idle => {
                                if is_limiting
                                    && !recording_snapshot.session_available
                                {
                                    ui.label("音频正在准备，稍后即可录制。");
                                } else if !is_limiting {
                                    ui.label(
                                        "请先启动限制；录制时耳机仍播放安全的限幅后声音。",
                                    );
                                } else {
                                    ui.label(
                                        "录到的 WAV 不含 Pre-Gain、RMS、Peak、Lookahead 或 Ceiling 处理。",
                                    );
                                }
                            }
                        }

                            if recording_snapshot.dropped_frames != 0 {
                                ui.colored_label(
                                    Color32::RED,
                                    format!(
                                        "录音缓冲丢失 {} 帧，请重新录制这一段。",
                                        recording_snapshot.dropped_frames
                                    ),
                                );
                            }
                        });
                        ui.separator();
                    }

                    ui.add_space(15.0);
                    ui.with_layout(Layout::bottom_up(Align::Center), |ui| {
                        ui.horizontal(|ui| {
                            let recorder_button_text =
                                if recorder_in_flight { "●" } else { "···" };
                            let recorder_toggle = ui
                                .small_button(recorder_button_text)
                                .on_hover_text("原始录音工具");

                            if recorder_toggle.clicked() && !recorder_in_flight {
                                self.show_raw_recorder = !self.show_raw_recorder;
                            }

                            ui.hyperlink_to(
                                "GitHub",
                                "https://github.com/winsrewu/soundlock-rs",
                            );
                        });
                    });
                });
        });

        // 写回配置 + Runtime 参数即时发布

        let mut config_snapshot_for_save: Option<Config> = None;

        {
            let mut config = match self.config.try_lock() {
                Ok(c) => c,

                Err(std::sync::TryLockError::WouldBlock) => {
                    // 写回失败时延迟启动，避免使用旧设备或旧参数。
                    if start_clicked {
                        self.retry_start = true;
                    }

                    ctx.request_repaint_after(
                        std::time::Duration::from_millis(16),
                    );
                    return;
                }

                Err(_) => {
                    log::error!("Config lock poisoned");
                    return;
                }
            };

            let mut limiter_params_changed = false;
            let mut any_config_changed = false;

            // Limiter DSP 参数

            if threshold_db != config.threshold_db {
                config.threshold_db = threshold_db;
                limiter_params_changed = true;
                any_config_changed = true;
            }

            if attack_ms != config.attack_ms {
                config.attack_ms = attack_ms;
                limiter_params_changed = true;
                any_config_changed = true;
            }

            if release_ms != config.release_ms {
                config.release_ms = release_ms;
                limiter_params_changed = true;
                any_config_changed = true;
            }

            if (pre_gain_db - config.pre_gain_db).abs() > f32::EPSILON {
                config.pre_gain_db = pre_gain_db;
                limiter_params_changed = true;
                any_config_changed = true;
            }

            if peak_release_ms != config.peak_release_ms {
                config.peak_release_ms = peak_release_ms;
                limiter_params_changed = true;
                any_config_changed = true;
            }
// 设备列表异步加载完成前，不得用无效选择覆盖 Config 中已有的设备 ID。

if let Some((_, input_id)) =
    self.input_devices.get(self.selected_input_idx)
{
    if config.target_input_device_id.as_ref()
        != Some(input_id)
    {
        config.target_input_device_id =
            Some(input_id.clone());

        any_config_changed = true;
    }
}

if let Some((_, output_id)) =
    self.output_devices.get(self.selected_output_idx)
{
    if config.target_output_device_id.as_ref()
        != Some(output_id)
    {
        config.target_output_device_id =
            Some(output_id.clone());

        any_config_changed = true;
    }
}
            // DSP 参数即时发布
            // 仅参数变化时 bump version，避免回调无意义地重算系数。
            if limiter_params_changed {
                self.runtime_params.publish_config(&config);
            }

            // 保存节流
            // trailing debounce：停止修改 500ms 后保存一次。
            if any_config_changed {
                self.pending_save = true;
                self.last_config_change = Instant::now();

                // 安排未来 repaint，保证无输入事件时 debounce 仍会触发。
                ctx.request_repaint_after(
                    std::time::Duration::from_millis(500),
                );
            }

            let debounce_elapsed = self
                .last_config_change
                .elapsed()
                > std::time::Duration::from_millis(500);

            if save_clicked {
                // 手动保存使用本帧最新 Config。
                config_snapshot_for_save = Some(config.clone());
                self.pending_save = false;
            } else if self.pending_save && debounce_elapsed {
                config_snapshot_for_save = Some(config.clone());
                self.pending_save = false;
            }
        }

        // 使用 Config clone 保存；此时已经释放 UI 的 Config Mutex。
        // 保存只影响 UI 线程，不会进入实时 Audio Callback。
        if let Some(config) = config_snapshot_for_save {
            Self::save_config(config);
        }

        // 执行本帧按钮动作
        // 必须在 Config 写回 / Runtime publish 之后。
        if start_clicked {
            self.start_limiting(ctx);
        }

        if stop_clicked {
            self.stop_limiting(ctx);
            ctx.request_repaint();
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.on_exit_save();
    }
}

fn find_device_index(devices: &[(Device, String)], device_id: &str) -> Option<usize> {
    devices.iter().position(|(_, id)| id == device_id)
}
