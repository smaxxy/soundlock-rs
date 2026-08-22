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


// 准星参数

/// 准星透明窗口边长（物理像素）。
const WINDOW_SIZE: i32 = 80;

/// 状态检查周期；仅样式变化时重绘。
const UPDATE_INTERVAL_MS: u32 = 20;

/// 抗锯齿 supersampling 网格边长。
const AA_GRID: i32 = 4;
const AA_SAMPLES: i32 = AA_GRID * AA_GRID;

/// 上一次已经提交到透明窗口的颜色。
static LAST_CROSSHAIR_COLOR: AtomicU32 =
    AtomicU32::new(u32::MAX);

/// 上一次已经提交到透明窗口的大小。
static LAST_CROSSHAIR_SIZE: AtomicU32 =
    AtomicU32::new(u32::MAX);


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


/// 返回准星 80×80 窗口左上角。
///
/// 线程使用 PER_MONITOR_AWARE_V2，因此坐标为真实物理像素。
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


/// 用 4×4 supersampling 生成 premultiplied BGRA，并通过
/// UpdateLayeredWindow 提交每像素 alpha。圆心固定在窗口几何中心，
/// 因而奇数和偶数直径保持同心。
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

    // UI 颜色格式：0x00RRGGBB。
    let r =
        ((rgb >> 16) & 0xFF)
            as u32;

    let g =
        ((rgb >> 8) & 0xFF)
            as u32;

    let b =
        (rgb & 0xFF)
            as u32;

    // 创建 80×80、32-bit、top-down DIB

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

    // 直接在 DIB 内存里生成圆

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

    // 背景 alpha=0。
    pixels.fill(0);

    // 偶数像素宽度的几何中心位于像素边界。
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

            // 根据像素内 4×4 采样点的覆盖率计算 alpha。
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

            // little-endian BGRA 内存对应 u32 0xAARRGGBB。
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

    // 提交到 layered window

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

    // 立即释放临时 GDI 资源，避免对象泄漏。

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


// Window Procedure

unsafe extern "system" fn crosshair_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        // 鼠标穿透

        WM_NCHITTEST => {
            return LRESULT(
                HTTRANSPARENT as isize,
            );
        }

        // Layered Window 自带 alpha，不让 Windows 擦除背景。

        WM_ERASEBKGND => {
            return LRESULT(1);
        }

        // 显示模式变化后重新渲染并定位到新屏幕中心。

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

        // 每 20ms 检查状态

        WM_TIMER => {
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

            // 仅样式变化时重新计算 alpha bitmap。

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

            if should_show {
                if !currently_visible {
                    let _ = ShowWindow(
                        hwnd,
                        SW_SHOWNOACTIVATE,
                    );
                }
            } else if currently_visible {
                let _ = ShowWindow(
                    hwnd,
                    SW_HIDE,
                );
            }

            return LRESULT(0);
        }

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


fn run_crosshair()
    -> windows::core::Result<()>
{
    unsafe {
        // 仅设置当前线程的 DPI 模式，避免影响 eframe UI；准星使用物理像素坐标。
        let _previous_dpi_context =
            SetThreadDpiAwarenessContext(
                DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            );

        let hinstance =
            GetModuleHandleW(None)?;

        let class_name =
            w!("SoundLockCrosshairWindow");

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

        let position =
            centered_window_position();

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

                    // 不抢游戏焦点
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

        // 隐藏状态下预生成 bitmap，首次显示无需临时计算。

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

        let _ = ShowWindow(
            hwnd,
            SW_HIDE,
        );

        SetTimer(
            Some(hwnd),
            1,
            UPDATE_INTERVAL_MS,
            None,
        );

        // 独立消息循环使准星在 eframe UI 关闭后继续运行。

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
                let _ = TranslateMessage(
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
