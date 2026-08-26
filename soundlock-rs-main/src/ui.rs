use crate::config::{
    Config,
    RuntimeLimiterParams,
};
use crate::{
    audio,
    AppState,
};

use cpal::traits::{
    DeviceTrait,
    HostTrait,
};
use cpal::Device;

use std::sync::atomic::{
    AtomicBool,
    Ordering,
};
use std::sync::{
    Arc,
    Mutex,
};

use windows::core::{
    w,
    Error,
    PCWSTR,
};

use windows::Win32::Foundation::{
    COLORREF,
    HWND,
    LPARAM,
    LRESULT,
    WPARAM,
};

use windows::Win32::Graphics::Gdi::{
    CreateFontW,
    DeleteObject,
    GetStockObject,
    UpdateWindow,
    CLEARTYPE_QUALITY,
    CLIP_DEFAULT_PRECIS,
    COLOR_WINDOW,
    DEFAULT_CHARSET,
    DEFAULT_GUI_FONT,
    DEFAULT_PITCH,
    FF_DONTCARE,
    FW_NORMAL,
    HFONT,
    HGDIOBJ,
    OUT_DEFAULT_PRECIS,
    SetBkMode,
GetSysColorBrush,
TRANSPARENT,
 HDC,
};

use windows::Win32::System::LibraryLoader::
    GetModuleHandleW;

use windows::Win32::System::SystemServices::{
    SS_ETCHEDHORZ,
    SS_LEFT,
    SS_RIGHT,
};

use windows::Win32::UI::Controls::{
    InitCommonControls,
    BST_CHECKED,
    BST_UNCHECKED,
    TBM_SETPAGESIZE,
    TBM_SETPOS,
    TBM_SETRANGE,
    TBM_SETTICFREQ,
    TBS_AUTOTICKS,
    TRACKBAR_CLASSW,
    WC_BUTTON,
    WC_COMBOBOXW,
    WC_EDITW,
    WC_STATICW,
};

use windows::Win32::UI::Controls::Dialogs::{
    ChooseColorW,
    CHOOSECOLORW,
    CC_FULLOPEN,
    CC_RGBINIT,
};

use windows::Win32::UI::Input::KeyboardAndMouse::
    EnableWindow;

use windows::Win32::UI::WindowsAndMessaging::*;


// ============================================================
// 控件 ID
// ============================================================

const ID_INPUT: i32 = 101;
const ID_OUTPUT: i32 = 102;
const ID_REFRESH: i32 = 103;

const ID_THRESHOLD: i32 = 110;
const ID_ATTACK: i32 = 111;
const ID_RELEASE: i32 = 112;
const ID_PRE_GAIN: i32 = 113;
const ID_PEAK_RELEASE: i32 = 114;

const ID_THRESHOLD_VALUE: i32 = 210;
const ID_ATTACK_VALUE: i32 = 211;
const ID_RELEASE_VALUE: i32 = 212;
const ID_PRE_GAIN_VALUE: i32 = 213;
const ID_PEAK_RELEASE_VALUE: i32 = 214;

const ID_CROSSHAIR: i32 = 120;
const ID_COLOR: i32 = 121;
const ID_SIZE: i32 = 122;

const ID_START_STOP: i32 = 130;
const ID_SAVE: i32 = 131;


// ============================================================
// UI Timer
// ============================================================

const UI_TIMER: usize = 1;

/// 原来 250ms。
///
/// 现在只用于：
/// - 检查后台设备枚举结果
/// - 更新 Start / Stop 状态
///
/// 1 秒足够，避免无意义高频唤醒。
const UI_STATUS_TIMER_MS: u32 = 1000;


// Trackbar TBM_GETPOS 就是 WM_USER。
const TBM_GETPOS: u32 = WM_USER;


// ============================================================
// SettingsWindow
// ============================================================

pub struct SettingsWindow {
    state:
        Arc<Mutex<AppState>>,

    config:
        Arc<Mutex<Config>>,

    runtime_params:
        Arc<RuntimeLimiterParams>,

    input_devices:
        Vec<(Device, String)>,

    output_devices:
        Vec<(Device, String)>,
custom_colors: [COLORREF; 16],
    // --------------------------------------------------------
    // 异步设备枚举
    // --------------------------------------------------------

    pending_devices:
        Arc<
            Mutex<
                Option<(
                    Vec<(Device, String)>,
                    Vec<(Device, String)>,
                )>,
            >,
        >,

    refresh_in_progress:
        Arc<AtomicBool>,

    // --------------------------------------------------------
    // Win32
    // --------------------------------------------------------

    hwnd:
        HWND,

    font:
        HFONT,
}


impl SettingsWindow {
    pub fn new(
        state: Arc<Mutex<AppState>>,
        config: Arc<Mutex<Config>>,
        runtime_params:
            Arc<RuntimeLimiterParams>,
    ) -> Self {
        Self {
            custom_colors: [COLORREF(0); 16],
            state,
            config,
            runtime_params,

            input_devices:
                Vec::new(),

            output_devices:
                Vec::new(),

            pending_devices:
                Arc::new(
                    Mutex::new(None),
                ),

            refresh_in_progress:
                Arc::new(
                    AtomicBool::new(false),
                ),

            hwnd:
                HWND::default(),

            font:
                HFONT::default(),
        }
    }


    // ========================================================
    // 音频设备
    // ========================================================

