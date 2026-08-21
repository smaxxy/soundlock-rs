use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Default)]
pub struct Diagnostics {
    pub ui_ticks: AtomicU64,
    pub input_callbacks: AtomicU64,
    pub output_callbacks: AtomicU64,
    pub input_errors: AtomicU64,
    pub output_errors: AtomicU64,
    pub limiter_lock_misses: AtomicU64,
    pub ring_push_drops: AtomicU64,
    pub output_underruns: AtomicU64,

    pub processed_frames: AtomicU64,
    pub ceiling_hit_frames: AtomicU64,
    pub peak_limit_frames: AtomicU64,
    pub peak_hold_events: AtomicU64,

    // 用 milli-dB 保存，0 = 0.000 dB。
    // Limiter gain 只会 <= 1，所以记录值只会 <= 0 dB。
    pub peak_gain_min_mdb: AtomicI32,
    pub rms_gain_min_mdb: AtomicI32,
}

static GLOBAL: OnceLock<Arc<Diagnostics>> = OnceLock::new();
static LOGGER_STARTED: AtomicBool = AtomicBool::new(false);

fn global() -> &'static Arc<Diagnostics> {
    GLOBAL.get_or_init(|| Arc::new(Diagnostics::default()))
}

pub fn init() {
    let diagnostics = Arc::clone(global());

    if LOGGER_STARTED.swap(true, Ordering::AcqRel) {
        return;
    }

    let path = log_path();
    if let Some(parent) = path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            eprintln!("failed to create diagnostics directory: {e}");
        }
    }

    write_line(
        &path,
        &format!("[{}] diagnostics started", now_seconds()),
    );

    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(2));

        let processed_frames = diagnostics.processed_frames.load(Ordering::Relaxed);
        let ceiling_hit_frames = diagnostics.ceiling_hit_frames.load(Ordering::Relaxed);
        let peak_limit_frames = diagnostics.peak_limit_frames.load(Ordering::Relaxed);
        let peak_hold_events = diagnostics.peak_hold_events.load(Ordering::Relaxed);
        let peak_gain_min_db =
            diagnostics.peak_gain_min_mdb.load(Ordering::Relaxed) as f32 / 1000.0;
        let rms_gain_min_db =
            diagnostics.rms_gain_min_mdb.load(Ordering::Relaxed) as f32 / 1000.0;

        let line = format!(
            "[{}] ui_ticks={} input_callbacks={} output_callbacks={} input_errors={} output_errors={} limiter_lock_misses={} ring_push_drops={} output_underruns={} processed_frames={} ceiling_hit_frames={} peak_limit_frames={} peak_hold_events={} peak_gain_min_db={:.3} rms_gain_min_db={:.3}",
            now_seconds(),
            diagnostics.ui_ticks.load(Ordering::Relaxed),
            diagnostics.input_callbacks.load(Ordering::Relaxed),
            diagnostics.output_callbacks.load(Ordering::Relaxed),
            diagnostics.input_errors.load(Ordering::Relaxed),
            diagnostics.output_errors.load(Ordering::Relaxed),
            diagnostics.limiter_lock_misses.load(Ordering::Relaxed),
            diagnostics.ring_push_drops.load(Ordering::Relaxed),
            diagnostics.output_underruns.load(Ordering::Relaxed),
            processed_frames,
            ceiling_hit_frames,
            peak_limit_frames,
            peak_hold_events,
            peak_gain_min_db,
            rms_gain_min_db,
        );

        write_line(&path, &line);
    });
}

pub fn reset_limiter_stats() {
    let d = global();
    d.processed_frames.store(0, Ordering::Relaxed);
    d.ceiling_hit_frames.store(0, Ordering::Relaxed);
    d.peak_limit_frames.store(0, Ordering::Relaxed);
    d.peak_hold_events.store(0, Ordering::Relaxed);
    d.peak_gain_min_mdb.store(0, Ordering::Relaxed);
    d.rms_gain_min_mdb.store(0, Ordering::Relaxed);
}

#[inline]
pub fn ui_tick() {
    global().ui_ticks.fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub fn input_callback() {
    global().input_callbacks.fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub fn output_callback() {
    global().output_callbacks.fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub fn input_error() {
    global().input_errors.fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub fn output_error() {
    global().output_errors.fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub fn limiter_lock_miss() {
    global().limiter_lock_misses.fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub fn ring_push_drop() {
    global().ring_push_drops.fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub fn output_underrun() {
    global().output_underruns.fetch_add(1, Ordering::Relaxed);
}

pub fn limiter_batch(
    processed_frames: u64,
    ceiling_hit_frames: u64,
    peak_limit_frames: u64,
    peak_hold_events: u64,
    peak_gain_min: f32,
    rms_gain_min: f32,
) {
    let d = global();

    d.processed_frames
        .fetch_add(processed_frames, Ordering::Relaxed);
    d.ceiling_hit_frames
        .fetch_add(ceiling_hit_frames, Ordering::Relaxed);
    d.peak_limit_frames
        .fetch_add(peak_limit_frames, Ordering::Relaxed);
    d.peak_hold_events
        .fetch_add(peak_hold_events, Ordering::Relaxed);

    update_min_gain_db(&d.peak_gain_min_mdb, peak_gain_min);
    update_min_gain_db(&d.rms_gain_min_mdb, rms_gain_min);
}

fn update_min_gain_db(target: &AtomicI32, gain: f32) {
    let db = if !gain.is_finite() || gain <= 0.000001 {
        -120.0
    } else {
        (20.0 * gain.log10()).clamp(-120.0, 0.0)
    };

    let value_mdb = (db * 1000.0).round() as i32;
    let mut current = target.load(Ordering::Relaxed);

    while value_mdb < current {
        match target.compare_exchange_weak(
            current,
            value_mdb,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(actual) => current = actual,
        }
    }
}

fn log_path() -> PathBuf {
    if let Ok(appdata) = std::env::var("APPDATA") {
        PathBuf::from(appdata)
            .join("SoundLockRust")
            .join("soundlock_debug.log")
    } else {
        PathBuf::from("soundlock_debug.log")
    }
}

fn write_line(path: &PathBuf, line: &str) {
    match OpenOptions::new().create(true).append(true).open(path) {
        Ok(mut file) => {
            if let Err(e) = writeln!(file, "{line}") {
                eprintln!("failed to write diagnostics log: {e}");
            }
        }
        Err(e) => eprintln!("failed to open diagnostics log: {e}"),
    }
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}