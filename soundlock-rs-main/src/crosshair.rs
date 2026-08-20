use std::sync::atomic::Ordering;

use windows::core::{w, PCWSTR};

use windows::Win32::Foundation::{
    COLORREF,
    HWND,
    LPARAM,
    LRESULT,
    WPARAM,
};

use windows::Win32::Graphics::Gdi::{
    BeginPaint,
    CreateSolidBrush,
    DeleteObject,
    Ellipse,
    EndPaint,
    FillRect,
    GetStockObject,
    InvalidateRect,
    SelectObject,
    HGDIOBJ,
    NULL_PEN,
    PAINTSTRUCT,
};

use windows::Win32::System::LibraryLoader::
    GetModuleHandleW;

use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState,
    VK_RBUTTON,
};

use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW,
    DefWindowProcW,
    DestroyWindow,
    DispatchMessageW,
    GetMessageW,
    GetSystemMetrics,
    IsWindowVisible,
    LoadCursorW,
    PostQuitMessage,
    RegisterClassExW,
    SetLayeredWindowAttributes,
    SetTimer,
    ShowWindow,
    TranslateMessage,
    CS_HREDRAW,
    CS_VREDRAW,
    HTTRANSPARENT,
    IDC_ARROW,
    LWA_COLORKEY,
    MSG,
    SM_CXSCREEN,
    SM_CYSCREEN,
    SW_HIDE,
    SW_SHOWNOACTIVATE,
    WM_DESTROY,
    WM_ERASEBKGND,
    WM_NCHITTEST,
    WM_PAINT,
    WM_TIMER,
    WNDCLASSEXW,
    WS_EX_LAYERED,
    WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST,
    WS_EX_TRANSPARENT,
    WS_POPUP,
};

/// ============================================================
/// 准星参数
/// ============================================================

/// 小窗口大小。
///
/// 窗口本身只有 40×40，
/// 不创建全屏透明 Overlay。
const WINDOW_SIZE: i32 = 40;

/// 蓝色实心圆直径。
///
/// 觉得大或者小以后只需要改这里。
const CROSSHAIR_DIAMETER: i32 = 16;

/// 检查右键和开关的周期。
///
/// 10ms 已经足够快。
const UPDATE_INTERVAL_MS: u32 = 10;

/// 背景颜色。
///
/// 纯黑作为透明色键。
const TRANSPARENT_COLOR: COLORREF =
    COLORREF(0x00000000);

/// 蓝色。
///
/// COLORREF 格式实际上是：
/// 0x00BBGGRR
///
/// 这里对应 RGB：
/// R = 0
/// G = 120
/// B = 255
const CROSSHAIR_COLOR: COLORREF =
    COLORREF(0x00FFFF00);

/// ============================================================
/// 启动准星后台线程
/// ============================================================

pub fn start_crosshair() {
    std::thread::spawn(|| {
        if let Err(e) = run_crosshair() {
            log::error!(
                "Crosshair thread failed: {:?}",
                e
            );
        }
    });
}

/// ============================================================
/// 准星窗口过程
/// ============================================================

unsafe extern "system" fn crosshair_wnd_proc(
    hwnd: HWND,
    msg: u32,
    _wparam: WPARAM,
    _lparam: LPARAM,
) -> LRESULT {
    match msg {
        // ----------------------------------------------------
        // 鼠标穿透
        // ----------------------------------------------------
        //
        // 即使鼠标位于准星圆点上，
        // 点击也不应该落到准星窗口本身。
        //
        // PUBG 仍然接收鼠标操作。
        WM_NCHITTEST => {
            return LRESULT(
                HTTRANSPARENT as isize
            );
        }

        // ----------------------------------------------------
        // 防止系统自动擦背景
        // ----------------------------------------------------
        //
        // 我们在 WM_PAINT 里面自己绘制背景。
        WM_ERASEBKGND => {
            return LRESULT(1);
        }

        // ----------------------------------------------------
        // 绘制准星
        // ----------------------------------------------------
        WM_PAINT => {
            let mut ps =
                PAINTSTRUCT::default();

            let hdc =
                BeginPaint(hwnd, &mut ps);

            // ================================================
            // 先把整个 40×40 窗口刷成黑色
            // ================================================
            //
            // 黑色稍后通过 LWA_COLORKEY
            // 变成完全透明。
            let background_brush =
                CreateSolidBrush(
                    TRANSPARENT_COLOR,
                );

            let rect =
                windows::Win32::Foundation::RECT {
                    left: 0,
                    top: 0,
                    right: WINDOW_SIZE,
                    bottom: WINDOW_SIZE,
                };

            FillRect(
                hdc,
                &rect,
                background_brush,
            );

            // ================================================
            // 创建蓝色画刷
            // ================================================

            let blue_brush =
                CreateSolidBrush(
                    CROSSHAIR_COLOR,
                );

            let old_brush =
                SelectObject(
                    hdc,
                    HGDIOBJ(
                        blue_brush.0,
                    ),
                );

            // ================================================
            // 不要圆形边框
            // ================================================
            //
            // 因为你要的是：
            //
            //     蓝色实心圆
            //
            // 而不是：
            //
            //     蓝色空心圆
            let null_pen =
                GetStockObject(
                    NULL_PEN,
                );

            let old_pen =
                SelectObject(
                    hdc,
                    null_pen,
                );

            // ================================================
            // 圆居中
            // ================================================

            let offset =
                (
                    WINDOW_SIZE
                        - CROSSHAIR_DIAMETER
                ) / 2;

            Ellipse(
                hdc,
                offset,
                offset,
                offset
                    + CROSSHAIR_DIAMETER,
                offset
                    + CROSSHAIR_DIAMETER,
            )
            .ok();

            // ================================================
            // 恢复 GDI 对象
            // ================================================

            SelectObject(
                hdc,
                old_pen,
            );

            SelectObject(
                hdc,
                old_brush,
            );

            DeleteObject(
                HGDIOBJ(
                    blue_brush.0,
                ),
            )
            .ok();

            DeleteObject(
                HGDIOBJ(
                    background_brush.0,
                ),
            )
            .ok();

            EndPaint(
                hwnd,
                &ps,
            );

            return LRESULT(0);
        }

        // ----------------------------------------------------
        // 定时检查
        // ----------------------------------------------------
        WM_TIMER => {
            // ================================================
            // 1. 整个程序是否退出
            // ================================================

            if crate::tray_state::SHOULD_EXIT.load(
                Ordering::SeqCst,
            ) {
                DestroyWindow(hwnd);
                return LRESULT(0);
            }

            // ================================================
            // 2. 准星总开关
            // ================================================

            let enabled =
                crate::tray_state::
                    CROSSHAIR_ENABLED
                    .load(
                        Ordering::SeqCst,
                    );

            // ================================================
            // 3. 检查鼠标右键
            // ================================================
            //
            // 返回值最高位为 1：
            // 表示当前按键正处于按下状态。
            let right_button_down =
                (
                    GetAsyncKeyState(
                        VK_RBUTTON.0
                            as i32,
                    )
                        as u16
                        & 0x8000
                ) != 0;

            // ================================================
            // 4. 最终显示逻辑
            // ================================================

            let should_show =
                enabled
                    && !right_button_down;

            let currently_visible =
                IsWindowVisible(hwnd)
                    .as_bool();

            if should_show
                && !currently_visible
            {
                ShowWindow(
                    hwnd,
                    SW_SHOWNOACTIVATE,
                );

                InvalidateRect(
                    Some(hwnd),
                    None,
                    false,
                );
            } else if !should_show
                && currently_visible
            {
                ShowWindow(
                    hwnd,
                    SW_HIDE,
                );
            }

            return LRESULT(0);
        }

        // ----------------------------------------------------
        // Window 被销毁
        // ----------------------------------------------------
        WM_DESTROY => {
            PostQuitMessage(0);
            return LRESULT(0);
        }

        _ => {}
    }

    DefWindowProcW(
        hwnd,
        msg,
        _wparam,
        _lparam,
    )
}