    fn get_devices(
    ) -> (
        Vec<(Device, String)>,
        Vec<(Device, String)>,
    ) {
        let host =
            cpal::default_host();

        let input =
            host
                .input_devices()
                .map(
                    |items| {
                        items
                            .filter_map(
                                |d| {
                                    d.id()
                                        .ok()
                                        .map(
                                            |id| {
                                                (
                                                    d,
                                                    id.to_string(),
                                                )
                                            },
                                        )
                                },
                            )
                            .collect()
                    },
                )
                .unwrap_or_default();

        let output =
            host
                .output_devices()
                .map(
                    |items| {
                        items
                            .filter_map(
                                |d| {
                                    d.id()
                                        .ok()
                                        .map(
                                            |id| {
                                                (
                                                    d,
                                                    id.to_string(),
                                                )
                                            },
                                        )
                                },
                            )
                            .collect()
                    },
                )
                .unwrap_or_default();

        (
            input,
            output,
        )
    }


    // ========================================================
    // Win32 Helpers
    // ========================================================

    unsafe fn control(
        &self,
        id: i32,
    ) -> HWND {
        GetDlgItem(
            Some(self.hwnd),
            id,
        )
        .unwrap_or_default()
    }


    unsafe fn set_text(
        &self,
        id: i32,
        value: &str,
    ) {
        let value =
            wide(value);

        let _ =
            SetWindowTextW(
                self.control(id),
                PCWSTR(
                    value.as_ptr(),
                ),
            );
    }


    unsafe fn text(
        &self,
        id: i32,
    ) -> String {
        let hwnd =
            self.control(id);

        let length =
            GetWindowTextLengthW(
                hwnd,
            )
                as usize;

        let mut buffer =
            vec![
                0u16;
                length + 1
            ];

        let used =
            GetWindowTextW(
                hwnd,
                &mut buffer,
            );

        String::from_utf16_lossy(
            &buffer[
                ..used as usize
            ],
        )
    }


    // ========================================================
    // 创建控件
    // ========================================================

    unsafe fn create_controls(
        &mut self,
    ) {
        self.font =
            create_ui_font(
                -17,
                FW_NORMAL.0
                    as i32,
            );

        child(
            self.hwnd,
            WC_STATICW,
            "",
            WINDOW_STYLE(
                SS_ETCHEDHORZ.0,
            ),
            18,
            20,
            406,
            2,
            0,
            self.font,
        );


        // ====================================================
        // 音频设备
        // ====================================================

       
        label(
            self.hwnd,
            "输入设备",
            34,
            50,
            72,
            24,
            self.font,
        );

        child(
            self.hwnd,
            WC_COMBOBOXW,
            "",
            WINDOW_STYLE(
                CBS_DROPDOWNLIST
                    as u32,
            )
                | WS_VSCROLL
                | WS_TABSTOP,
            112,
            47,
            290,
            240,
            ID_INPUT,
            self.font,
        );

        label(
            self.hwnd,
            "输出设备",
            34,
            100,
            72,
            24,
            self.font,
        );

        child(
            self.hwnd,
            WC_COMBOBOXW,
            "",
            WINDOW_STYLE(
                CBS_DROPDOWNLIST
                    as u32,
            )
                | WS_VSCROLL
                | WS_TABSTOP,
            112,
            97,
            200,
            240,
            ID_OUTPUT,
            self.font,
        );
 child(
            self.hwnd,
            WC_BUTTON,
            "刷新设备",
            WINDOW_STYLE(
                BS_PUSHBUTTON
                    as u32,
            )
                | WS_TABSTOP,
            320,
            97,
            82,
            30,
            ID_REFRESH,
            self.font,
        );


        // ====================================================
        // Limiter
        // ====================================================

        group_box(
            self.hwnd,
            "",
            18,
            142,
            406,
            256,
            self.font,
        );

        slider_row(
            self.hwnd,
            "RMS 阈值",
            ID_THRESHOLD,
            ID_THRESHOLD_VALUE,
            167,
            self.font,
        );

        slider_row(
            self.hwnd,
            "RMS 触发",
            ID_ATTACK,
            ID_ATTACK_VALUE,
            212,
            self.font,
        );

        slider_row(
            self.hwnd,
            "RMS 释放",
            ID_RELEASE,
            ID_RELEASE_VALUE,
            257,
            self.font,
        );

        slider_row(
            self.hwnd,
            "Pre-Gain",
            ID_PRE_GAIN,
            ID_PRE_GAIN_VALUE,
            302,
            self.font,
        );

        slider_row(
            self.hwnd,
            "Peak 释放",
            ID_PEAK_RELEASE,
            ID_PEAK_RELEASE_VALUE,
            347,
            self.font,
        );


        // ====================================================
        // 准星
        // ====================================================

        child(
            self.hwnd,
            WC_BUTTON,
            "开启屏幕准星",
            WINDOW_STYLE(
                BS_AUTOCHECKBOX
                    as u32,
            )
                | WS_TABSTOP,
            34,
            418,
            126,
            26,
            ID_CROSSHAIR,
            self.font,
        );

        child(
            self.hwnd,
            WC_BUTTON,
            "选择颜色",
            WINDOW_STYLE(
                BS_PUSHBUTTON
                    as u32,
            )
                | WS_TABSTOP,
            168,
            415,
            102,
            30,
            ID_COLOR,
            self.font,
        );

        label(
            self.hwnd,
            "大小(px)",
            282,
            418,
            58,
            24,
            self.font,
        );

        child(
            self.hwnd,
            WC_EDITW,
            "",
            WS_BORDER
                | WINDOW_STYLE(
                    (
                        ES_NUMBER
                            | ES_AUTOHSCROLL
                    ) as u32,
                )
                | WS_TABSTOP,
            338,
            415,
            58,
            28,
            ID_SIZE,
            self.font,
        );


        // ====================================================
        // Start / Save
        // ====================================================

        child(
            self.hwnd,
            WC_BUTTON,
            "启动限制",
            WINDOW_STYLE(
                BS_DEFPUSHBUTTON
                    as u32,
            )
                | WS_TABSTOP,
            18,
            478,
            196,
            42,
            ID_START_STOP,
            self.font,
        );

        child(
            self.hwnd,
            WC_BUTTON,
            "保存设置",
            WINDOW_STYLE(
                BS_PUSHBUTTON
                    as u32,
            )
                | WS_TABSTOP,
            228,
            478,
            196,
            42,
            ID_SAVE,
            self.font,
        );


        // ====================================================
        // 初始化
        // ====================================================

        self.load_config_into_controls();

        // 音频设备不在 UI 主线程同步枚举。
        self.refresh_devices_async();

        self.refresh_status();
    }


