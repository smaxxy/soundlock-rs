use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};

use windows::core::{w, Error, PCWSTR};

use windows::Win32::Foundation::{
    COLORREF,
    HWND,
    LPARAM,
    LRESULT,
    POINT,
    SIZE,
    WPARAM,
};

use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC,
    CreateDIBSection,
    DeleteDC,
    DeleteObject,
    SelectObject,
    AC_SRC_ALPHA,
    AC_SRC_OVER,
    BI_RGB,
    BITMAPINFO,
    BITMAPINFOHEADER,
    BLENDFUNCTION,
    DIB_RGB_COLORS,
    HGDIOBJ,
};

use windows::Win32::System::LibraryLoader::GetModuleHandleW;

use windows::Win32::UI::HiDpi::{
    SetThreadDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

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
    SetTimer,
    ShowWindow,
    TranslateMessage,
    UpdateLayeredWindow,
    CS_HREDRAW,
    CS_VREDRAW,
    HTTRANSPARENT,
    IDC_ARROW,
    MSG,
    SM_CXSCREEN,
    SM_CYSCREEN,
    SW_HIDE,
    SW_SHOWNOACTIVATE,
    ULW_ALPHA,
    WM_DESTROY,
    WM_DISPLAYCHANGE,
    WM_ERASEBKGND,
    WM_NCHITTEST,
    WM_TIMER,
    WNDCLASSEXW,
    WS_EX_LAYERED,
    WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST,
    WS_EX_TRANSPARENT,
    WS_POPUP,
};


// ============================================================
// 准星参数
// ============================================================

/// 准星透明窗口大小。
///
/// 80×80 = 6400 像素。
/// 32-bit BGRA 缓冲区仅约 25 KB。
const WINDOW_SIZE: i32 = 80;

/// 状态检查周期。
///
/// 20ms = 50Hz。
///
/// 只做：
/// - 准星开关检查
/// - 右键检查
/// - 颜色 / 大小是否变化
///
/// 不会每 20ms 重绘。
const UPDATE_INTERVAL_MS: u32 = 20;

/// 4×4 supersampling。
///
/// 仅在真正需要重绘时运行：
/// - 第一次创建
/// - 颜色改变
/// - 大小改变
/// - 显示分辨率改变
///
/// 80×80×16 ≈ 10 万个非常简单的距离比较，
/// 且不是持续运行，对 CPU 几乎没有影响。
const AA_GRID: i32 = 4;
const AA_SAMPLES: i32 = AA_GRID * AA_GRID;

/// 上一次已经提交到透明窗口的颜色。
static LAST_CROSSHAIR_COLOR: AtomicU32 =
    AtomicU32::new(u32::MAX);

/// 上一次已经提交到透明窗口的大小。
static LAST_CROSSHAIR_SIZE: AtomicU32 =
    AtomicU32::new(u32::MAX);


// ============================================================
// 启动准星线程
// ============================================================

pub fn start_crosshair() {
    std::thread::spawn(|| {
        if let Err(e) = run_crosshair() {
            log::error!(
                "准星线程运行失败: {:?}",
                e
            );
        }
    });
}


// ============================================================
// 屏幕中心
// ============================================================

/// 返回准星 80×80 窗口左上角。
///
/// crosshair 线程会设为 PER_MONITOR_AWARE_V2，
/// 所以这里使用的是该线程对应的真实屏幕坐标体系。
unsafe fn centered_window_position() -> POINT {
    let screen_width =
        GetSystemMetrics(
            SM_CXSCREEN,
        );

    let screen_height =
        GetSystemMetrics(
            SM_CYSCREEN,
        );

    POINT {
        x:
            screen_width / 2
                - WINDOW_SIZE / 2,

        y:
            screen_height / 2
                - WINDOW_SIZE / 2,
    }
}


// ============================================================
// 抗锯齿准星渲染
// ============================================================

