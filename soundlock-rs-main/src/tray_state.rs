use std::sync::atomic::AtomicBool;

/// 整个程序退出标志。
///
/// 托盘点击“退出”后变成 true。
pub static SHOULD_EXIT: AtomicBool =
    AtomicBool::new(false);

/// 屏幕准星总开关。
///
/// true  = 允许显示
/// false = 完全关闭
///
/// 注意：
/// 即使这里是 true，按住鼠标右键时
/// crosshair.rs 仍然会临时隐藏准星。
pub static CROSSHAIR_ENABLED: AtomicBool =
    AtomicBool::new(false);