    // ========================================================
    // Config -> UI
    // ========================================================

    unsafe fn load_config_into_controls(
        &self,
    ) {
        let cfg =
            match self.config.lock() {
                Ok(v) =>
                    v,

                Err(p) =>
                    p.into_inner(),
            };

        self.configure_slider(
            ID_THRESHOLD,
            0,
            600,
            (
                (
                    cfg.threshold_db
                        + 60.0
                )
                    * 10.0
            )
                .round()
                as u32,
            50,
            10,
        );

        self.configure_slider(
            ID_ATTACK,
            1,
            300,
            cfg.attack_ms,
            25,
            10,
        );

        self.configure_slider(
            ID_RELEASE,
            1,
            300,
            cfg.release_ms,
            25,
            10,
        );

        self.configure_slider(
            ID_PRE_GAIN,
            0,
            120,
            (
                cfg.pre_gain_db
                    * 10.0
            )
                .round()
                as u32,
            10,
            10,
        );

        self.configure_slider(
            ID_PEAK_RELEASE,
            20,
            150,
            cfg.peak_release_ms,
            10,
            10,
        );

        drop(cfg);

        self.update_slider_labels();

        self.set_text(
            ID_SIZE,
            &crate::tray_state::
                CROSSHAIR_SIZE
                .load(
                    Ordering::SeqCst,
                )
                .to_string(),
        );

        let checked =
            if crate::tray_state::
                CROSSHAIR_ENABLED
                .load(
                    Ordering::SeqCst,
                )
            {
                BST_CHECKED
            } else {
                BST_UNCHECKED
            };

        SendMessageW(
            self.control(
                ID_CROSSHAIR,
            ),
            BM_SETCHECK,
            Some(
                WPARAM(
                    checked.0
                        as usize,
                ),
            ),
            Some(
                LPARAM(0),
            ),
        );

    }


    // ========================================================
    // Slider
    // ========================================================

    unsafe fn configure_slider(
        &self,
        id: i32,
        minimum: u16,
        maximum: u16,
        position: u32,
        tick_frequency: usize,
        page_size: usize,
    ) {
        let hwnd =
            self.control(id);

        let range =
            minimum as u32
                | (
                    (
                        maximum
                            as u32
                    )
                        << 16
                );

        SendMessageW(
            hwnd,
            TBM_SETRANGE,
            Some(
                WPARAM(1),
            ),
            Some(
                LPARAM(
                    range
                        as isize,
                ),
            ),
        );

        SendMessageW(
            hwnd,
            TBM_SETTICFREQ,
            Some(
                WPARAM(
                    tick_frequency,
                ),
            ),
            None,
        );

        SendMessageW(
            hwnd,
            TBM_SETPAGESIZE,
            None,
            Some(
                LPARAM(
                    page_size
                        as isize,
                ),
            ),
        );

        SendMessageW(
            hwnd,
            TBM_SETPOS,
            Some(
                WPARAM(1),
            ),
            Some(
                LPARAM(
                    position
                        as isize,
                ),
            ),
        );
    }


    unsafe fn slider_position(
        &self,
        id: i32,
    ) -> u32 {
        SendMessageW(
            self.control(id),
            TBM_GETPOS,
            None,
            None,
        )
        .0
        .max(0)
            as u32
    }


    unsafe fn update_slider_labels(
        &self,
    ) {
        let threshold =
            self
                .slider_position(
                    ID_THRESHOLD,
                )
                as f32
                / 10.0
                - 60.0;

        let pre_gain =
            self
                .slider_position(
                    ID_PRE_GAIN,
                )
                as f32
                / 10.0;

        self.set_text(
            ID_THRESHOLD_VALUE,
            &format!(
                "{threshold:.1} dB"
            ),
        );

        self.set_text(
            ID_ATTACK_VALUE,
            &format!(
                "{} ms",
                self.slider_position(
                    ID_ATTACK,
                )
            ),
        );

        self.set_text(
            ID_RELEASE_VALUE,
            &format!(
                "{} ms",
                self.slider_position(
                    ID_RELEASE,
                )
            ),
        );

        self.set_text(
            ID_PRE_GAIN_VALUE,
            &format!(
                "+{pre_gain:.1} dB"
            ),
        );

        self.set_text(
            ID_PEAK_RELEASE_VALUE,
            &format!(
                "{} ms",
                self.slider_position(
                    ID_PEAK_RELEASE,
                )
            ),
        );
    }


    // ========================================================
    // 异步设备刷新
    // ========================================================