/// 使用 32-bit premultiplied BGRA + UpdateLayeredWindow
/// 提交一个真正带 alpha 的抗锯齿圆。
///
/// 重要：
///
/// 这里不是传统 GDI Ellipse。
///
/// 原来的 GDI Ellipse：
/// - 整数像素栅格
/// - 没有真正抗锯齿
/// - 很小的圆视觉上容易“歪”或有锯齿
///
/// 现在：
/// - 4×4 supersampling
/// - 每像素 alpha
/// - 圆心固定为 (40.0, 40.0)
/// - 奇数 / 偶数直径都围绕同一个几何中心
///
/// 这能避免原来：
///
///     (80 - diameter) / 2
///
/// 在奇数 diameter 时丢掉 0.5 像素的问题。
unsafe fn render_crosshair(
    hwnd: HWND,
    rgb: u32,
    diameter: u32,
) -> windows::core::Result<()> {
    let diameter =
        diameter.clamp(
            1,
            (WINDOW_SIZE - 2) as u32,
        ) as f32;

    // UI 保存格式：
    // 0x00RRGGBB
    let r =
        ((rgb >> 16) & 0xFF)
            as u32;

    let g =
        ((rgb >> 8) & 0xFF)
            as u32;

    let b =
        (rgb & 0xFF)
            as u32;

    // --------------------------------------------------------
    // 创建 80×80、32-bit、top-down DIB
    // --------------------------------------------------------

    let mut bitmap_info =
        BITMAPINFO::default();

    bitmap_info.bmiHeader =
        BITMAPINFOHEADER {
            biSize:
                std::mem::size_of::<
                    BITMAPINFOHEADER,
                >() as u32,

            biWidth:
                WINDOW_SIZE,

            // 负数 = top-down，
            // 内存第 0 行就是屏幕最上面一行。
            biHeight:
                -WINDOW_SIZE,

            biPlanes:
                1,

            biBitCount:
                32,

            biCompression:
                BI_RGB.0,

            ..Default::default()
        };

    let memory_dc =
        CreateCompatibleDC(None);

    if memory_dc.is_invalid() {
        return Err(
            Error::from_thread(),
        );
    }

    let mut bits:
        *mut c_void =
        std::ptr::null_mut();

    let bitmap =
        match CreateDIBSection(
            None,
            &bitmap_info,
            DIB_RGB_COLORS,
            &mut bits,
            None,
            0,
        ) {
            Ok(bitmap) =>
                bitmap,

            Err(e) => {
                let _ =
                    DeleteDC(
                        memory_dc,
                    );

                return Err(e);
            }
        };

    if bits.is_null() {
        let _ =
            DeleteObject(
                HGDIOBJ(
                    bitmap.0,
                ),
            );

        let _ =
            DeleteDC(
                memory_dc,
            );

        return Err(
            Error::from_thread(),
        );
    }

    let old_bitmap =
        SelectObject(
            memory_dc,
            HGDIOBJ(
                bitmap.0,
            ),
        );

    // --------------------------------------------------------
    // 直接在 DIB 内存里生成圆
    // --------------------------------------------------------

    let pixel_count =
        (WINDOW_SIZE
            * WINDOW_SIZE)
            as usize;

    let pixels =
        std::slice::
            from_raw_parts_mut(
                bits as *mut u32,
                pixel_count,
            );

    // 整个窗口背景 alpha=0，
    // 完全透明。
    pixels.fill(0);

    // 几何圆心固定在窗口真正中心：
    //
    // 80 / 2 = 40.0
    //
    // 注意这里故意不是 39.5 / 40.5。
    // 对于偶数像素宽度，几何中心本来就在像素边界上。
    let center =
        WINDOW_SIZE as f32
            * 0.5;

    let radius =
        diameter
            * 0.5;

    let radius_sq =
        radius
            * radius;

    for y in 0..WINDOW_SIZE {
        for x in 0..WINDOW_SIZE {
            let mut inside =
                0i32;

            // 4×4 supersampling。
            //
            // 一个屏幕像素内部取 16 个采样点，
            // 根据落入圆内的采样点数量计算 alpha。
            for sample_y in 0..AA_GRID {
                let py =
                    y as f32
                        + (
                            sample_y as f32
                                + 0.5
                        )
                            / AA_GRID as f32;

                let dy =
                    py - center;

                for sample_x in 0..AA_GRID {
                    let px =
                        x as f32
                            + (
                                sample_x as f32
                                    + 0.5
                            )
                                / AA_GRID as f32;

                    let dx =
                        px - center;

                    if dx * dx
                        + dy * dy
                        <= radius_sq
                    {
                        inside += 1;
                    }
                }
            }

            if inside == 0 {
                continue;
            }

            // 0..16 -> 0..255
            let alpha =
                (
                    inside
                        * 255
                        + AA_SAMPLES / 2
                )
                    / AA_SAMPLES;

            let alpha =
                alpha
                    .clamp(
                        0,
                        255,
                    )
                    as u32;

            // UpdateLayeredWindow + AC_SRC_ALPHA
            // 要求源像素为 premultiplied alpha。
            let premul_r =
                (
                    r * alpha
                        + 127
                )
                    / 255;

            let premul_g =
                (
                    g * alpha
                        + 127
                )
                    / 255;

            let premul_b =
                (
                    b * alpha
                        + 127
                )
                    / 255;

            // little-endian 内存：
            //
            // B G R A
            //
            // u32 形式：
            //
            // 0xAARRGGBB
            pixels[
                (
                    y * WINDOW_SIZE
                        + x
                ) as usize
            ] =
                (alpha << 24)
                    | (premul_r << 16)
                    | (premul_g << 8)
                    | premul_b;
        }
    }

    // --------------------------------------------------------
    // 提交到 layered window
    // --------------------------------------------------------

    let source_point =
        POINT {
            x: 0,
            y: 0,
        };

    let destination_point =
        centered_window_position();

    let size =
        SIZE {
            cx:
                WINDOW_SIZE,

            cy:
                WINDOW_SIZE,
        };

    let blend =
        BLENDFUNCTION {
            BlendOp:
                AC_SRC_OVER as u8,

            BlendFlags:
                0,

            SourceConstantAlpha:
                255,

            AlphaFormat:
                AC_SRC_ALPHA as u8,
        };

    let update_result =
        UpdateLayeredWindow(
            hwnd,
            None,
            Some(
                &destination_point
                    as *const POINT,
            ),
            Some(
                &size
                    as *const SIZE,
            ),
            Some(
                memory_dc,
            ),
            Some(
                &source_point
                    as *const POINT,
            ),
            COLORREF(0),
            Some(
                &blend
                    as *const BLENDFUNCTION,
            ),
            ULW_ALPHA,
        );

    // --------------------------------------------------------
    // GDI 资源立刻释放
    // --------------------------------------------------------
    //
    // 没有长期 bitmap / DC 缓存，
    // 常驻内存更小，也不会积累 GDI object。
    //
    // 重绘本来就极少发生，
    // 所以每次临时创建的成本可以忽略。

    SelectObject(
        memory_dc,
        old_bitmap,
    );

    let _ =
        DeleteObject(
            HGDIOBJ(
                bitmap.0,
            ),
        );

    let _ =
        DeleteDC(
            memory_dc,
        );

    update_result
}


