#![windows_subsystem = "windows"]

mod audio;
mod config;
mod setup;
mod tray_state;
mod ui;

use crate::config::Config;
use crate::ui::SettingsWindow;
use eframe::egui;
use egui::IconData;
use std::sync::{Arc, Mutex};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::Win32::UI::Shell::*;

#[derive(Default)]
pub struct AppState {
    pub is_limiting: bool,
}

fn message_box_yes_no(title: &str, text: &str) -> bool {
    let title_wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    let text_wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let ret = MessageBoxW(
            None,
            PCWSTR::from_raw(text_wide.as_ptr()),
            PCWSTR::from_raw(title_wide.as_ptr()),
            MB_ICONQUESTION | MB_YESNO,
        );
        ret == IDYES
    }
}

const WM_TRAYICON: u32 = WM_APP;
const IDM_SHOW: usize = 1001;
const IDM_EXIT: usize = 1002;

unsafe extern "system" fn tray_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    use std::sync::atomic::Ordering;

    if msg == WM_TRAYICON {
        if lparam.0 as u32 == WM_LBUTTONUP {
            tray_state::WINDOW_VISIBLE.store(true, Ordering::SeqCst);
        } else if lparam.0 as u32 == WM_RBUTTONUP {
            let mut cursor_pos = Default::default();
            GetCursorPos(&mut cursor_pos);
            SetForegroundWindow(hwnd);
            let menu = CreatePopupMenu().unwrap();
            AppendMenuW(menu, MF_STRING, IDM_SHOW, w!("显示窗口")).ok();
            AppendMenuW(menu, MF_STRING, IDM_EXIT, w!("退出")).ok();
            TrackPopupMenu(
                menu,
                TPM_LEFTALIGN,
                cursor_pos.x,
                cursor_pos.y,
                None,
                hwnd,
                None,
            ).ok();
            DestroyMenu(menu).ok();
        }
    } else if msg == WM_COMMAND {
    let cmd = wparam.0 as usize;

    if cmd == IDM_SHOW {
        // 通过 Windows 原生 API 直接恢复 Sound Lock 主窗口
        let main_hwnd = FindWindowW(None, w!("Sound Lock Rust"));

        if let Ok(hwnd) = main_hwnd {
            if !hwnd.is_invalid() {
                ShowWindow(hwnd, SW_RESTORE);
                ShowWindow(hwnd, SW_SHOW);
                SetForegroundWindow(hwnd);
            } else {
                log::warn!("找到 Sound Lock 窗口，但 HWND 无效");
            }
        } else {
            log::warn!("没有找到 Sound Lock Rust 主窗口");
        }

        // 同时通知 UI 状态恢复
        tray_state::WINDOW_VISIBLE.store(true, Ordering::SeqCst);
    } else if cmd == IDM_EXIT {
        tray_state::SHOULD_EXIT.store(true, Ordering::SeqCst);
        PostQuitMessage(0);
    }
} else if msg == WM_DESTROY {
        PostQuitMessage(0);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

fn main() -> Result<(), ()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("debug")).init();

    // VB-Cable 安装（保持不变）
    if !setup::is_vbcable_installed() {
        let user_wants_install = message_box_yes_no(
            "虚拟声卡未安装",
            "Sound Lock 需要虚拟声卡 VB-Cable 才能工作。\n\n是否立即安装？（需要管理员权限）",
        );
        if user_wants_install {
            match setup::install_vbcable() {
                Ok(()) => {
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

    let instance = single_instance::SingleInstance::new("SoundLockRustInstance")
        .expect("无法创建单实例锁");
    if !instance.is_single() {
        return Ok(());
    }

    // ---------- 启动托盘线程 ----------
    std::thread::spawn(move || {
        unsafe {
            let hinstance = GetModuleHandleW(None).unwrap();
            let class_name = w!("SoundLockTrayWindow");
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(tray_wnd_proc),
                hInstance: hinstance.into(),
                hIcon: LoadIconW(None, IDI_APPLICATION).unwrap(),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap(),
                hbrBackground: HBRUSH::default(),
                lpszMenuName: PCWSTR::null(),
                lpszClassName: class_name,
                hIconSm: LoadIconW(None, IDI_APPLICATION).unwrap(),
                ..Default::default()
            };
            RegisterClassExW(&wc);

            let hwnd = CreateWindowExW(
                WS_EX_NOACTIVATE,
                class_name,
                w!(""),
                WS_OVERLAPPEDWINDOW,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                0,
                0,
                None,
                None,
                Some(hinstance.into()),
                None,
            ).expect("CreateWindowExW failed");

            let mut nid = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: hwnd,
                uID: 1,
                uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
                uCallbackMessage: WM_TRAYICON,
                hIcon: LoadIconW(None, IDI_APPLICATION).unwrap(),
                szTip: {
                    let mut tip: [u16; 128] = [0; 128];
                    let text = "Sound Lock\0";
                    for (i, c) in text.encode_utf16().take(127).enumerate() {
                        tip[i] = c;
                    }
                    tip
                },
                ..Default::default()
            };
            Shell_NotifyIconW(NIM_ADD, &mut nid).ok();

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                DispatchMessageW(&msg);
                if tray_state::SHOULD_EXIT.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
            }
            Shell_NotifyIconW(NIM_DELETE, &mut nid).ok();
        }
    });
    // ---------------------------------

    let icon_data = image::load_from_memory(include_bytes!("../assets/icon.png"))
        .expect("图标加载失败")
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
    let config = Config::load().unwrap_or_else(|e| {
        log::error!("加载配置失败: {}, 将使用默认配置", e);
        Arc::new(Mutex::new(Config::default()))
    });

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
    .expect("无法创建窗口");

    Ok(())
}