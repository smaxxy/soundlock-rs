use crate::config::{Config, RuntimeLimiterParams};
use crate::{audio, AppState};
use cpal::traits::{DeviceTrait, HostTrait};
use cpal::Device;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    DeleteObject, GetStockObject, UpdateWindow, COLOR_WINDOW, DEFAULT_GUI_FONT, HFONT, HGDIOBJ,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemServices::SS_LEFT;
use windows::Win32::UI::Controls::{
    InitCommonControls, BST_CHECKED, BST_UNCHECKED, TBM_SETPAGESIZE, TBM_SETPOS, TBM_SETRANGE,
    TBM_SETTICFREQ, TBS_AUTOTICKS, TRACKBAR_CLASSW, WC_BUTTON, WC_COMBOBOXW, WC_EDITW, WC_STATICW,
};
use windows::Win32::UI::WindowsAndMessaging::*;

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
const UI_TIMER: usize = 1;
const TBM_GETPOS: u32 = WM_USER;

pub struct SettingsWindow {
    state: Arc<Mutex<AppState>>,
    config: Arc<Mutex<Config>>,
    runtime_params: Arc<RuntimeLimiterParams>,
    input_devices: Vec<(Device, String)>,
    output_devices: Vec<(Device, String)>,
    hwnd: HWND,
    font: HFONT,
}

impl SettingsWindow {
    pub fn new(
        state: Arc<Mutex<AppState>>,
        config: Arc<Mutex<Config>>,
        runtime_params: Arc<RuntimeLimiterParams>,
    ) -> Self {
        Self {
            state,
            config,
            runtime_params,
            input_devices: Vec::new(),
            output_devices: Vec::new(),
            hwnd: HWND::default(),
            font: HFONT::default(),
        }
    }