    unsafe fn refresh_devices_async(
        &self,
    ) {
        let limiting =
            match self.state.lock() {
                Ok(v) =>
                    v.is_limiting,

                Err(p) =>
                    p
                        .into_inner()
                        .is_limiting,
            };

        if limiting {
            return;
        }

        if self
            .refresh_in_progress
            .swap(
                true,
                Ordering::SeqCst,
            )
        {
            // 已经有刷新线程在跑。
            return;
        }

        let _ =
            EnableWindow(
                self.control(
                    ID_REFRESH,
                ),
                false,
            );

        let pending =
            Arc::clone(
                &self.pending_devices,
            );

        let refresh_flag =
            Arc::clone(
                &self.refresh_in_progress,
            );

        std::thread::spawn(
            move || {
                let devices =
                    Self::get_devices();

                match pending.lock() {
                    Ok(mut guard) => {
                        *guard =
                            Some(
                                devices,
                            );
                    }

                    Err(poisoned) => {
                        log::warn!(
                            "Device refresh result mutex poisoned; recovering"
                        );

                        *poisoned
                            .into_inner() =
                            Some(
                                devices,
                            );
                    }
                }

                refresh_flag.store(
                    false,
                    Ordering::SeqCst,
                );
            },
        );
    }


    unsafe fn apply_pending_devices(
        &mut self,
    ) {
        let devices =
            {
                match self
                    .pending_devices
                    .try_lock()
                {
                    Ok(mut pending) =>
                        pending.take(),

                    Err(_) =>
                        None,
                }
            };

        let Some(
            (
                inputs,
                outputs,
            ),
        ) = devices
        else {
            return;
        };


        let input =
            self.control(
                ID_INPUT,
            );

        let output =
            self.control(
                ID_OUTPUT,
            );

        SendMessageW(
            input,
            CB_RESETCONTENT,
            None,
            None,
        );

        SendMessageW(
            output,
            CB_RESETCONTENT,
            None,
            None,
        );


        self.input_devices =
            inputs;

        self.output_devices =
            outputs;


        for (
            device,
            _,
        ) in &self.input_devices
        {
            combo_add(
                input,
                &device
                    .description()
                    .map(
                        |d| {
                            d.to_string()
                        },
                    )
                    .unwrap_or_else(
                        |_| {
                            "未知输入设备"
                                .to_owned()
                        },
                    ),
            );
        }


        for (
            device,
            _,
        ) in &self.output_devices
        {
            combo_add(
                output,
                &device
                    .description()
                    .map(
                        |d| {
                            d.to_string()
                        },
                    )
                    .unwrap_or_else(
                        |_| {
                            "未知输出设备"
                                .to_owned()
                        },
                    ),
            );
        }


        let cfg =
            match self.config.lock() {
                Ok(v) =>
                    v,

                Err(p) =>
                    p.into_inner(),
            };


        if let Some(i) =
            cfg
                .target_input_device_id
                .as_deref()
                .and_then(
                    |id| {
                        find_device_index(
                            &self.input_devices,
                            id,
                        )
                    },
                )
        {
            SendMessageW(
                input,
                CB_SETCURSEL,
                Some(
                    WPARAM(i),
                ),
                None,
            );
        }


        if let Some(i) =
            cfg
                .target_output_device_id
                .as_deref()
                .and_then(
                    |id| {
                        find_device_index(
                            &self.output_devices,
                            id,
                        )
                    },
                )
        {
            SendMessageW(
                output,
                CB_SETCURSEL,
                Some(
                    WPARAM(i),
                ),
                None,
            );
        }

        drop(cfg);

        self.refresh_status();
    }


    // ========================================================
    // 准星大小实时更新
    // ========================================================

    unsafe fn update_crosshair_size_from_edit(
        &self,
    ) {
        let text =
            self.text(
                ID_SIZE,
            );

        if let Ok(value) =
            text
                .trim()
                .parse::<u32>()
        {
            let value =
                value.clamp(
                    4,
                    60,
                );

            crate::tray_state::
                CROSSHAIR_SIZE
                .store(
                    value,
                    Ordering::SeqCst,
                );
        }
    }


    unsafe fn normalize_crosshair_size_edit(
        &self,
    ) {
        let current =
            crate::tray_state::
                CROSSHAIR_SIZE
                .load(
                    Ordering::SeqCst,
                )
                .clamp(
                    4,
                    60,
                );

        let value =
            self
                .text(
                    ID_SIZE,
                )
                .trim()
                .parse::<u32>()
                .unwrap_or(
                    current,
                )
                .clamp(
                    4,
                    60,
                );

        crate::tray_state::
            CROSSHAIR_SIZE
            .store(
                value,
                Ordering::SeqCst,
            );

        self.set_text(
            ID_SIZE,
            &value.to_string(),
        );
    }


    // ========================================================
    // UI -> Config
    // ========================================================