// ============================================================
// Window Procedure
// ============================================================

unsafe extern "system" fn crosshair_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        // ----------------------------------------------------
        // 鼠标穿透
        // ----------------------------------------------------

        WM_NCHITTEST => {
            return LRESULT(
                HTTRANSPARENT as isize,
            );
        }

        // ----------------------------------------------------
        // Layered Window 自己有透明 alpha
        // 不让 Windows 擦背景
        // ----------------------------------------------------

        WM_ERASEBKGND => {
            return LRESULT(1);
        }

        // ----------------------------------------------------
        // 显示模式 / 分辨率发生变化
        // ----------------------------------------------------
        //
        // 重新提交一次：
        //
        // - 会重新获取屏幕尺寸
        // - 会重新定位到真正中心
        //
        // 平时不会触发，不增加持续 CPU。

        WM_DISPLAYCHANGE => {
            let color =
                crate::tray_state::
                    CROSSHAIR_COLOR
                    .load(
                        Ordering::SeqCst,
                    );

            let size =
                crate::tray_state::
                    CROSSHAIR_SIZE
                    .load(
                        Ordering::SeqCst,
                    );

            if let Err(e) =
                render_crosshair(
                    hwnd,
                    color,
                    size,
                )
            {
                log::error!(
                    "显示模式变化后重绘准星失败: {}",
                    e
                );
            }

            return LRESULT(0);
        }

        // ----------------------------------------------------
        // 每 20ms 检查状态
        // ----------------------------------------------------

        WM_TIMER => {
            // 程序是否真正退出。
            if crate::tray_state::
                SHOULD_EXIT
                .load(
                    Ordering::SeqCst,
                )
            {
                let _ =
                    DestroyWindow(
                        hwnd,
                    );

                return LRESULT(0);
            }

            let enabled =
                crate::tray_state::
                    CROSSHAIR_ENABLED
                    .load(
                        Ordering::SeqCst,
                    );

            let right_button_down =
                (
                    GetAsyncKeyState(
                        VK_RBUTTON.0
                            as i32,
                    ) as u16
                        & 0x8000
                ) != 0;

            let should_show =
                enabled
                    && !right_button_down;

            let currently_visible =
                IsWindowVisible(
                    hwnd,
                )
                .as_bool();

            let current_color =
                crate::tray_state::
                    CROSSHAIR_COLOR
                    .load(
                        Ordering::SeqCst,
                    );

            let current_size =
                crate::tray_state::
                    CROSSHAIR_SIZE
                    .load(
                        Ordering::SeqCst,
                    );

            let last_color =
                LAST_CROSSHAIR_COLOR
                    .load(
                        Ordering::SeqCst,
                    );

            let last_size =
                LAST_CROSSHAIR_SIZE
                    .load(
                        Ordering::SeqCst,
                    );

            // -----------------------------------------------
            // 颜色 / 大小真正发生变化
            // 才重新计算 80×80 alpha bitmap。
            // -----------------------------------------------

            let style_changed =
                current_color
                    != last_color
                    || current_size
                        != last_size;

            if style_changed {
                match render_crosshair(
                    hwnd,
                    current_color,
                    current_size,
                ) {
                    Ok(()) => {
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

                    Err(e) => {
                        log::error!(
                            "重绘准星失败: {}",
                            e
                        );
                    }
                }
            }

            // -----------------------------------------------
            // 显示 / 隐藏
            // -----------------------------------------------

            if should_show {
                if !currently_visible {
                    // 如果这是第一次显示，
                    // 上面的 style_changed 一定为 true
                    // （LAST_* 初始是 u32::MAX），
                    // 所以 bitmap 已经准备好。
                    ShowWindow(
                        hwnd,
                        SW_SHOWNOACTIVATE,
                    );
                }
            } else if currently_visible {
                ShowWindow(
                    hwnd,
                    SW_HIDE,
                );
            }

            return LRESULT(0);
        }

        // ----------------------------------------------------
        // 窗口销毁
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
        wparam,
        lparam,
    )
}