    fn get_devices() -> (Vec<(Device, String)>, Vec<(Device, String)>) {
        let host = cpal::default_host();
        let input = host
            .input_devices()
            .map(|items| {
                items
                    .filter_map(|d| d.id().ok().map(|id| (d, id.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        let output = host
            .output_devices()
            .map(|items| {
                items
                    .filter_map(|d| d.id().ok().map(|id| (d, id.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        (input, output)
    }

    unsafe fn control(&self, id: i32) -> HWND {
        GetDlgItem(Some(self.hwnd), id).unwrap_or_default()
    }
    unsafe fn set_text(&self, id: i32, value: &str) {
        let value = wide(value);
        let _ = SetWindowTextW(self.control(id), PCWSTR(value.as_ptr()));
    }
    unsafe fn text(&self, id: i32) -> String {
        let hwnd = self.control(id);
        let mut buffer = vec![0u16; GetWindowTextLengthW(hwnd) as usize + 1];
        let used = GetWindowTextW(hwnd, &mut buffer);
        String::from_utf16_lossy(&buffer[..used as usize])
    }

    unsafe fn create_controls(&mut self) {
        self.font = HFONT(GetStockObject(DEFAULT_GUI_FONT).0);
        label(self.hwnd, "Sound Lock 全频降音", 18, 12, 340, 28, self.font);
        label(self.hwnd, "输入设备", 18, 52, 80, 24, self.font);
        child(
            self.hwnd,
            WC_COMBOBOXW,
            "",
            WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_VSCROLL,
            100,
            48,
            260,
            240,
            ID_INPUT,
            self.font,
        );
        label(self.hwnd, "输出设备", 18, 88, 80, 24, self.font);
        child(
            self.hwnd,
            WC_COMBOBOXW,
            "",
            WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_VSCROLL,
            100,
            84,
            260,
            240,
            ID_OUTPUT,
            self.font,
        );
        child(
            self.hwnd,
            WC_BUTTON,
            "刷新设备",
            WINDOW_STYLE(BS_PUSHBUTTON as u32),
            275,
            120,
            85,
            28,
            ID_REFRESH,
            self.font,
        );
        slider_row(
            self.hwnd,
            "RMS 阈值",
            ID_THRESHOLD,
            ID_THRESHOLD_VALUE,
            154,
            self.font,
        );
        slider_row(
            self.hwnd,
            "RMS 触发",
            ID_ATTACK,
            ID_ATTACK_VALUE,
            190,
            self.font,
        );
        slider_row(
            self.hwnd,
            "RMS 释放",
            ID_RELEASE,
            ID_RELEASE_VALUE,
            226,
            self.font,
        );
        slider_row(
            self.hwnd,
            "Pre-Gain",
            ID_PRE_GAIN,
            ID_PRE_GAIN_VALUE,
            262,
            self.font,
        );
        slider_row(
            self.hwnd,
            "Peak 释放",
            ID_PEAK_RELEASE,
            ID_PEAK_RELEASE_VALUE,
            298,
            self.font,
        );
        child(
            self.hwnd,
            WC_BUTTON,
            "开启屏幕准星",
            WINDOW_STYLE(BS_AUTOCHECKBOX as u32),
            18,
            338,
            135,
            26,
            ID_CROSSHAIR,
            self.font,
        );
        child(
            self.hwnd,
            WC_BUTTON,
            "切换颜色",
            WINDOW_STYLE(BS_PUSHBUTTON as u32),
            160,
            336,
            90,
            28,
            ID_COLOR,
            self.font,
        );
        label(self.hwnd, "大小(px)", 260, 340, 58, 24, self.font);
        child(
            self.hwnd,
            WC_EDITW,
            "",
            WS_BORDER | WINDOW_STYLE((ES_NUMBER | ES_AUTOHSCROLL) as u32),
            320,
            337,
            40,
            26,
            ID_SIZE,
            self.font,
        );
        child(
            self.hwnd,
            WC_BUTTON,
            "启动限制",
            WINDOW_STYLE(BS_PUSHBUTTON as u32),
            18,
            382,
            160,
            38,
            ID_START_STOP,
            self.font,
        );
        child(
            self.hwnd,
            WC_BUTTON,
            "保存设置",
            WINDOW_STYLE(BS_PUSHBUTTON as u32),
            200,
            382,
            160,
            38,
            ID_SAVE,
            self.font,
        );
        self.load_config_into_controls();
        self.refresh_devices();
        self.refresh_status();
    }

    unsafe fn load_config_into_controls(&self) {
        let cfg = match self.config.lock() {
            Ok(v) => v,
            Err(p) => p.into_inner(),
        };
        self.configure_slider(
            ID_THRESHOLD,
            0,
            600,
            ((cfg.threshold_db + 60.0) * 10.0).round() as u32,
            50,
            10,
        );
        self.configure_slider(ID_ATTACK, 1, 300, cfg.attack_ms, 25, 10);
        self.configure_slider(ID_RELEASE, 1, 300, cfg.release_ms, 25, 10);
        self.configure_slider(
            ID_PRE_GAIN,
            0,
            120,
            (cfg.pre_gain_db * 10.0).round() as u32,
            10,
            10,
        );
        self.configure_slider(ID_PEAK_RELEASE, 20, 150, cfg.peak_release_ms, 10, 10);
        self.update_slider_labels();
        self.set_text(
            ID_SIZE,
            &crate::tray_state::CROSSHAIR_SIZE
                .load(Ordering::SeqCst)
                .to_string(),
        );
        let checked = if crate::tray_state::CROSSHAIR_ENABLED.load(Ordering::SeqCst) {
            BST_CHECKED
        } else {
            BST_UNCHECKED
        };
        SendMessageW(
            self.control(ID_CROSSHAIR),
            BM_SETCHECK,
            Some(WPARAM(checked.0 as usize)),
            Some(LPARAM(0)),
        );
    }

    unsafe fn configure_slider(
        &self,
        id: i32,
        minimum: u16,
        maximum: u16,
        position: u32,
        tick_frequency: usize,
        page_size: usize,
    ) {
        let hwnd = self.control(id);
        let range = (minimum as u32) | ((maximum as u32) << 16);
        SendMessageW(
            hwnd,
            TBM_SETRANGE,
            Some(WPARAM(1)),
            Some(LPARAM(range as isize)),
        );
        SendMessageW(hwnd, TBM_SETTICFREQ, Some(WPARAM(tick_frequency)), None);
        SendMessageW(
            hwnd,
            TBM_SETPAGESIZE,
            None,
            Some(LPARAM(page_size as isize)),
        );
        SendMessageW(
            hwnd,
            TBM_SETPOS,
            Some(WPARAM(1)),
            Some(LPARAM(position as isize)),
        );
    }

    unsafe fn slider_position(&self, id: i32) -> u32 {
        SendMessageW(self.control(id), TBM_GETPOS, None, None)
            .0
            .max(0) as u32
    }

    unsafe fn update_slider_labels(&self) {
        let threshold = self.slider_position(ID_THRESHOLD) as f32 / 10.0 - 60.0;
        let pre_gain = self.slider_position(ID_PRE_GAIN) as f32 / 10.0;
        self.set_text(ID_THRESHOLD_VALUE, &format!("{threshold:.1} dB"));
        self.set_text(
            ID_ATTACK_VALUE,
            &format!("{} ms", self.slider_position(ID_ATTACK)),
        );
        self.set_text(
            ID_RELEASE_VALUE,
            &format!("{} ms", self.slider_position(ID_RELEASE)),
        );
        self.set_text(ID_PRE_GAIN_VALUE, &format!("+{pre_gain:.1} dB"));
        self.set_text(
            ID_PEAK_RELEASE_VALUE,
            &format!("{} ms", self.slider_position(ID_PEAK_RELEASE)),
        );
    }

    unsafe fn refresh_devices(&mut self) {
        let input = self.control(ID_INPUT);
        let output = self.control(ID_OUTPUT);
        SendMessageW(input, CB_RESETCONTENT, None, None);
        SendMessageW(output, CB_RESETCONTENT, None, None);
        let (inputs, outputs) = Self::get_devices();
        self.input_devices = inputs;
        self.output_devices = outputs;
        for (device, _) in &self.input_devices {
            combo_add(
                input,
                &device
                    .description()
                    .map(|d| d.to_string())
                    .unwrap_or_else(|_| "未知输入设备".to_owned()),
            );
        }
        for (device, _) in &self.output_devices {
            combo_add(
                output,
                &device
                    .description()
                    .map(|d| d.to_string())
                    .unwrap_or_else(|_| "未知输出设备".to_owned()),
            );
        }
        let cfg = match self.config.lock() {
            Ok(v) => v,
            Err(p) => p.into_inner(),
        };
        if let Some(i) = cfg
            .target_input_device_id
            .as_deref()
            .and_then(|id| find_device_index(&self.input_devices, id))
        {
            SendMessageW(input, CB_SETCURSEL, Some(WPARAM(i)), None);
        }
        if let Some(i) = cfg
            .target_output_device_id
            .as_deref()
            .and_then(|id| find_device_index(&self.output_devices, id))
        {
            SendMessageW(output, CB_SETCURSEL, Some(WPARAM(i)), None);
        }
    }

    unsafe fn apply_controls(&self, save: bool) -> Result<(), String> {
        let parse_u32 = |id, label: &str| {
            self.text(id)
                .trim()
                .parse::<u32>()
                .map_err(|_| format!("{label} 格式不正确"))
        };
        let crosshair_size = parse_u32(ID_SIZE, "准星大小")?.clamp(4, 60);
        crate::tray_state::CROSSHAIR_SIZE.store(crosshair_size, Ordering::SeqCst);

        let mut cfg = match self.config.lock() {
            Ok(v) => v,
            Err(p) => p.into_inner(),
        };
        cfg.threshold_db = self.slider_position(ID_THRESHOLD) as f32 / 10.0 - 60.0;
        cfg.attack_ms = self.slider_position(ID_ATTACK).clamp(1, 300);
        cfg.release_ms = self.slider_position(ID_RELEASE).clamp(1, 300);
        cfg.pre_gain_db = self.slider_position(ID_PRE_GAIN) as f32 / 10.0;
        cfg.peak_release_ms = self.slider_position(ID_PEAK_RELEASE).clamp(20, 150);
        let in_idx = SendMessageW(self.control(ID_INPUT), CB_GETCURSEL, None, None).0 as usize;
        let out_idx = SendMessageW(self.control(ID_OUTPUT), CB_GETCURSEL, None, None).0 as usize;
        if let Some((_, id)) = self.input_devices.get(in_idx) {
            cfg.target_input_device_id = Some(id.clone());
        }
        if let Some((_, id)) = self.output_devices.get(out_idx) {
            cfg.target_output_device_id = Some(id.clone());
        }
        cfg.crosshair_enabled = crate::tray_state::CROSSHAIR_ENABLED.load(Ordering::SeqCst);
        cfg.crosshair_color =
            crate::tray_state::CROSSHAIR_COLOR.load(Ordering::SeqCst) & 0x00FF_FFFF;
        cfg.crosshair_size = crosshair_size;
        self.runtime_params.publish_config(&cfg);
        if save {
            cfg.save().map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    unsafe fn start_or_stop(&self) {
        let limiting = match self.state.lock() {
            Ok(v) => v.is_limiting,
            Err(p) => p.into_inner().is_limiting,
        };
        if limiting {
            match self.state.lock() {
                Ok(mut v) => v.is_limiting = false,
                Err(p) => p.into_inner().is_limiting = false,
            }
        } else {
            if let Err(message) = self.apply_controls(true) {
                show_error(self.hwnd, &message);
                return;
            }
            let has_devices = {
                let cfg = match self.config.lock() {
                    Ok(v) => v,
                    Err(p) => p.into_inner(),
                };
                cfg.target_input_device_id.is_some() && cfg.target_output_device_id.is_some()
            };
            if !has_devices {
                show_error(self.hwnd, "请选择输入和输出设备");
                return;
            }
            match self.state.lock() {
                Ok(mut v) => v.is_limiting = true,
                Err(p) => p.into_inner().is_limiting = true,
            }
            audio::start_limiter(
                Arc::clone(&self.state),
                Arc::clone(&self.config),
                Arc::clone(&self.runtime_params),
            );
        }
        self.refresh_status();
    }

    unsafe fn cycle_crosshair_color(&self) {
        const COLORS: [u32; 6] = [0x0078FF, 0x00FF00, 0xFF0000, 0xFFFF00, 0xFFFFFF, 0xFF00FF];
        let current = crate::tray_state::CROSSHAIR_COLOR.load(Ordering::SeqCst);
        let next = COLORS
            .iter()
            .position(|v| *v == current)
            .map(|i| COLORS[(i + 1) % COLORS.len()])
            .unwrap_or(COLORS[0]);
        crate::tray_state::CROSSHAIR_COLOR.store(next, Ordering::SeqCst);
    }

    unsafe fn refresh_status(&self) {
        let limiting = match self.state.lock() {
            Ok(v) => v.is_limiting,
            Err(p) => p.into_inner().is_limiting,
        };
        self.set_text(ID_START_STOP, if limiting { "停止" } else { "启动限制" });
    }

    unsafe fn handle_command(&mut self, id: i32, code: u16) {
        match id {
            ID_REFRESH => self.refresh_devices(),
            ID_INPUT | ID_OUTPUT if code == CBN_SELCHANGE as u16 => {
                let _ = self.apply_controls(false);
            }
            ID_CROSSHAIR => {
                let checked = SendMessageW(self.control(ID_CROSSHAIR), BM_GETCHECK, None, None).0
                    == BST_CHECKED.0 as isize;
                crate::tray_state::CROSSHAIR_ENABLED.store(checked, Ordering::SeqCst);
            }
            ID_COLOR => self.cycle_crosshair_color(),
            ID_START_STOP => self.start_or_stop(),
            ID_SAVE => match self.apply_controls(true) {
                Ok(()) => show_info(self.hwnd, "设置已保存"),
                Err(e) => show_error(self.hwnd, &e),
            },
            _ => {}
        }
    }

    unsafe fn handle_slider_change(&self, control: HWND) {
        let id = GetDlgCtrlID(control);
        if matches!(
            id,
            ID_THRESHOLD | ID_ATTACK | ID_RELEASE | ID_PRE_GAIN | ID_PEAK_RELEASE
        ) {
            self.update_slider_labels();
            let _ = self.apply_controls(false);
        }
    }
}

impl Drop for SettingsWindow {
    fn drop(&mut self) {
        unsafe {
            if !self.font.is_invalid() && self.font.0 != GetStockObject(DEFAULT_GUI_FONT).0 {
                let _ = DeleteObject(HGDIOBJ(self.font.0));
            }
        }
    }
}

pub fn run_settings_window(window: SettingsWindow) -> windows::core::Result<()> {
    unsafe {
        InitCommonControls();
        let instance = GetModuleHandleW(None)?;
        let class_name = w!("SoundLockSettingsWindow");
        let icon = LoadIconW(Some(instance.into()), PCWSTR(1 as *const u16)).unwrap_or_default();
        let cursor = LoadCursorW(None, IDC_ARROW)?;
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(settings_wnd_proc),
            hInstance: instance.into(),
            hIcon: icon,
            hCursor: cursor,
            hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH((COLOR_WINDOW.0 + 1) as *mut _),
            lpszClassName: class_name,
            hIconSm: icon,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let raw = Box::into_raw(Box::new(window));
        let hwnd = CreateWindowExW(
            WS_EX_APPWINDOW,
            class_name,
            w!("Sound Lock Rust"),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            400,
            470,
            None,
            None,
            Some(instance.into()),
            Some(raw.cast()),
        )?;
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = UpdateWindow(hwnd);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        UnregisterClassW(class_name, Some(instance.into())).ok();
        Ok(())
    }
}

unsafe extern "system" fn settings_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_NCCREATE {
        let create = &*(lparam.0 as *const CREATESTRUCTW);
        let state = create.lpCreateParams as *mut SettingsWindow;
        (*state).hwnd = hwnd;
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
    }
    let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut SettingsWindow;
    match msg {
        WM_CREATE if !state.is_null() => {
            (*state).create_controls();
            SetTimer(Some(hwnd), UI_TIMER, 250, None);
            return LRESULT(0);
        }
        WM_COMMAND if !state.is_null() => {
            (*state).handle_command(
                (wparam.0 & 0xffff) as i32,
                ((wparam.0 >> 16) & 0xffff) as u16,
            );
            return LRESULT(0);
        }
        WM_HSCROLL if !state.is_null() && lparam.0 != 0 => {
            (*state).handle_slider_change(HWND(lparam.0 as *mut _));
            return LRESULT(0);
        }
        WM_TIMER if !state.is_null() => {
            (*state).refresh_status();
            return LRESULT(0);
        }
        WM_CLOSE if !state.is_null() => {
            let _ = (*state).apply_controls(true);
            DestroyWindow(hwnd).ok();
            return LRESULT(0);
        }
        WM_DESTROY => {
            KillTimer(Some(hwnd), UI_TIMER).ok();
            PostQuitMessage(0);
            return LRESULT(0);
        }
        WM_NCDESTROY if !state.is_null() => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            drop(Box::from_raw(state));
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

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
    let text = wide(text);
    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        class,
        PCWSTR(text.as_ptr()),
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | style,
        x,
        y,
        width,
        height,
        Some(parent),
        Some(HMENU(id as *mut _)),
        None,
        None,
    )
    .unwrap_or_default();
    SendMessageW(
        hwnd,
        WM_SETFONT,
        Some(WPARAM(font.0 as usize)),
        Some(LPARAM(1)),
    );
    hwnd
}
unsafe fn label(parent: HWND, text: &str, x: i32, y: i32, width: i32, height: i32, font: HFONT) {
    child(
        parent,
        WC_STATICW,
        text,
        WINDOW_STYLE(SS_LEFT.0),
        x,
        y,
        width,
        height,
        0,
        font,
    );
}
unsafe fn slider_row(parent: HWND, text: &str, id: i32, value_id: i32, y: i32, font: HFONT) {
    label(parent, text, 18, y + 8, 92, 24, font);
    child(
        parent,
        TRACKBAR_CLASSW,
        "",
        WINDOW_STYLE(TBS_AUTOTICKS),
        105,
        y,
        195,
        32,
        id,
        font,
    );
    child(
        parent,
        WC_STATICW,
        "",
        WINDOW_STYLE(SS_LEFT.0),
        302,
        y + 8,
        76,
        24,
        value_id,
        font,
    );
}
unsafe fn combo_add(hwnd: HWND, text: &str) {
    let wide = wide(text);
    SendMessageW(
        hwnd,
        CB_ADDSTRING,
        None,
        Some(LPARAM(wide.as_ptr() as isize)),
    );
}
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}
unsafe fn show_error(hwnd: HWND, message: &str) {
    let text = wide(message);
    MessageBoxW(
        Some(hwnd),
        PCWSTR(text.as_ptr()),
        w!("Sound Lock"),
        MB_OK | MB_ICONERROR,
    );
}
unsafe fn show_info(hwnd: HWND, message: &str) {
    let text = wide(message);
    MessageBoxW(
        Some(hwnd),
        PCWSTR(text.as_ptr()),
        w!("Sound Lock"),
        MB_OK | MB_ICONINFORMATION,
    );
}
fn find_device_index(devices: &[(Device, String)], id: &str) -> Option<usize> {
    devices.iter().position(|(_, current)| current == id)
}