    unsafe fn apply_controls(
        &self,
        save: bool,
    ) -> Result<(), String> {
        let parse_u32 =
            |id, label: &str| {
                self
                    .text(id)
                    .trim()
                    .parse::<u32>()
                    .map_err(
                        |_| {
                            format!(
                                "{label} 格式不正确"
                            )
                        },
                    )
            };


        let crosshair_size =
            parse_u32(
                ID_SIZE,
                "准星大小",
            )?
                .clamp(
                    4,
                    60,
                );


        crate::tray_state::
            CROSSHAIR_SIZE
            .store(
                crosshair_size,
                Ordering::SeqCst,
            );


        let mut cfg =
            match self.config.lock() {
                Ok(v) =>
                    v,

                Err(p) =>
                    p.into_inner(),
            };


        cfg.threshold_db =
            self
                .slider_position(
                    ID_THRESHOLD,
                )
                as f32
                / 10.0
                - 60.0;

        cfg.attack_ms =
            self
                .slider_position(
                    ID_ATTACK,
                )
                .clamp(
                    1,
                    300,
                );

        cfg.release_ms =
            self
                .slider_position(
                    ID_RELEASE,
                )
                .clamp(
                    1,
                    300,
                );

        cfg.pre_gain_db =
            self
                .slider_position(
                    ID_PRE_GAIN,
                )
                as f32
                / 10.0;

        cfg.peak_release_ms =
            self
                .slider_position(
                    ID_PEAK_RELEASE,
                )
                .clamp(
                    20,
                    150,
                );


        let in_idx =
            SendMessageW(
                self.control(
                    ID_INPUT,
                ),
                CB_GETCURSEL,
                None,
                None,
            )
                .0
                as usize;

        let out_idx =
            SendMessageW(
                self.control(
                    ID_OUTPUT,
                ),
                CB_GETCURSEL,
                None,
                None,
            )
                .0
                as usize;


        // 设备列表异步加载期间，
        // 不得用无效索引覆盖原 Config。
        if let Some(
            (
                _,
                id,
            ),
        ) =
            self
                .input_devices
                .get(
                    in_idx,
                )
        {
            cfg.target_input_device_id =
                Some(
                    id.clone(),
                );
        }


        if let Some(
            (
                _,
                id,
            ),
        ) =
            self
                .output_devices
                .get(
                    out_idx,
                )
        {
            cfg.target_output_device_id =
                Some(
                    id.clone(),
                );
        }


        // 准星开启状态不持久化。
// 每次重新启动 Sound Lock 都默认关闭。
cfg.crosshair_enabled = false;
        cfg.crosshair_color =
            crate::tray_state::
                CROSSHAIR_COLOR
                .load(
                    Ordering::SeqCst,
                )
                & 0x00FF_FFFF;

        cfg.crosshair_size =
            crosshair_size;


        self.runtime_params
            .publish_config(
                &cfg,
            );


        if save {
            cfg
                .save()
                .map_err(
                    |e| {
                        e.to_string()
                    },
                )?;
        }


        Ok(())
    }


    // ========================================================
    // Start / Stop
    // ========================================================

    unsafe fn start_or_stop(
        &self,
    ) {
        let limiting =
            match self.state.lock() {
                Ok(v) =>
                    v.is_limiting,

                Err(p) =>
                    p
                        .into_inner()
                        .is_limiting,
            };


        if limiting {
            match self.state.lock() {
                Ok(mut v) => {
                    v.is_limiting =
                        false;
                }

                Err(p) => {
                    p
                        .into_inner()
                        .is_limiting =
                        false;
                }
            }
        } else {
            if let Err(message) =
                self.apply_controls(
                    true,
                )
            {
                show_error(
                    self.hwnd,
                    &message,
                );

                return;
            }


            let has_devices =
                {
                    let cfg =
                        match self.config.lock() {
                            Ok(v) =>
                                v,

                            Err(p) =>
                                p.into_inner(),
                        };

                    cfg
                        .target_input_device_id
                        .is_some()
                        && cfg
                            .target_output_device_id
                            .is_some()
                };


            if !has_devices {
                show_error(
                    self.hwnd,
                    "请选择输入和输出设备",
                );

                return;
            }


            match self.state.lock() {
                Ok(mut v) => {
                    v.is_limiting =
                        true;
                }

                Err(p) => {
                    p
                        .into_inner()
                        .is_limiting =
                        true;
                }
            }


            audio::start_limiter(
                Arc::clone(
                    &self.state,
                ),
                Arc::clone(
                    &self.config,
                ),
                Arc::clone(
                    &self.runtime_params,
                ),
            );
        }


        self.refresh_status();
    }


    // ========================================================
    // Crosshair
    // ========================================================
unsafe fn choose_crosshair_color(&mut self) {
    // Sound Lock 当前保存的是 0xRRGGBB
    let current_rgb =
        crate::tray_state::CROSSHAIR_COLOR
            .load(Ordering::SeqCst)
            & 0x00FF_FFFF;

    // 转成 Windows COLORREF
    let current_colorref =
        rgb_to_colorref(current_rgb);

    let mut choose_color = CHOOSECOLORW {
        lStructSize:
            std::mem::size_of::<CHOOSECOLORW>() as u32,

        // 让颜色窗口属于 Sound Lock，
        // 弹出来后会正确位于主窗口前面。
        hwndOwner: self.hwnd,

        // 打开时默认选中当前准星颜色。
        rgbResult: current_colorref,

        // Windows 要求提供 16 个自定义颜色槽。
        lpCustColors:
            self.custom_colors.as_mut_ptr(),
        Flags:
            CC_RGBINIT
                | CC_FULLOPEN,

        ..Default::default()
    };

    // ChooseColorW 是模态原生 Win32 对话框。
    //
    // 用户点“确定” -> true
    // 用户点“取消” -> false
    if ChooseColorW(
        &mut choose_color
    )
    .as_bool()
    {
        // Windows COLORREF
        // 转回 Sound Lock 的 0xRRGGBB。
        let rgb =
            colorref_to_rgb(
                choose_color.rgbResult
            );

        crate::tray_state::CROSSHAIR_COLOR
            .store(
                rgb,
                Ordering::SeqCst,
            );

    }
}


    // ========================================================
    // UI 状态
    // ========================================================

