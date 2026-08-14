use std::fs::{create_dir_all, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, OnceLock,
};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub struct Diagnostics {
    pub ui_ticks: AtomicU64,
    pub input_callbacks: AtomicU64,
    pub output_callbacks: AtomicU64,

    pub input_errors: AtomicU64,
    pub output_errors: AtomicU64,

    pub limiter_lock_misses: AtomicU64,
    pub ring_push_drops: AtomicU64,
    pub output_underruns: AtomicU64,
}

static DIAGNOSTICS: OnceLock<Arc<Diagnostics>> = OnceLock::new();

fn create_diagnostics() -> Arc<Diagnostics> {
    Arc::new(Diagnostics {
        ui_ticks: AtomicU64::new(0),
        input_callbacks: AtomicU64::new(0),
        output_callbacks: AtomicU64::new(0),

        input_errors: AtomicU64::new(0),
        output_errors: AtomicU64::new(0),

        limiter_lock_misses: AtomicU64::new(0),
        ring_push_drops: AtomicU64::new(0),
        output_underruns: AtomicU64::new(0),
    })
}

fn log_path() -> PathBuf {
    if let Some(mut path) = dirs::config_dir() {
        path.push("SoundLockRust");

        if let Err(e) = create_dir_all(&path) {
            eprintln!("创建诊断日志目录失败: {}", e);
        }

        path.push("soundlock_debug.log");
        return path;
    }

    let mut path = std::env::current_dir().unwrap_or_default();
    path.push("soundlock_debug.log");
    path
}

/// 启动诊断线程。
/// 整个程序只需要调用一次。
pub fn init() {
    let diagnostics = DIAGNOSTICS
        .get_or_init(create_diagnostics)
        .clone();

    let path = log_path();

    thread::spawn(move || {
        let mut file = match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            Ok(file) => file,
            Err(err) => {
                eprintln!("无法创建诊断日志 {:?}: {}", path, err);
                return;
            }
        };

        write_line(
            &mut file,
            &format!(
                "\n========== Sound Lock diagnostic session {} ==========",
                now_seconds()
            ),
        );

        loop {
            thread::sleep(Duration::from_secs(2));

            let line = format!(
                "[{}] ui_ticks={} input_callbacks={} output_callbacks={} \
input_errors={} output_errors={} limiter_lock_misses={} \
ring_push_drops={} output_underruns={}",
                now_seconds(),
                diagnostics.ui_ticks.load(Ordering::Relaxed),
                diagnostics.input_callbacks.load(Ordering::Relaxed),
                diagnostics.output_callbacks.load(Ordering::Relaxed),
                diagnostics.input_errors.load(Ordering::Relaxed),
                diagnostics.output_errors.load(Ordering::Relaxed),
                diagnostics
                    .limiter_lock_misses
                    .load(Ordering::Relaxed),
                diagnostics.ring_push_drops.load(Ordering::Relaxed),
                diagnostics
                    .output_underruns
                    .load(Ordering::Relaxed),
            );

            write_line(&mut file, &line);
        }
    });
}

fn write_line(file: &mut std::fs::File, line: &str) {
    if let Err(e) = writeln!(file, "{}", line) {
        eprintln!("写入诊断日志失败: {}", e);
        return;
    }

    if let Err(e) = file.flush() {
        eprintln!("刷新诊断日志失败: {}", e);
    }
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn global() -> &'static Arc<Diagnostics> {
    DIAGNOSTICS.get_or_init(create_diagnostics)
}

pub fn ui_tick() {
    global().ui_ticks.fetch_add(1, Ordering::Relaxed);
}

pub fn input_callback() {
    global()
        .input_callbacks
        .fetch_add(1, Ordering::Relaxed);
}

pub fn output_callback() {
    global()
        .output_callbacks
        .fetch_add(1, Ordering::Relaxed);
}

pub fn input_error() {
    global()
        .input_errors
        .fetch_add(1, Ordering::Relaxed);
}

pub fn output_error() {
    global()
        .output_errors
        .fetch_add(1, Ordering::Relaxed);
}

pub fn limiter_lock_miss() {
    global()
        .limiter_lock_misses
        .fetch_add(1, Ordering::Relaxed);
}

pub fn ring_push_drop() {
    global()
        .ring_push_drops
        .fetch_add(1, Ordering::Relaxed);
}

pub fn output_underrun() {
    global()
        .output_underruns
        .fetch_add(1, Ordering::Relaxed);
}