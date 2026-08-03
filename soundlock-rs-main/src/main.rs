#![windows_subsystem = "windows"]

mod audio;
mod config;
mod setup;
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

/// 弹出 Windows 消息框，返回 true 表示用户点击了“是”
fn message_box_yes_no(title: &str, text: &str) -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONQUESTION, MB_YESNO, IDYES,
    };

    let title_wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    let text_wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();

    unsafe {
        let ret = MessageBoxW(
            None,
            PCWSTR::from_raw(text_wide.as_ptr()),
            PCWSTR::from_raw(title_wide.as_ptr()),
            MB_YESNO | MB_ICONQUESTION,
        );
        ret == IDYES
    }
}

fn main() -> Result<(), ()> {
    env_logger::init();

    // ---------- 自动检测并安装 VB-Cable ----------
    if !setup::is_vbcable_installed() {
        let user_wants_install = message_box_yes_no(
            "虚拟声卡未安装",
            "Sound Lock 需要虚拟声卡 VB-Cable 才能工作。\n\n是否立即安装？（需要管理员权限）",
        );

        if user_wants_install {
            match setup::install_vbcable() {
                Ok(()) => {
                    // 设置默认播放设备（音频服务已在安装时重启，无需再次重启）
                    if let Err(e) = setup::set_default_playback_device("CABLE Input") {
                        log::error!("设置默认播放设备失败: {}", e);
                    }
                }
                Err(e) => {
                    log::error!("VB-Cable 安装失败: {}", e);
                }
            }
        }
    }
    // ---------------------------------------------

    let instance = single_instance::SingleInstance::new("SoundLockRustInstance").unwrap();

    if !instance.is_single() {
        return Ok(());
    }

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