    unsafe fn refresh_status(
        &self,
    ) {
        let limiting =
            match self.state.lock() {
                Ok(v) =>
                    v.is_limiting,

                Err(p) =>
                    p
                        .into_inner()
                        .is_limiting,
            };


        self.set_text(
            ID_START_STOP,
            if limiting {
                "停止"
            } else {
                "启动限制"
            },
        );


        let refreshing =
            self
                .refresh_in_progress
                .load(
                    Ordering::SeqCst,
                );


        let can_change_devices =
            !limiting
                && !refreshing;


        let _ =
            EnableWindow(
                self.control(
                    ID_INPUT,
                ),
                can_change_devices,
            );


        let _ =
            EnableWindow(
                self.control(
                    ID_OUTPUT,
                ),
                can_change_devices,
            );


        let _ =
            EnableWindow(
                self.control(
                    ID_REFRESH,
                ),
                can_change_devices,
            );
    }


    // ========================================================
    // WM_COMMAND
    // ========================================================

    unsafe fn handle_command(
        &mut self,
        id: i32,
        code: u16,
    ) {
        match id {
            ID_REFRESH => {
                self
                    .refresh_devices_async();

                self
                    .refresh_status();
            }


            ID_INPUT
            | ID_OUTPUT
                if code
                    == CBN_SELCHANGE
                        as u16 =>
            {
                let _ =
                    self
                        .apply_controls(
                            false,
                        );
            }


            ID_CROSSHAIR => {
                let checked =
                    SendMessageW(
                        self.control(
                            ID_CROSSHAIR,
                        ),
                        BM_GETCHECK,
                        None,
                        None,
                    )
                        .0
                        == BST_CHECKED
                            .0
                            as isize;


                crate::tray_state::
                    CROSSHAIR_ENABLED
                    .store(
                        checked,
                        Ordering::SeqCst,
                    );
            }


            ID_COLOR => {
                self
                    .choose_crosshair_color();
            }


            ID_SIZE
                if code
                    == EN_CHANGE
                        as u16 =>
            {
                self
                    .update_crosshair_size_from_edit();
            }


            ID_SIZE
                if code
                    == EN_KILLFOCUS
                        as u16 =>
            {
                self
                    .normalize_crosshair_size_edit();
            }


            ID_START_STOP => {
                self
                    .start_or_stop();
            }


            ID_SAVE => {
                self
                    .normalize_crosshair_size_edit();

                match self
                    .apply_controls(
                        true,
                    )
                {
                    Ok(()) => {
                        show_info(
                            self.hwnd,
                            "设置已保存",
                        );
                    }

                    Err(e) => {
                        show_error(
                            self.hwnd,
                            &e,
                        );
                    }
                }
            }


            _ => {}
        }
    }


    // ========================================================
    // Slider Change
    // ========================================================

    unsafe fn handle_slider_change(
        &self,
        control: HWND,
    ) {
        let id =
            GetDlgCtrlID(
                control,
            );


        if matches!(
            id,
            ID_THRESHOLD
                | ID_ATTACK
                | ID_RELEASE
                | ID_PRE_GAIN
                | ID_PEAK_RELEASE
        ) {
            self
                .update_slider_labels();

            let _ =
                self
                    .apply_controls(
                        false,
                    );
        }
    }
}


// ============================================================
// Drop
// ============================================================

impl Drop for SettingsWindow {
    fn drop(
        &mut self,
    ) {
        unsafe {
            delete_owned_font(
                self.font,
            );
        }
    }
}


// ============================================================
// Run Settings Window
// ============================================================

pub fn run_settings_window(
    window: SettingsWindow,
) -> windows::core::Result<()> {
    unsafe {
        InitCommonControls();


        let instance =
            GetModuleHandleW(
                None,
            )?;


        let class_name =
            w!(
                "SoundLockSettingsWindow"
            );


        let icon =
            LoadIconW(
                Some(
                    instance.into(),
                ),
                PCWSTR(
                    1
                        as *const u16,
                ),
            )
            .unwrap_or_default();


        let cursor =
            LoadCursorW(
                None,
                IDC_ARROW,
            )?;


        let wc =
            WNDCLASSEXW {
                cbSize:
                    std::mem::
                        size_of::<
                            WNDCLASSEXW,
                        >()
                        as u32,

                style:
                    CS_HREDRAW
                        | CS_VREDRAW,

                lpfnWndProc:
                    Some(
                        settings_wnd_proc,
                    ),

                hInstance:
                    instance.into(),

                hIcon:
                    icon,

                hCursor:
                    cursor,

                hbrBackground:
                    windows::
                        Win32::
                        Graphics::
                        Gdi::
                        HBRUSH(
                            (
                                COLOR_WINDOW
                                    .0
                                    + 1
                            )
                                as *mut _,
                        ),

                lpszClassName:
                    class_name,

                hIconSm:
                    icon,

                ..Default::default()
            };


        RegisterClassExW(
            &wc,
        );


        // ----------------------------------------------------
        // SettingsWindow 的所有权继续留在当前函数。
        //
        // Box 地址稳定，可以安全传给 HWND。
        //
        // 不再 Box::into_raw()，
        // 因此 CreateWindowExW 失败也不会泄漏。
        // ----------------------------------------------------

        let mut window =
            Box::new(
                window,
            );


        let state_ptr:
            *mut SettingsWindow =
            &mut *window;


        let hwnd_result =
            CreateWindowExW(
                WS_EX_APPWINDOW,

                class_name,

                w!(
                    "Sound Lock Rust"
                ),

                WS_OVERLAPPED
                    | WS_CAPTION
                    | WS_SYSMENU
                    | WS_MINIMIZEBOX,

                CW_USEDEFAULT,
                CW_USEDEFAULT,

                458,
                580,

                None,
                None,

                Some(
                    instance.into(),
                ),

                Some(
                    state_ptr
                        .cast(),
                ),
            );


        let hwnd =
            match hwnd_result {
                Ok(hwnd) =>
                    hwnd,

                Err(e) => {
                    UnregisterClassW(
                        class_name,
                        Some(
                            instance.into(),
                        ),
                    )
                    .ok();

                    return Err(e);
                }
            };


        let _ =
            ShowWindow(
                hwnd,
                SW_SHOW,
            );


        let _ =
            UpdateWindow(
                hwnd,
            );


        let mut msg =
            MSG::default();


        let loop_result:
            windows::core::Result<()> =
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
                    if !IsDialogMessageW(
                        hwnd,
                        &msg,
                    )
                    .as_bool()
                    {
                        let _ =
                            TranslateMessage(
                                &msg,
                            );

                        DispatchMessageW(
                            &msg,
                        );
                    }
                } else if result == 0 {
                    // WM_QUIT
                    break Ok(());
                } else {
                    // GetMessageW == -1
                    let error =
                        Error::
                            from_thread();

                    log::error!(
                        "Settings window GetMessageW failed: {}",
                        error
                    );

                    // 防止 SettingsWindow Box 被释放后，
                    // HWND 中仍保留失效 userdata。
                    DestroyWindow(
                        hwnd,
                    )
                    .ok();

                    break Err(
                        error,
                    );
                }
            };


        UnregisterClassW(
            class_name,
            Some(
                instance.into(),
            ),
        )
        .ok();


        // window Box 在这里自动 Drop。
        //
        // Drop 中只释放我们自己创建的 HFONT。
        drop(window);


        loop_result
    }
}