/// ============================================================
/// 创建并运行准星窗口
/// ============================================================

fn run_crosshair()
    -> windows::core::Result<()>
{
    unsafe {
        let hinstance =
            GetModuleHandleW(None)?;

        let class_name =
            w!("SoundLockCrosshairWindow");

        // ====================================================
        // 注册窗口类
        // ====================================================

        let wc =
            WNDCLASSEXW {
                cbSize:
                    std::mem::size_of::<
                        WNDCLASSEXW,
                    >() as u32,

                style:
                    CS_HREDRAW
                        | CS_VREDRAW,

                lpfnWndProc:
                    Some(
                        crosshair_wnd_proc,
                    ),

                hInstance:
                    hinstance.into(),

                hCursor:
                    LoadCursorW(
                        None,
                        IDC_ARROW,
                    )?,

                hbrBackground:
                    Default::default(),

                lpszMenuName:
                    PCWSTR::null(),

                lpszClassName:
                    class_name,

                ..Default::default()
            };

        RegisterClassExW(&wc);

        // ====================================================
        // 获取主屏幕大小
        // ====================================================

        let screen_width =
            GetSystemMetrics(
                SM_CXSCREEN,
            );

        let screen_height =
            GetSystemMetrics(
                SM_CYSCREEN,
            );

        let x =
            screen_width / 2
                - WINDOW_SIZE / 2;

        let y =
            screen_height / 2
                - WINDOW_SIZE / 2;

        // ====================================================
        // 创建窗口
        // ====================================================

        let hwnd =
            CreateWindowExW(
                WS_EX_TOPMOST
                    | WS_EX_LAYERED
                    | WS_EX_TRANSPARENT
                    | WS_EX_TOOLWINDOW
                    | WS_EX_NOACTIVATE,

                class_name,

                w!(""),

                WS_POPUP,

                x,
                y,

                WINDOW_SIZE,
                WINDOW_SIZE,

                None,
                None,

                Some(
                    hinstance.into(),
                ),

                None,
            )?;

        // ====================================================
        // 黑色变成完全透明
        // ====================================================

        SetLayeredWindowAttributes(
            hwnd,
            TRANSPARENT_COLOR,
            255,
            LWA_COLORKEY,
        )?;

        // ====================================================
        // 默认隐藏
        // ====================================================
        //
        // CROSSHAIR_ENABLED 初始值是 false。
        //
        // 等 UI 打开开关以后才显示。
        ShowWindow(
            hwnd,
            SW_HIDE,
        );

        // ====================================================
        // 启动 10ms Timer
        // ====================================================

        SetTimer(
    Some(hwnd),
    1,
    UPDATE_INTERVAL_MS,
    None,
);

        // ====================================================
        // 独立消息循环
        // ====================================================
        //
        // 这个线程和 eframe UI 完全独立。
        //
        // UI退出：
        //     此线程继续。
        //
        // 托盘退出：
        //     SHOULD_EXIT=true
        //     → DestroyWindow()
        //     → WM_DESTROY
        //     → PostQuitMessage
        //     → 线程结束。
        let mut msg =
            MSG::default();

        while GetMessageW(
            &mut msg,
            None,
            0,
            0,
        )
        .as_bool()
        {
            TranslateMessage(
                &msg,
            );

            DispatchMessageW(
                &msg,
            );
        }
    }

    Ok(())
}