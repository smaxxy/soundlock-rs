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
    input_devices: Vec<Device>,
    output_devices: Vec<Device>,
    selected_input_idx: usize,
    selected_output_idx: usize,
}

impl SettingsWindow {
    pub fn new(state: Arc<Mutex<AppState>>, config: Arc<Mutex<Config>>) -> Self {
        let sessions = Self::enumerate_sessions();

        let (input_devices, output_devices) = Self::get_devices();

        let selected_input_idx;
        let selected_output_idx;
        {
            let config = config.lock().unwrap();
            selected_input_idx = config
                .target_input_device_id
                .as_ref()
                .and_then(|id| find_device_index(&input_devices, id))
                .unwrap_or(usize::MAX);
            selected_output_idx = config
                .target_output_device_id
                .as_ref()
                .and_then(|id| find_device_index(&output_devices, id))
                .unwrap_or(usize::MAX);
        }

        Self {
            state,
            config,
            sessions,
            last_refresh: std::time::Instant::now(),
            input_devices,
            output_devices,
            selected_input_idx,
            selected_output_idx,
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

    fn get_devices() -> (Vec<Device>, Vec<Device>) {
        let host = cpal::default_host();

        let input_devices: Vec<Device> = host
            .input_devices()
            .unwrap()
            .filter(|d| {
                if !(d.description().is_ok() && d.id().is_ok()) {
                    log::warn!("Failed to get device description or ID in input_devices");
                    return false;
                }
                true
            })
            .collect();

        let output_devices: Vec<Device> = host
            .output_devices()
            .unwrap()
            .filter(|d| {
                if !(d.description().is_ok() && d.id().is_ok()) {
                    log::warn!("Failed to get device description or ID in output_devices");
                    return false;
                }
                true
            })
            .collect();

        (input_devices, output_devices)
    }

    fn refresh_sessions(&mut self) {
        self.sessions = Self::enumerate_sessions();
        self.last_refresh = std::time::Instant::now();
        log::debug!("Sessions refreshed");
    }

    fn save_config(&self, config: &Config) {
        match config.save() {
            Ok(_) => log::info!("Config saved"),
            Err(e) => log::error!("Failed to save config: {}", e),
        }
    }

    fn start_limiting(&self) {
        let config = self.config.lock().unwrap();

        self.state.lock().unwrap().is_limiting = true;

        self.save_config(&config);

        log::info!("Starting limiter");

        if config.operation_mode == OperationMode::WindowsAPI && config.target_pid.is_none() {
            log::warn!("Cannot start limiting: no application selected");
            return;
        }

        if config.operation_mode == OperationMode::Cable
            && (config.target_input_device_id.is_none() || config.target_output_device_id.is_none())
        {
            log::warn!("Cannot start limiting: no audio devices selected");
            return;
        }

        audio::start_limiter(Arc::clone(&self.state), Arc::clone(&self.config));
    }

    fn stop_limiting(&self) {
        log::info!("Stopping limiter");

        self.state.lock().unwrap().is_limiting = false;
    }
}

impl eframe::App for SettingsWindow {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        let is_limiting = self.state.lock().unwrap().is_limiting;

        let mut threshold_db;
        let mut selected_pid;
        let mut attack_ms;
        let mut release_ms;
        let mut scan_interval_ms;
        let mut operation_mode;
        let mut volume_change_percentage_threshold;
        {
            let config = self.config.lock().unwrap();
            threshold_db = config.threshold_db;
            selected_pid = config.target_pid;
            attack_ms = config.attack_ms;
            release_ms = config.release_ms;
            scan_interval_ms = config.scan_interval_ms;
            volume_change_percentage_threshold = config.volume_change_percentage_threshold;
            operation_mode = config.operation_mode;
        }

        CentralPanel::default().show(ctx, |ui| {
            ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(10.0, 10.0);

                    ui.heading("Sound Lock Settings");
                    ui.separator();

                    ui.horizontal(|ui| {
                        ui.label("Status:");

                        let (color, text) = if is_limiting {
                            (Color32::from_rgb(0, 200, 0), "Active")
                        } else {
                            (Color32::GRAY, "Inactive")
                        };

                        ui.colored_label(color, text);
                    });

                    ui.separator();

                    ui.horizontal(|ui| {
                        ui.radio_value(
                            &mut operation_mode,
                            OperationMode::WindowsAPI,
                            "Windows API",
                        );
                        ui.radio_value(&mut operation_mode, OperationMode::Cable, "Cable");
                    });

                    ui.separator();

                    if operation_mode == OperationMode::Cable {
                        ui.group(|ui| {
                            ui.label("Audio Devices");

                            ui.horizontal(|ui| {
                                ui.label("Input:");

                                let selected_text = if self.selected_input_idx == usize::MAX {
                                    "None"
                                } else {
                                    &self.input_devices[self.selected_input_idx]
                                        .description()
                                        .unwrap()
                                        .to_string()
                                };

                                egui::ComboBox::from_id_salt("input_device_combo")
                                    .selected_text(selected_text)
                                    .show_ui(ui, |ui| {
                                        for (idx, name) in self.input_devices.iter().enumerate() {
                                            ui.selectable_value(
                                                &mut self.selected_input_idx,
                                                idx,
                                                name.description().unwrap().name(),
                                            );
                                        }
                                    });
                            });

                            ui.horizontal(|ui| {
                                ui.label("Output:");

                                let selected_text = if self.selected_output_idx == usize::MAX {
                                    "None"
                                } else {
                                    &self.output_devices[self.selected_output_idx]
                                        .description()
                                        .unwrap()
                                        .to_string()
                                };

                                egui::ComboBox::from_id_salt("output_device_combo")
                                    .selected_text(selected_text)
                                    .show_ui(ui, |ui| {
                                        for (idx, name) in self.output_devices.iter().enumerate() {
                                            ui.selectable_value(
                                                &mut self.selected_output_idx,
                                                idx,
                                                name.description().unwrap().name(),
                                            );
                                        }
                                    });
                            });
                        });
                    } else {
                        ui.label("Select Application to Limit:");

                        ui.horizontal(|ui| {
                            if ui.button("Refresh List").clicked() {
                                self.refresh_sessions();
                            }
                        });

                        ui.group(|ui| {
                            ui.set_min_height(150.0);

                            ScrollArea::vertical().show(ui, |ui| {
                                if self.sessions.is_empty() {
                                    ui.label(
                                    "No audio sessions found. Make sure an app is playing audio.",
                                );
                                }

                                for session in &self.sessions {
                                    let is_selected = selected_pid == Some(session.pid);

                                    ui.horizontal(|ui| {
                                        let response = ui.radio_value(
                                            &mut selected_pid,
                                            Some(session.pid),
                                            "",
                                        );

                                        ui.label(format!(
                                            "{} (PID: {})",
                                            session.name, session.pid
                                        ));

                                        if is_selected {
                                            response.highlight();
                                        }
                                    });
                                }
                            });
                        });
                    }

                    ui.separator();

                    ui.label("Maximum Loudness Threshold:");

                    ui.horizontal(|ui| {
                        ui.add(
                            Slider::new(&mut threshold_db, -60.0..=0.0)
                                .text("dB")
                                .max_decimals(1),
                        );

                        ui.label(format!("{:.1} dB", threshold_db));
                    });

                    ui.separator();

                    ui.label("Attack Time:");

                    ui.horizontal(|ui| {
                        ui.add(
                            Slider::new(&mut attack_ms, 1..=300)
                                .text("ms")
                                .max_decimals(0),
                        );

                        ui.label(format!("{:} ms", attack_ms));
                    });

                    ui.label("Release Time:");

                    ui.horizontal(|ui| {
                        ui.add(
                            Slider::new(&mut release_ms, 1..=300)
                                .text("ms")
                                .max_decimals(0),
                        );

                        ui.label(format!("{:} ms", release_ms));
                    });

                    if operation_mode == OperationMode::WindowsAPI {
                        ui.label("Volume Scan Interval:");

                        ui.horizontal(|ui| {
                            ui.add(
                                Slider::new(&mut scan_interval_ms, 1..=300)
                                    .text("ms")
                                    .max_decimals(0),
                            );

                            ui.label(format!("{:} ms", scan_interval_ms));
                        });

                        ui.label("Volume Change Percentage Threshold:");

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
                                Button::new("Start Limiting").fill(Color32::from_rgb(0, 150, 0)),
                            );

                            if start_btn.clicked() {
                                if selected_pid.is_some() {
                                    self.start_limiting();
                                } else {
                                    ctx.send_viewport_cmd(egui::ViewportCommand::Title(
                                        "Please select an application first".to_string(),
                                    ));
                                }
                            }
                        } else {
                            let stop_btn = ui.add_sized(
                                btn_size,
                                Button::new("Stop").fill(Color32::from_rgb(200, 0, 0)),
                            );

                            if stop_btn.clicked() {
                                self.stop_limiting();
                            }
                        }

                        if ui.add_sized(btn_size, Button::new("Save Config")).clicked() {
                            self.save_config(&self.config.lock().unwrap());
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

        {
            let mut config = self.config.lock().unwrap();
            if threshold_db != config.threshold_db {
                config.threshold_db = threshold_db;
            }
            if selected_pid != config.target_pid {
                config.target_pid = selected_pid;
            }
            if attack_ms != config.attack_ms {
                config.attack_ms = attack_ms;
            }
            if release_ms != config.release_ms {
                config.release_ms = release_ms;
            }
            if scan_interval_ms != config.scan_interval_ms {
                config.scan_interval_ms = scan_interval_ms;
            }
            if volume_change_percentage_threshold != config.volume_change_percentage_threshold {
                config.volume_change_percentage_threshold = volume_change_percentage_threshold;
            }
            if operation_mode != config.operation_mode {
                config.operation_mode = operation_mode;
                self.stop_limiting();
            }
            if let Some(id) = self
                .input_devices
                .get(self.selected_input_idx)
                .map(|d| d.id().unwrap().to_string())
                .filter(|id| config.target_input_device_id.as_ref() != Some(id))
            {
                config.target_input_device_id = Some(id);
                self.stop_limiting();
            }
            if let Some(id) = self
                .output_devices
                .get(self.selected_output_idx)
                .map(|d| d.id().unwrap().to_string())
                .filter(|id| config.target_output_device_id.as_ref() != Some(id))
            {
                config.target_output_device_id = Some(id);
                self.stop_limiting();
            }
        }
    }
}

fn find_device_index(devices: &[Device], device_id: &str) -> Option<usize> {
    devices
        .iter()
        .position(|d| d.id().unwrap().to_string() == device_id)
}