// ============================================================
// Window Procedure
// ============================================================

unsafe extern "system" fn settings_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // --------------------------------------------------------
    // HWND 建立时保存 SettingsWindow 地址。
    //
    // 注意：
    // SettingsWindow 本体由 run_settings_window()
    // 中的 Box 持有。
    // --------------------------------------------------------

    if msg
        == WM_NCCREATE
    {
        let create =
            &*(
                lparam.0
                    as *const CREATESTRUCTW
            );


        let state =
            create
                .lpCreateParams
                as *mut SettingsWindow;


        if !state.is_null() {
            (*state).hwnd =
                hwnd;


            SetWindowLongPtrW(
                hwnd,
                GWLP_USERDATA,
                state
                    as isize,
            );
        }
    }


    let state =
        GetWindowLongPtrW(
            hwnd,
            GWLP_USERDATA,
        )
            as *mut SettingsWindow;


    match msg {
        // ----------------------------------------------------
        // Create
        // ----------------------------------------------------
WM_CTLCOLORSTATIC => {
    let hdc = HDC(
        wparam.0 as *mut _
    );

    let _ = SetBkMode(
        hdc,
        TRANSPARENT,
    );

    return LRESULT(
        GetSysColorBrush(
            COLOR_WINDOW
        )
        .0 as isize
    );
}
        WM_CREATE
            if !state.is_null() =>
        {
            (*state)
                .create_controls();


            SetTimer(
                Some(
                    hwnd,
                ),
                UI_TIMER,
                UI_STATUS_TIMER_MS,
                None,
            );


            return LRESULT(0);
        }


        // ----------------------------------------------------
        // Command
        // ----------------------------------------------------

        WM_COMMAND
            if !state.is_null() =>
        {
            (*state)
                .handle_command(
                    (
                        wparam.0
                            & 0xFFFF
                    )
                        as i32,

                    (
                        (
                            wparam.0
                                >> 16
                        )
                            & 0xFFFF
                    )
                        as u16,
                );


            return LRESULT(0);
        }


        // ----------------------------------------------------
        // Trackbar
        // ----------------------------------------------------

        WM_HSCROLL
            if !state.is_null()
                && lparam.0
                    != 0 =>
        {
            (*state)
                .handle_slider_change(
                    HWND(
                        lparam.0
                            as *mut _,
                    ),
                );


            return LRESULT(0);
        }


        // ----------------------------------------------------
        // 低频状态 Timer
        // ----------------------------------------------------

        WM_TIMER
            if !state.is_null()
                && wparam.0
                    == UI_TIMER =>
        {
            // 后台设备枚举完成后，
            // 由 UI 线程安全接管 Vec<Device>。
            (*state)
                .apply_pending_devices();


            (*state)
                .refresh_status();


            return LRESULT(0);
        }


        // ----------------------------------------------------
        // Close
        // ----------------------------------------------------

        WM_CLOSE
            if !state.is_null() =>
        {
            (*state)
                .normalize_crosshair_size_edit();


            match (*state)
                .apply_controls(
                    true,
                )
            {
                Ok(()) => {
                    DestroyWindow(
                        hwnd,
                    )
                    .ok();
                }

                Err(e) => {
                    show_error(
                        hwnd,
                        &e,
                    );
                }
            }


            return LRESULT(0);
        }


        // ----------------------------------------------------
        // Destroy
        // ----------------------------------------------------

        WM_DESTROY => {
            KillTimer(
                Some(
                    hwnd,
                ),
                UI_TIMER,
            )
            .ok();


            PostQuitMessage(
                0,
            );


            return LRESULT(0);
        }


        // ----------------------------------------------------
        // NC Destroy
        // ----------------------------------------------------

        WM_NCDESTROY => {
            // 这里只清掉 HWND -> SettingsWindow 指针。
            //
            // 不再 Box::from_raw()。
            //
            // SettingsWindow 的所有权始终属于
            // run_settings_window() 中的 Box。
            SetWindowLongPtrW(
                hwnd,
                GWLP_USERDATA,
                0,
            );
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
// Child Control
// ============================================================

unsafe fn child(
    parent: HWND,
    class: PCWSTR,
    text: &str,
    style: WINDOW_STYLE,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    id: i32,
    font: HFONT,
) -> HWND {
    let text =
        wide(text);


    let hwnd =
        CreateWindowExW(
            WINDOW_EX_STYLE(0),

            class,

            PCWSTR(
                text.as_ptr(),
            ),

            WS_CHILD
                | WS_VISIBLE
                | style,

            x,
            y,

            width,
            height,

            Some(
                parent,
            ),

            Some(
                HMENU(
                    id
                        as *mut _,
                ),
            ),

            None,
            None,
        )
        .unwrap_or_default();


    SendMessageW(
        hwnd,

        WM_SETFONT,

        Some(
            WPARAM(
                font.0
                    as usize,
            ),
        ),

        Some(
            LPARAM(1),
        ),
    );


    hwnd
}


// ============================================================
// Label
// ============================================================

unsafe fn label(
    parent: HWND,
    text: &str,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    font: HFONT,
) {
    child(
        parent,
        WC_STATICW,
        text,
        WINDOW_STYLE(
            SS_LEFT.0,
        ),
        x,
        y,
        width,
        height,
        0,
        font,
    );
}

fn rgb_to_colorref(rgb: u32) -> COLORREF {
    let r = (rgb >> 16) & 0xFF;
    let g = (rgb >> 8) & 0xFF;
    let b = rgb & 0xFF;

    // Windows COLORREF = 0x00BBGGRR
    COLORREF(
        r
            | (g << 8)
            | (b << 16)
    )
}


fn colorref_to_rgb(color: COLORREF) -> u32 {
    let value = color.0;

    let r = value & 0xFF;
    let g = (value >> 8) & 0xFF;
    let b = (value >> 16) & 0xFF;

    // Sound Lock = 0xRRGGBB
    (r << 16)
        | (g << 8)
        | b
}
// ============================================================
// Slider Row
// ============================================================

unsafe fn slider_row(
    parent: HWND,
    text: &str,
    id: i32,
    value_id: i32,
    y: i32,
    font: HFONT,
) {
    label(
        parent,
        text,
        34,
        y + 7,
        78,
        24,
        font,
    );


    child(
        parent,
        TRACKBAR_CLASSW,
        "",
        WINDOW_STYLE(
            TBS_AUTOTICKS,
        )
            | WS_TABSTOP,
        112,
        y+9,
        212,
        32,
        id,
        font,
    );


    child(
        parent,
        WC_STATICW,
        "",
        WINDOW_STYLE(
            SS_RIGHT.0,
        ),
        330,
        y + 8,
        74,
        24,
        value_id,
        font,
    );
}


// ============================================================
// Group Box
// ============================================================

unsafe fn group_box(
    parent: HWND,
    text: &str,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    font: HFONT,
) {
    child(
        parent,
        WC_BUTTON,
        text,
        WINDOW_STYLE(
            BS_GROUPBOX
                as u32,
        ),
        x,
        y,
        width,
        height,
        0,
        font,
    );
}


// ============================================================
// Font
// ============================================================

unsafe fn create_ui_font(
    height: i32,
    weight: i32,
) -> HFONT {
    let font =
        CreateFontW(
            height,
            0,
            0,
            0,
            weight,

            0,
            0,
            0,

            DEFAULT_CHARSET,

            OUT_DEFAULT_PRECIS,

            CLIP_DEFAULT_PRECIS,

            CLEARTYPE_QUALITY,

            DEFAULT_PITCH.0
                as u32
                | FF_DONTCARE.0
                    as u32,

            w!(
                "Microsoft YaHei UI"
            ),
        );


    if font.is_invalid() {
        HFONT(
            GetStockObject(
                DEFAULT_GUI_FONT,
            )
                .0,
        )
    } else {
        font
    }
}


unsafe fn delete_owned_font(
    font: HFONT,
) {
    if !font.is_invalid()
        && font.0
            != GetStockObject(
                DEFAULT_GUI_FONT,
            )
                .0
    {
        let _ =
            DeleteObject(
                HGDIOBJ(
                    font.0,
                ),
            );
    }
}


// ============================================================
// ComboBox
// ============================================================

unsafe fn combo_add(
    hwnd: HWND,
    text: &str,
) {
    let wide =
        wide(text);


    SendMessageW(
        hwnd,
        CB_ADDSTRING,
        None,
        Some(
            LPARAM(
                wide
                    .as_ptr()
                    as isize,
            ),
        ),
    );
}


// ============================================================
// UTF-16
// ============================================================

fn wide(
    value: &str,
) -> Vec<u16> {
    value
        .encode_utf16()
        .chain(
            std::iter::once(
                0,
            ),
        )
        .collect()
}


// ============================================================
// MessageBox
// ============================================================

unsafe fn show_error(
    hwnd: HWND,
    message: &str,
) {
    let text =
        wide(message);


    MessageBoxW(
        Some(
            hwnd,
        ),

        PCWSTR(
            text.as_ptr(),
        ),

        w!(
            "Sound Lock"
        ),

        MB_OK
            | MB_ICONERROR,
    );
}


unsafe fn show_info(
    hwnd: HWND,
    message: &str,
) {
    let text =
        wide(message);


    MessageBoxW(
        Some(
            hwnd,
        ),

        PCWSTR(
            text.as_ptr(),
        ),

        w!(
            "Sound Lock"
        ),

        MB_OK
            | MB_ICONINFORMATION,
    );
}


// ============================================================
// Device Search
// ============================================================

fn find_device_index(
    devices:
        &[(Device, String)],
    id: &str,
) -> Option<usize> {
    devices
        .iter()
        .position(
            |(
                _,
                current,
            )| {
                current == id
            },
        )
}