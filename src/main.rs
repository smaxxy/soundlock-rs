#![windows_subsystem = "windows"]

mod audio;
mod config;
mod ui;

use crate::config::Config;
use crate::ui::SettingsWindow;
use eframe::egui;
use egui::IconData;
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct AppState {
    pub is_limiting: bool,
}

fn main() -> Result<(), ()> {
    env_logger::init();

    let _instance = single_instance::SingleInstance::new("SoundLockRustInstance").unwrap();

    let icon_data = image::load_from_memory(include_bytes!("../assets/icon.png"))
        .unwrap()
        .to_rgba8()
        .to_vec();

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([400.0, 550.0])
            .with_min_inner_size([300.0, 400.0])
            .with_icon(Arc::new(IconData {
                rgba: icon_data,
                width: 256,
                height: 256,
            })),
        ..Default::default()
    };

    let app_state = Arc::new(Mutex::new(AppState::default()));
    let config = Config::load().unwrap();

    eframe::run_native(
        "Sound Lock Rust",
        native_options,
        Box::new(|_| {
            Ok(Box::new(SettingsWindow::new(
                Arc::clone(&app_state),
                Arc::clone(&config),
            )))
        }),
    )
    .unwrap();

    Ok(())
}
