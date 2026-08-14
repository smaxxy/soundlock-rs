use std::sync::atomic::AtomicBool;

// pub static WINDOW_VISIBLE: AtomicBool = AtomicBool::new(true);
pub static SHOULD_EXIT: AtomicBool = AtomicBool::new(false);