// ============================================================
// 创建准星窗口
// ============================================================

fn run_crosshair()
    -> windows::core::Result<()>
{
    unsafe {
        // ----------------------------------------------------
        // 只把“准星线程”设为真实 DPI 感知
        // ----------------------------------------------------
        //
        // 不修改整个进程的 DPI 模式，
        // 所以不会干扰 eframe UI。
        //
        // 对高分辨率 / Windows 缩放显示器尤其重要：
        // 准星定位和 80×80 bitmap 都按真实像素体系工作。
        let _previous_dpi_context =
            SetThreadDpiAwarenessContext(
                DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            );

        let hinstance =
            GetModuleHandleW(None)?;

        let class_name =
            w!("SoundLockCrosshairWindow");

        // ----------------------------------------------------
        // 注册 Win32 Window Class
        // ----------------------------------------------------

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

        let atom =
            RegisterClassExW(
                &wc,
            );

        if atom == 0 {
            return Err(
                Error::from_thread(),
            );
        }

        // ----------------------------------------------------
        // 计算真实屏幕中心
        // ----------------------------------------------------

        let position =
            centered_window_position();

        // ----------------------------------------------------
        // 创建准星窗口
        // ----------------------------------------------------

        let hwnd =
            CreateWindowExW(
                // 永远置顶
                WS_EX_TOPMOST

                    // 每像素 alpha
                    | WS_EX_LAYERED

                    // 鼠标穿透
                    | WS_EX_TRANSPARENT

                    // 不出现在 Alt+Tab
                    | WS_EX_TOOLWINDOW

                    // 不抢 PUBG 焦点
                    | WS_EX_NOACTIVATE,

                class_name,

                w!(""),

                // 无边框 Popup
                WS_POPUP,

                position.x,
                position.y,

                WINDOW_SIZE,
                WINDOW_SIZE,

                None,
                None,

                Some(
                    hinstance.into(),
                ),

                None,
            )?;

        // ----------------------------------------------------
        // 首次生成抗锯齿 bitmap
        // ----------------------------------------------------
        //
        // 窗口仍保持隐藏。
        // UI 开启准星时只 ShowWindow，不需要临时再算。

        let initial_color =
            crate::tray_state::
                CROSSHAIR_COLOR
                .load(
                    Ordering::SeqCst,
                );

        let initial_size =
            crate::tray_state::
                CROSSHAIR_SIZE
                .load(
                    Ordering::SeqCst,
                );

        render_crosshair(
            hwnd,
            initial_color,
            initial_size,
        )?;

        LAST_CROSSHAIR_COLOR
            .store(
                initial_color,
                Ordering::SeqCst,
            );

        LAST_CROSSHAIR_SIZE
            .store(
                initial_size,
                Ordering::SeqCst,
            );

        // ----------------------------------------------------
        // 默认隐藏
        // ----------------------------------------------------

        ShowWindow(
            hwnd,
            SW_HIDE,
        );

        // ----------------------------------------------------
        // 20ms 状态检查
        // ----------------------------------------------------
        //
        // 50Hz 足够让“按住右键立即隐藏”没有明显迟滞，
        // 比原来的 10ms / 100Hz 减少一半定时器唤醒。

        SetTimer(
            Some(hwnd),
            1,
            UPDATE_INTERVAL_MS,
            None,
        );

        // ----------------------------------------------------
        // 独立 Windows 消息循环
        // ----------------------------------------------------
        //
        // 不依赖 eframe。
        // UI 被彻底关闭后准星仍然继续运行。

        let mut msg =
            MSG::default();

        loop {
            let result =
                GetMessageW(
                    &mut msg,
                    None,
                    0,
                    0,
                )
                .0;

            if result > 0 {
                TranslateMessage(
                    &msg,
                );

                DispatchMessageW(
                    &msg,
                );
            } else if result == 0 {
                break;
            } else {
                return Err(
                    Error::from_thread(),
                );
            }
        }
    }

    Ok(())
}