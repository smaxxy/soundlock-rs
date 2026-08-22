#![windows_subsystem = "windows"]

mod audio;
mod config;
mod crosshair;
mod setup;
mod tray_state;
mod ui;

use crate::config::{Config, RuntimeLimiterParams};
use crate::ui::SettingsWindow;

use eframe::egui;
use egui::IconData;

use std::sync::{Arc, Mutex};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

#[derive(Default)]
pub struct AppState {
    pub is_limiting: bool,
}

/// 简单 Yes / No 消息框

fn message_box_yes_no(
    title: &str,
    text: &str,
) -> bool {
    let title_wide: Vec<u16> =
        title
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

    let text_wide: Vec<u16> =
        text
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

    unsafe {
        let ret = MessageBoxW(
            None,
            PCWSTR::from_raw(
                text_wide.as_ptr(),
            ),
            PCWSTR::from_raw(
                title_wide.as_ptr(),
            ),
            MB_ICONQUESTION | MB_YESNO,
        );

        ret == IDYES
    }
}

/// 托盘

const WM_TRAYICON: u32 = WM_APP;
const IDM_EXIT: usize = 1002;

unsafe extern "system" fn tray_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    use std::sync::atomic::Ordering;

    match msg {
        WM_TRAYICON => {
            // 左键请求重新打开 UI，并结束当前托盘消息循环。
            if lparam.0 as u32 == WM_LBUTTONUP {
                tray_state::SHOULD_SHOW_UI.store(
                    true,
                    Ordering::SeqCst,
                );

                // WM_QUIT 只结束托盘循环，并会被下方的 GetMessageW 消费。
                PostQuitMessage(0);

                return LRESULT(0);
            }

            // 右键显示退出菜单。
            if lparam.0 as u32 == WM_RBUTTONUP {
                let mut cursor_pos =
                    Default::default();

                // Win32 callback 内禁止 panic。
                if GetCursorPos(
                    &mut cursor_pos,
                )
                .is_ok()
                {
                    let _ = SetForegroundWindow(hwnd);

                    // panic 不得穿过 extern "system" FFI 边界。
                    if let Ok(menu) =
                        CreatePopupMenu()
                    {
                        AppendMenuW(
                            menu,
                            MF_STRING,
                            IDM_EXIT,
                            w!("退出"),
                        )
                        .ok();

                        let _ = TrackPopupMenu(
                            menu,
                            TPM_LEFTALIGN,
                            cursor_pos.x,
                            cursor_pos.y,
                            None,
                            hwnd,
                            None,
                        )
                        .ok();

                        DestroyMenu(menu).ok();
                    } else {
                        log::error!(
                            "Failed to create tray popup menu"
                        );
                    }
                }

                return LRESULT(0);
            }
        }

        WM_COMMAND => {
            // 菜单命令 ID 位于 LOWORD。
            let cmd =
                (wparam.0 as usize)
                    & 0xFFFF;

            if cmd == IDM_EXIT {
                // 仅退出菜单设置整个程序的退出标志。
                tray_state::SHOULD_EXIT.store(
                    true,
                    Ordering::SeqCst,
                );

                PostQuitMessage(0);

                return LRESULT(0);
            }
        }

        WM_DESTROY => {
            // 销毁托盘窗口不等于退出程序；重新打开 UI 也会走到这里。
            return LRESULT(0);
        }

        _ => {}
    }

    DefWindowProcW(
        hwnd,
        msg,
        wparam,
        lparam,
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrayAction {
    ShowUi,
    Exit,
    Failed,
}

fn run_tray_loop() -> TrayAction {
    use std::sync::atomic::Ordering;

    // 每次进入托盘前只清掉“重新打开 UI”请求。
    // SHOULD_EXIT 绝不能在这里清零。
    tray_state::SHOULD_SHOW_UI.store(
        false,
        Ordering::SeqCst,
    );

    unsafe {
        let hinstance =
            match GetModuleHandleW(None) {
                Ok(handle) => handle,

                Err(e) => {
                    log::error!(
                        "GetModuleHandleW failed: {}",
                        e
                    );
                    return TrayAction::Failed;
                }
            };

        let class_name =
            w!("SoundLockTrayWindow");

        // app.rc 将 assets/icon.ico 编译为资源 #1。
        let icon =
            match LoadIconW(
                Some(hinstance.into()),
                PCWSTR(1 as *const u16),
            ) {
                Ok(icon) => icon,

                Err(e) => {
                    log::error!(
                        "Failed to load tray icon: {}",
                        e
                    );
                    return TrayAction::Failed;
                }
            };

        let cursor =
            match LoadCursorW(
                None,
                IDC_ARROW,
            ) {
                Ok(cursor) => cursor,

                Err(e) => {
                    log::error!(
                        "Failed to load tray cursor: {}",
                        e
                    );
                    return TrayAction::Failed;
                }
            };

        let wc = WNDCLASSEXW {
            cbSize:
                std::mem::size_of::<
                    WNDCLASSEXW,
                >() as u32,

            style:
                CS_HREDRAW
                    | CS_VREDRAW,

            lpfnWndProc:
                Some(tray_wnd_proc),

            hInstance:
                hinstance.into(),

            hIcon: icon,

            hCursor: cursor,

            hbrBackground:
                HBRUSH::default(),

            lpszMenuName:
                PCWSTR::null(),

            lpszClassName:
                class_name,

            hIconSm: icon,

            ..Default::default()
        };

        let atom =
            RegisterClassExW(&wc);

        if atom == 0 {
            log::error!(
                "RegisterClassExW failed"
            );
            return TrayAction::Failed;
        }

        let hwnd =
            match CreateWindowExW(
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
                Some(
                    hinstance.into(),
                ),
                None,
            ) {
                Ok(hwnd) => hwnd,

                Err(e) => {
                    log::error!(
                        "CreateWindowExW failed: {}",
                        e
                    );

                    UnregisterClassW(
                        class_name,
                        Some(hinstance.into()),
                    )
                    .ok();

                    return TrayAction::Failed;
                }
            };

        let mut nid =
            NOTIFYICONDATAW {
                cbSize:
                    std::mem::size_of::<
                        NOTIFYICONDATAW,
                    >() as u32,

                hWnd: hwnd,

                uID: 1,

                uFlags:
                    NIF_ICON
                        | NIF_MESSAGE
                        | NIF_TIP,

                uCallbackMessage:
                    WM_TRAYICON,

                hIcon: icon,

                szTip: {
                    let mut tip:
                        [u16; 128] =
                        [0; 128];

                    let text =
                        "Sound Lock";

                    for (
                        i,
                        c,
                    ) in text
                        .encode_utf16()
                        .take(127)
                        .enumerate()
                    {
                        tip[i] = c;
                    }

                    tip
                },

                ..Default::default()
            };

        if Shell_NotifyIconW(
            NIM_ADD,
            &mut nid,
        )
        .ok()
        .is_err()
        {
            log::error!(
                "Failed to add tray icon"
            );

            DestroyWindow(hwnd).ok();

            UnregisterClassW(
                class_name,
                Some(hinstance.into()),
            )
            .ok();

            return TrayAction::Failed;
        }

        let mut msg =
            MSG::default();

        loop {
            // GetMessageW:
            //
            // > 0 : 普通消息
            // = 0 : 收到 WM_QUIT
            // < 0 : 调用失败
            let result =
                GetMessageW(
                    &mut msg,
                    None,
                    0,
                    0,
                )
                .0;

            if result > 0 {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            } else if result == 0 {
                // WM_QUIT 已在这里被消费。
                break;
            } else {
                log::error!(
                    "GetMessageW failed"
                );
                break;
            }
        }

        // 离开托盘模式：
        // 先删除托盘图标，再销毁隐藏窗口，最后注销窗口类。
        let _ = Shell_NotifyIconW(
            NIM_DELETE,
            &mut nid,
        )
        .ok();

        DestroyWindow(hwnd).ok();

        if let Err(e) =
            UnregisterClassW(
                class_name,
                Some(hinstance.into()),
            )
        {
            log::warn!(
                "UnregisterClassW failed: {}",
                e
            );
        }
    }

    if tray_state::SHOULD_EXIT.load(
        Ordering::SeqCst,
    ) {
        TrayAction::Exit
    } else if tray_state::SHOULD_SHOW_UI.swap(
        false,
        Ordering::SeqCst,
    ) {
        TrayAction::ShowUi
    } else {
        TrayAction::Failed
    }
}

/// Main

fn main() -> Result<(), ()> {
    // Logger

    env_logger::Builder::from_env(
        env_logger::Env::default()
            .default_filter_or(
                "warn",
            ),
    )
    .init();

    // 单实例检查
    // 必须先于 VB-Cable 检查，避免第二个进程弹安装提示或修改设备。
    let instance =
        match single_instance::
            SingleInstance::new(
                "SoundLockRustInstance",
            )
        {
            Ok(instance) =>
                instance,

            Err(e) => {
                log::error!(
                    "无法创建单实例锁: {}",
                    e
                );
                return Err(());
            }
        };

    if !instance.is_single() {
        return Ok(());
    }

    // VB-Cable 安装检查

    if !setup::is_vbcable_installed() {
        let user_wants_install =
            message_box_yes_no(
                "虚拟声卡未安装",
                "Sound Lock 需要虚拟声卡 VB-Cable 才能工作。\n\n是否立即安装？（需要管理员权限）",
            );

        if user_wants_install {
            match setup::
                install_vbcable()
            {
                Ok(()) => {
                    if let Err(e) =
                        setup::
                            set_default_playback_device(
                                "CABLE Input",
                            )
                    {
                        log::error!(
                            "设置默认播放设备失败: {}",
                            e
                        );
                    }
                }

                Err(e) => {
                    log::error!(
                        "VB-Cable 安装失败: {}",
                        e
                    );
                }
            }
        }
    }

    // Window Icon

    let icon_image =
        match image::
            load_from_memory(
                include_bytes!(
                    "../assets/icon.png"
                ),
            )
        {
            Ok(image) => image,

            Err(e) => {
                log::error!(
                    "图标加载失败: {}",
                    e
                );

                return Err(());
            }
        };

    let icon_rgba =
        icon_image
            .to_rgba8();

    let (
        icon_width,
        icon_height,
    ) =
        icon_rgba
            .dimensions();

    // UI 可反复创建；IconData 只构造一次，NativeOptions clone 共享 Arc。
    let window_icon =
        Arc::new(
            IconData {
                rgba:
                    icon_rgba
                        .into_raw(),

                width:
                    icon_width,

                height:
                    icon_height,
            },
        );

    // App State

    let app_state =
        Arc::new(
            Mutex::new(
                AppState::default(),
            ),
        );

    // Config

    let config =
        Config::load()
            .unwrap_or_else(
                |e| {
                    log::error!(
                        "加载配置失败: {}, 将使用默认配置",
                        e
                    );

                    Arc::new(
                        Mutex::new(
                            Config::default(),
                        ),
                    )
                },
            );

    // 唯一 RuntimeLimiterParams
    // UI publish 与 Audio load_if_changed 必须共享同一个实例。
    let runtime_params = {
        let cfg_guard =
            match config.lock() {
                Ok(guard) =>
                    guard,

                Err(poisoned) => {
                    log::warn!(
                        "Config mutex poisoned while creating RuntimeLimiterParams; using recovered value"
                    );

                    poisoned
                        .into_inner()
                }
            };

        Arc::new(
            RuntimeLimiterParams::new(
                &cfg_guard,
            ),
        )
    };

    // Crosshair

    crosshair::
        start_crosshair();

    // UI / Tray 生命周期
    // eframe 和 Win32 Tray 共用主线程。关闭 UI 后进入托盘消息循环，
    // 托盘左键返回后重建 UI，但复用 Audio、Ring 和 RuntimeLimiterParams。

    loop {
        // 每次重新打开 UI，都重新创建 NativeOptions。
        let native_options =
            eframe::NativeOptions {
                viewport:
                    egui::
                        ViewportBuilder::
                        default()
                        .with_inner_size(
                            [
                                400.0,
                                550.0,
                            ],
                        )
                        .with_min_inner_size(
                            [
                                300.0,
                                400.0,
                            ],
                        )
                        .with_icon(
                            Arc::clone(
                                &window_icon,
                            ),
                        ),

                // 关闭窗口后返回 main 进入托盘模式。
                run_and_return:
                    true,

                ..Default::default()
            };

        // move 闭包只拿本轮 UI 的 Arc clone。
        let ui_app_state =
            Arc::clone(
                &app_state,
            );

        let ui_config =
            Arc::clone(
                &config,
            );

        let ui_runtime_params =
            Arc::clone(
                &runtime_params,
            );

        let run_result =
            eframe::run_native(
                "Sound Lock Rust",
                native_options,

                Box::new(
                    move |_| {
                        Ok(
                            Box::new(
                                SettingsWindow::new(
                                    Arc::clone(
                                        &ui_app_state,
                                    ),

                                    Arc::clone(
                                        &ui_config,
                                    ),

                                    Arc::clone(
                                        &ui_runtime_params,
                                    ),
                                ),
                            ),
                        )
                    },
                ),
            );

        if let Err(e) =
            run_result
        {
            log::error!(
                "无法创建或运行窗口: {}",
                e
            );

            tray_state::SHOULD_EXIT
                .store(
                    true,
                    std::sync::
                        atomic::
                        Ordering::SeqCst,
                );

            break;
        }

        if tray_state::SHOULD_EXIT
            .load(
                std::sync::
                    atomic::
                    Ordering::SeqCst,
            )
        {
            break;
        }

        // UI 已关闭 -> 托盘模式

        let tray_action =
            run_tray_loop();

        match tray_action {
            TrayAction::ShowUi => {
                // 继续下一轮，在同一个主线程重新 run_native。
                continue;
            }

            TrayAction::Exit => {
                break;
            }

            TrayAction::Failed => {
                log::error!(
                    "托盘阶段异常结束；为避免留下无 UI、无托盘的后台进程，程序将安全退出"
                );

                tray_state::SHOULD_EXIT
                    .store(
                        true,
                        std::sync::
                            atomic::
                            Ordering::SeqCst,
                    );

                break;
            }
        }
    }

    // 最终退出
    // 无论托盘正常退出还是创建失败，都通知后台线程结束。
    tray_state::SHOULD_EXIT.store(
        true,
        std::sync::atomic::Ordering::SeqCst,
    );

    // Audio Supervisor 最长每 50ms 检查一次退出条件；
    // 这里等待所有已启动的 Supervisor 自己释放 Stream / Ring / Limiter。
    if !audio::wait_for_shutdown(std::time::Duration::from_secs(3)) {
        log::warn!("等待音频线程退出超时，主程序将继续结束");
    }
    Ok(())
}
