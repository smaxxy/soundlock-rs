#![windows_subsystem = "windows"]

mod audio;
mod config;
mod crosshair;
mod diagnostics;
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

/// ============================================================
/// 简单 Yes / No 消息框
/// ============================================================

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

/// ============================================================
/// 托盘
/// ============================================================

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
            if lparam.0 as u32 == WM_RBUTTONUP {
                let mut cursor_pos =
                    Default::default();

                // 即使获取鼠标位置失败，
                // 也不能在 Win32 callback 中 panic。
                if GetCursorPos(
                    &mut cursor_pos,
                )
                .is_ok()
                {
                    SetForegroundWindow(hwnd);

                    // 不在 extern "system" 回调中 unwrap。
                    //
                    // panic 穿过 FFI 边界是不安全的。
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

                        TrackPopupMenu(
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
                tray_state::SHOULD_EXIT.store(
                    true,
                    Ordering::SeqCst,
                );

                PostQuitMessage(0);

                return LRESULT(0);
            }
        }

        WM_DESTROY => {
            // 如果托盘窗口由于任何原因被销毁，
            // 同样通知音频后台线程退出。
            tray_state::SHOULD_EXIT.store(
                true,
                Ordering::SeqCst,
            );

            PostQuitMessage(0);

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

fn run_tray_loop() {
    unsafe {
        let hinstance =
            match GetModuleHandleW(None) {
                Ok(handle) => handle,

                Err(e) => {
                    log::error!(
                        "GetModuleHandleW failed: {}",
                        e
                    );
                    return;
                }
            };

        let class_name =
            w!("SoundLockTrayWindow");

        let icon =
            match LoadIconW(
                None,
                IDI_APPLICATION,
            ) {
                Ok(icon) => icon,

                Err(e) => {
                    log::error!(
                        "Failed to load tray icon: {}",
                        e
                    );
                    return;
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
                    return;
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
            return;
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
                    return;
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

                    // 数组本身已经补 0，
                    // 不需要在字符串里额外放 '\0'。
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

            // 没有托盘图标时继续进入消息循环，
            // 用户将没有正常退出入口。
            // 因此这里直接结束托盘阶段。
            DestroyWindow(hwnd).ok();
            return;
        }

        let mut msg =
            MSG::default();

        loop {
            // GetMessageW:
            //
            // > 0 : 收到普通消息
            // = 0 : 收到 WM_QUIT
            // < 0 : 调用失败
            //
            // 不能直接 .as_bool()，
            // 否则 -1 也会被当成 true。
            let result =
                GetMessageW(
                    &mut msg,
                    None,
                    0,
                    0,
                )
                .0;

            if result > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);

                if tray_state::
                    SHOULD_EXIT
                    .load(
                        std::sync::
                            atomic::
                            Ordering::
                            SeqCst,
                    )
                {
                    break;
                }
            } else if result == 0 {
                break;
            } else {
                log::error!(
                    "GetMessageW failed"
                );
                break;
            }
        }

        Shell_NotifyIconW(
            NIM_DELETE,
            &mut nid,
        )
        .ok();

        DestroyWindow(hwnd).ok();
    }
}

/// ============================================================
/// Main
/// ============================================================

fn main() -> Result<(), ()> {
    // ========================================================
    // Logger
    // ========================================================

    env_logger::Builder::from_env(
        env_logger::Env::default()
            .default_filter_or(
                "debug",
            ),
    )
    .init();

    // ========================================================
    // 单实例检查
    // ========================================================
    //
    // 必须尽量靠前。
    //
    // 原来的代码是在 VB-Cable 检查 / 安装之后
    // 才做单实例判断。
    //
    // 那会导致第二个进程也可能弹安装提示，
    // 甚至执行设备设置。
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
        log::info!(
            "Sound Lock 已在运行"
        );

        return Ok(());
    }

    // ========================================================
    // Diagnostics
    // ========================================================
    //
    // Limiter lifetime 统计只在整个应用 Session
    // 开始时清零一次。
    //
    // 以后重建 Limiter / 设备重连时不再清空。
    diagnostics::reset_limiter_stats();
    diagnostics::init();

    // ========================================================
    // VB-Cable 安装检查
    // ========================================================

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

    // ========================================================
    // Window Icon
    // ========================================================

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

    let icon_data =
        icon_rgba
            .into_raw();

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
                        Arc::new(
                            IconData {
                                rgba:
                                    icon_data,

                                width:
                                    icon_width,

                                height:
                                    icon_height,
                            },
                        ),
                    ),

            ..Default::default()
        };

    // ========================================================
    // App State
    // ========================================================

    let app_state =
        Arc::new(
            Mutex::new(
                AppState::default(),
            ),
        );

    // ========================================================
    // Config
    // ========================================================

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

    // ========================================================
    // 唯一 RuntimeLimiterParams
    // ========================================================
    //
    // 整个进程只创建这一份。
    //
    // 后续：
    //
    // Main
    //   │
    //   └── Arc<RuntimeLimiterParams>
    //          ├── UI publish()
    //          └── Audio / Limiter load_if_changed()
    //
    // 严禁 UI / Audio 各自 new 一份。
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

    // ========================================================
    // Crosshair
    // ========================================================

    crosshair::
        start_crosshair();

    // ========================================================
    // UI
    // ========================================================
    //
    // 注意：
    //
    // main 不直接启动 audio::start_limiter()。
    //
    // 音频仍由 UI 中的“启动声音锁”逻辑创建，
    // 避免重复创建 Input / Output Stream。
    //
    // 这里只把唯一的 runtime_params
    // 传给 SettingsWindow。
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
                                    &app_state,
                                ),

                                Arc::clone(
                                    &config,
                                ),

                                Arc::clone(
                                    &runtime_params,
                                ),
                            ),
                        ),
                    )
                },
            ),
        );

    if let Err(e) = run_result {
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

        return Err(());
    }

    // ========================================================
    // UI 已关闭
    // ========================================================
    //
    // UI 被释放后：
    //
    // - eframe / egui 内存释放
    // - 如果声音锁已经启动，Audio Thread 继续运行
    // - 准星后台线程继续运行
    // - 托盘负责最终退出
    log::info!(
        "UI 已关闭，启动系统托盘，限幅继续运行"
    );

    run_tray_loop();

    // ========================================================
    // 最终退出
    // ========================================================
    // 无论托盘是正常退出还是创建失败返回，都明确通知后台线程结束。
    tray_state::SHOULD_EXIT.store(
        true,
        std::sync::atomic::Ordering::SeqCst,
    );

    // Audio Supervisor 最长每 50ms 检查一次退出条件；
    // 这里等待所有已启动的 Supervisor 自己释放 Stream / Ring / Limiter。
    if !audio::wait_for_shutdown(std::time::Duration::from_secs(3)) {
        log::warn!("等待音频线程退出超时，主程序将继续结束");
    }

    log::info!("程序退出");
    Ok(())
}
