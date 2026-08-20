use std::sync::atomic::{AtomicU32, Ordering};

use windows::core::{w, PCWSTR};

use windows::Win32::Foundation::{
    COLORREF,
    HWND,
    LPARAM,
    LRESULT,
    RECT,
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

use windows::Win32::System::LibraryLoader::GetModuleHandleW;

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

//
// ============================================================
// 准星参数
// ============================================================
//

/// 准星窗口大小。
///
/// 实际圆的大小由 UI 中的 CROSSHAIR_SIZE 控制。
/// 80×80 很小，性能开销基本可以忽略。
const WINDOW_SIZE: i32 = 80;

/// 每隔多少毫秒检查一次：
///
/// - 准星开关
/// - 鼠标右键
/// - 颜色变化
/// - 大小变化
const UPDATE_INTERVAL_MS: u32 = 10;

/// 透明背景色。
///
/// 这里故意不用纯黑。
/// 这样 UI 里以后选择纯黑准星也能正常显示。
///
/// COLORREF 格式为：0x00BBGGRR
///
/// RGB(1,1,1)
const TRANSPARENT_COLOR: COLORREF =
    COLORREF(0x00010101);

/// 上一次已经绘制的颜色。
///
/// 用来判断 UI 中颜色有没有变化。
static LAST_CROSSHAIR_COLOR: AtomicU32 =
    AtomicU32::new(u32::MAX);

/// 上一次已经绘制的大小。
///
/// 用来判断 UI 中大小有没有变化。
static LAST_CROSSHAIR_SIZE: AtomicU32 =
    AtomicU32::new(u32::MAX);


//
// ============================================================
// 启动准星线程
// ============================================================
//

pub fn start_crosshair() {
    std::thread::spawn(|| {
        if let Err(e) = run_crosshair() {
            log::error!("准星线程运行失败: {:?}", e);
        }
    });
}


//
// ============================================================
// Window Procedure
// ============================================================
//

unsafe extern "system" fn crosshair_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        //
        // ----------------------------------------------------
        // 鼠标穿透
        // ----------------------------------------------------
        //
        // 鼠标即使正好位于准星上，
        // 准星窗口也不会接收鼠标操作。
        //
        // PUBG 仍然正常接收鼠标。
        //
        WM_NCHITTEST => {
            return LRESULT(HTTRANSPARENT as isize);
        }

        //
        // ----------------------------------------------------
        // 禁止 Windows 自动清背景
        // ----------------------------------------------------
        //
        WM_ERASEBKGND => {
            return LRESULT(1);
        }

        //
        // ----------------------------------------------------
        // 绘制准星
        // ----------------------------------------------------
        //
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();

            let hdc = BeginPaint(
                hwnd,
                &mut ps,
            );

            //
            // -----------------------------------------------
            // 先画透明背景
            // -----------------------------------------------
            //

            let background_brush =
                CreateSolidBrush(
                    TRANSPARENT_COLOR,
                );

            let rect = RECT {
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

            //
            // -----------------------------------------------
            // 获取当前 UI 设置的准星颜色
            // -----------------------------------------------
            //
            // CROSSHAIR_COLOR 保存格式：
            //
            // 0x00RRGGBB
            //

            let rgb =
                crate::tray_state::CROSSHAIR_COLOR
                    .load(Ordering::SeqCst);

            let r =
                ((rgb >> 16) & 0xFF) as u32;

            let g =
                ((rgb >> 8) & 0xFF) as u32;

            let b =
                (rgb & 0xFF) as u32;

            //
            // Windows COLORREF 是：
            //
            // 0x00BBGGRR
            //

            let colorref =
                COLORREF(
                    r
                        | (g << 8)
                        | (b << 16),
                );

            let crosshair_brush =
                CreateSolidBrush(colorref);

            let old_brush =
                SelectObject(
                    hdc,
                    HGDIOBJ(
                        crosshair_brush.0,
                    ),
                );

            //
            // -----------------------------------------------
            // 去掉圆形边框
            // -----------------------------------------------
            //
            // 我们只需要：
            //
            //     实心圆
            //

            let null_pen =
                GetStockObject(NULL_PEN);

            let old_pen =
                SelectObject(
                    hdc,
                    null_pen,
                );

            //
            // -----------------------------------------------
            // 获取当前 UI 设置的准星大小
            // -----------------------------------------------
            //

            let diameter =
                crate::tray_state::CROSSHAIR_SIZE
                    .load(Ordering::SeqCst)
                    as i32;

            //
            // 防止 UI 或配置异常导致圆超出窗口。
            //

            let diameter =
                diameter.clamp(
                    1,
                    WINDOW_SIZE - 2,
                );

            //
            // 让圆始终位于 80×80 窗口正中心。
            //

            let offset =
                (WINDOW_SIZE - diameter) / 2;

            let _ = Ellipse(
                hdc,
                offset,
                offset,
                offset + diameter,
                offset + diameter,
            );

            //
            // -----------------------------------------------
            // 恢复 GDI 对象
            // -----------------------------------------------
            //

            SelectObject(
                hdc,
                old_pen,
            );

            SelectObject(
                hdc,
                old_brush,
            );

            let _ = DeleteObject(
                HGDIOBJ(
                    crosshair_brush.0,
                ),
            );

            let _ = DeleteObject(
                HGDIOBJ(
                    background_brush.0,
                ),
            );

            EndPaint(
                hwnd,
                &ps,
            );

            return LRESULT(0);
        }

        //
        // ----------------------------------------------------
        // 每 10ms 检查状态
        // ----------------------------------------------------
        //
        WM_TIMER => {
            //
            // -----------------------------------------------
            // 程序是否真正退出
            // -----------------------------------------------
            //
            // 这里读取和音频线程相同的 SHOULD_EXIT。
            //
            // 关闭 UI：
            //     SHOULD_EXIT 还是 false
            //     → 准星继续运行
            //
            // 托盘点击“退出”：
            //     SHOULD_EXIT = true
            //     → 准星结束
            //

            if crate::tray_state::SHOULD_EXIT
                .load(Ordering::SeqCst)
            {
                let _ =
                    DestroyWindow(hwnd);

                return LRESULT(0);
            }

            //
            // -----------------------------------------------
            // 准星总开关
            // -----------------------------------------------
            //

            let enabled =
                crate::tray_state::CROSSHAIR_ENABLED
                    .load(Ordering::SeqCst);

            //
            // -----------------------------------------------
            // 检查鼠标右键
            // -----------------------------------------------
            //
            // GetAsyncKeyState 返回值最高位：
            //
            // 1 = 当前按住
            // 0 = 当前没有按
            //

            let right_button_down =
                (
                    GetAsyncKeyState(
                        VK_RBUTTON.0 as i32,
                    ) as u16
                        & 0x8000
                ) != 0;

            //
            // -----------------------------------------------
            // 最终显示条件
            // -----------------------------------------------
            //

            let should_show =
                enabled
                    && !right_button_down;

            let currently_visible =
                IsWindowVisible(hwnd)
                    .as_bool();

            //
            // -----------------------------------------------
            // 当前颜色
            // -----------------------------------------------
            //

            let current_color =
                crate::tray_state::CROSSHAIR_COLOR
                    .load(Ordering::SeqCst);

            let last_color =
                LAST_CROSSHAIR_COLOR
                    .load(Ordering::SeqCst);

            //
            // -----------------------------------------------
            // 当前大小
            // -----------------------------------------------
            //

            let current_size =
                crate::tray_state::CROSSHAIR_SIZE
                    .load(Ordering::SeqCst);

            let last_size =
                LAST_CROSSHAIR_SIZE
                    .load(Ordering::SeqCst);

            //
            // =================================================
            // 需要显示准星
            // =================================================
            //

            if should_show {
                //
                // ---------------------------------------------
                // 当前隐藏
                // → 显示
                // ---------------------------------------------
                //

                if !currently_visible {
                    ShowWindow(
                        hwnd,
                        SW_SHOWNOACTIVATE,
                    );

                    let _ =
                        InvalidateRect(
                            Some(hwnd),
                            None,
                            false,
                        );

                    LAST_CROSSHAIR_COLOR
                        .store(
                            current_color,
                            Ordering::SeqCst,
                        );

                    LAST_CROSSHAIR_SIZE
                        .store(
                            current_size,
                            Ordering::SeqCst,
                        );
                }

                //
                // ---------------------------------------------
                // 当前已经显示
                // 但是 UI 改了颜色或大小
                // → 立即重新绘制
                // ---------------------------------------------
                //

                else if current_color != last_color
                    || current_size != last_size
                {
                    let _ =
                        InvalidateRect(
                            Some(hwnd),
                            None,
                            false,
                        );

                    LAST_CROSSHAIR_COLOR
                        .store(
                            current_color,
                            Ordering::SeqCst,
                        );

                    LAST_CROSSHAIR_SIZE
                        .store(
                            current_size,
                            Ordering::SeqCst,
                        );
                }
            }

            //
            // =================================================
            // 不应该显示
            // =================================================
            //
            // 两种情况：
            //
            // 1. UI 关闭了准星开关
            // 2. 当前正在按鼠标右键
            //

            else if currently_visible {
                ShowWindow(
                    hwnd,
                    SW_HIDE,
                );
            }

            return LRESULT(0);
        }

        //
        // ----------------------------------------------------
        // 窗口销毁
        // ----------------------------------------------------
        //

        WM_DESTROY => {
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


//
// ============================================================
// 创建准星窗口
// ============================================================
//

fn run_crosshair()
    -> windows::core::Result<()>
{
    unsafe {
        let hinstance =
            GetModuleHandleW(None)?;

        let class_name =
            w!("SoundLockCrosshairWindow");

        //
        // ----------------------------------------------------
        // 注册 Win32 Window Class
        // ----------------------------------------------------
        //

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

        //
        // ----------------------------------------------------
        // 获取主显示器分辨率
        // ----------------------------------------------------
        //

        let screen_width =
            GetSystemMetrics(
                SM_CXSCREEN,
            );

        let screen_height =
            GetSystemMetrics(
                SM_CYSCREEN,
            );

        //
        // ----------------------------------------------------
        // 让 80×80 的准星窗口本身居中
        // ----------------------------------------------------
        //

        let x =
            screen_width / 2
                - WINDOW_SIZE / 2;

        let y =
            screen_height / 2
                - WINDOW_SIZE / 2;

        //
        // ----------------------------------------------------
        // 创建准星窗口
        // ----------------------------------------------------
        //

        let hwnd =
            CreateWindowExW(
                //
                // 永远置顶
                //
                WS_EX_TOPMOST

                    //
                    // 分层窗口
                    // 允许 ColorKey 透明
                    //
                    | WS_EX_LAYERED

                    //
                    // 鼠标穿透
                    //
                    | WS_EX_TRANSPARENT

                    //
                    // 不出现在 Alt+Tab
                    //
                    | WS_EX_TOOLWINDOW

                    //
                    // 不抢 PUBG 焦点
                    //
                    | WS_EX_NOACTIVATE,

                class_name,

                w!(""),

                //
                // 无边框 Popup
                //
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

        //
        // ----------------------------------------------------
        // 设置背景透明色
        // ----------------------------------------------------
        //
        // 所有 RGB(1,1,1) 的像素都会变透明。
        //

        SetLayeredWindowAttributes(
            hwnd,
            TRANSPARENT_COLOR,
            255,
            LWA_COLORKEY,
        )?;

        //
        // ----------------------------------------------------
        // 默认隐藏
        // ----------------------------------------------------
        //
        // 因为 tray_state.rs 中：
        //
        // CROSSHAIR_ENABLED = false
        //

        ShowWindow(
            hwnd,
            SW_HIDE,
        );

        //
        // ----------------------------------------------------
        // 每 10ms 触发一次 WM_TIMER
        // ----------------------------------------------------
        //

        SetTimer(
            Some(hwnd),
            1,
            UPDATE_INTERVAL_MS,
            None,
        );

        //
        // ----------------------------------------------------
        // 独立 Windows 消息循环
        // ----------------------------------------------------
        //
        // 这个线程完全不依赖 eframe UI。
        //
        // 所以：
        //
        // UI关闭
        // ↓
        // eframe释放
        // ↓
        // crosshair.rs 仍然继续
        //

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