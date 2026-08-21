use std::sync::atomic::{AtomicBool, AtomicU32};

pub static SHOULD_EXIT: AtomicBool =
    AtomicBool::new(false);

pub static CROSSHAIR_ENABLED: AtomicBool =
    AtomicBool::new(false);

pub static SHOULD_SHOW_UI: AtomicBool =
    AtomicBool::new(false);
/// 准星颜色，格式：0x00RRGGBB
pub static CROSSHAIR_COLOR: AtomicU32 =
    AtomicU32::new(0x0078FF);

/// 准星大小，单位：像素
pub static CROSSHAIR_SIZE: AtomicU32 =
    AtomicU32::new(18);