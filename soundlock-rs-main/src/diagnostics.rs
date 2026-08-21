use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{
    AtomicBool, AtomicI32, AtomicU64, Ordering,
};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const LOG_INTERVAL: Duration = Duration::from_secs(2);
const LOG_MAX_BYTES: u64 = 5 * 1024 * 1024;
const LOG_BACKUP_COUNT: usize = 2;

/// ============================================================
/// Diagnostics
/// ============================================================
///
/// Audio Callback 原则：
/// - 不加锁
/// - 不格式化字符串
/// - 不写文件
/// - 只做 Relaxed Atomic 或 Limiter 本地批量统计
pub struct Diagnostics {
    // 基础 lifetime
    pub ui_ticks: AtomicU64,
    pub input_callbacks: AtomicU64,
    pub output_callbacks: AtomicU64,
    pub input_errors: AtomicU64,
    pub output_errors: AtomicU64,

    /// 旧架构兼容字段；新版实时路径正常应始终为 0。
    pub limiter_lock_misses: AtomicU64,

    /// Ring 满时丢弃的完整 Stereo Frame 数。
    pub ring_push_drops: AtomicU64,

    /// Output Callback 发现 Ring 空的 callback 次数。
    pub output_underruns: AtomicU64,

    /// Ring 漂移补偿计数 / 阈值，单位 Stereo Frame。
    pub ring_drift_low_corrections: AtomicU64,
    pub ring_drift_high_corrections: AtomicU64,
    pub ring_drift_low_watermark_frames: AtomicU64,
    pub ring_drift_high_watermark_frames: AtomicU64,

    // Audio Supervisor lifetime
    pub audio_session_starts: AtomicU64,
    pub audio_reconnect_attempts: AtomicU64,

    // 当前 Audio Session 信息
    pub audio_sample_rate_hz: AtomicU64,
    pub limiter_lookahead_ms_x1000: AtomicU64,

    // Limiter lifetime
    pub processed_frames: AtomicU64,
    pub ceiling_hit_frames: AtomicU64,
    pub peak_limit_frames: AtomicU64,
    pub rms_limit_frames: AtomicU64,
    pub peak_hold_events: AtomicU64,
    pub peak_gain_min_mdb: AtomicI32,
    pub rms_gain_min_mdb: AtomicI32,

    // Limiter 2 秒 Window
    window_processed_frames: AtomicU64,
    window_ceiling_hit_frames: AtomicU64,
    window_peak_limit_frames: AtomicU64,
    window_rms_limit_frames: AtomicU64,
    window_peak_hold_events: AtomicU64,
    window_peak_gain_min_mdb: AtomicI32,
    window_rms_gain_min_mdb: AtomicI32,

    // Ring 水位，单位 Stereo Frame
    pub ring_capacity_frames: AtomicU64,
    pub ring_target_frames: AtomicU64,
    pub ring_fill_current_frames: AtomicU64,
    ring_fill_window_min_frames: AtomicU64,
    ring_fill_window_max_frames: AtomicU64,
    ring_fill_window_sum_frames: AtomicU64,
    ring_fill_window_samples: AtomicU64,
}

impl Default for Diagnostics {
    fn default() -> Self {
        Self {
            ui_ticks: AtomicU64::new(0),
            input_callbacks: AtomicU64::new(0),
            output_callbacks: AtomicU64::new(0),
            input_errors: AtomicU64::new(0),
            output_errors: AtomicU64::new(0),
            limiter_lock_misses: AtomicU64::new(0),
            ring_push_drops: AtomicU64::new(0),
            output_underruns: AtomicU64::new(0),
            ring_drift_low_corrections: AtomicU64::new(0),
            ring_drift_high_corrections: AtomicU64::new(0),
            ring_drift_low_watermark_frames: AtomicU64::new(0),
            ring_drift_high_watermark_frames: AtomicU64::new(0),

            audio_session_starts: AtomicU64::new(0),
            audio_reconnect_attempts: AtomicU64::new(0),
            audio_sample_rate_hz: AtomicU64::new(0),
            limiter_lookahead_ms_x1000: AtomicU64::new(0),

            processed_frames: AtomicU64::new(0),
            ceiling_hit_frames: AtomicU64::new(0),
            peak_limit_frames: AtomicU64::new(0),
            rms_limit_frames: AtomicU64::new(0),
            peak_hold_events: AtomicU64::new(0),
            peak_gain_min_mdb: AtomicI32::new(0),
            rms_gain_min_mdb: AtomicI32::new(0),

            window_processed_frames: AtomicU64::new(0),
            window_ceiling_hit_frames: AtomicU64::new(0),
            window_peak_limit_frames: AtomicU64::new(0),
            window_rms_limit_frames: AtomicU64::new(0),
            window_peak_hold_events: AtomicU64::new(0),
            window_peak_gain_min_mdb: AtomicI32::new(0),
            window_rms_gain_min_mdb: AtomicI32::new(0),

            ring_capacity_frames: AtomicU64::new(0),
            ring_target_frames: AtomicU64::new(0),
            ring_fill_current_frames: AtomicU64::new(0),
            ring_fill_window_min_frames: AtomicU64::new(u64::MAX),
            ring_fill_window_max_frames: AtomicU64::new(0),
            ring_fill_window_sum_frames: AtomicU64::new(0),
            ring_fill_window_samples: AtomicU64::new(0),
        }
    }
}

static GLOBAL: OnceLock<Arc<Diagnostics>> = OnceLock::new();
static LOGGER_STARTED: AtomicBool = AtomicBool::new(false);

fn global() -> &'static Arc<Diagnostics> {
    GLOBAL.get_or_init(|| Arc::new(Diagnostics::default()))
}

/// ============================================================
/// Logger
/// ============================================================

pub fn init() {
    let diagnostics = Arc::clone(global());

    if LOGGER_STARTED.swap(true, Ordering::AcqRel) {
        return;
    }

    let path = log_path();

    if let Some(parent) = path.parent() {
        if let Err(error) = fs::create_dir_all(parent) {
            eprintln!("failed to create diagnostics directory: {error}");
        }
    }

    if let Err(error) = std::thread::Builder::new()
        .name("sound-lock-diagnostics".to_owned())
        .spawn(move || logger_loop(diagnostics, path))
    {
        LOGGER_STARTED.store(false, Ordering::Release);
        eprintln!("failed to spawn diagnostics logger: {error}");
    }
}

fn logger_loop(diagnostics: Arc<Diagnostics>, path: PathBuf) {
    let mut writer = match RotatingLogWriter::open(path.clone()) {
        Ok(writer) => writer,
        Err(error) => {
            eprintln!("failed to open diagnostics log: {error}");
            // 不让 logger 线程直接死亡：每个周期尝试重新打开。
            loop {
                std::thread::sleep(LOG_INTERVAL);
                match RotatingLogWriter::open(path.clone()) {
                    Ok(writer) => break writer,
                    Err(error) => eprintln!("failed to reopen diagnostics log: {error}"),
                }
            }
        }
    };

    if let Err(error) = writer.write_line(&format!(
        "[{}] diagnostics started",
        now_seconds()
    )) {
        eprintln!("failed to write diagnostics start line: {error}");
    }

    loop {
        std::thread::sleep(LOG_INTERVAL);
        let line = build_log_line(&diagnostics);

        if let Err(error) = writer.write_line(&line) {
            eprintln!("failed to write diagnostics log: {error}");

            if let Ok(reopened) = RotatingLogWriter::open(path.clone()) {
                writer = reopened;
            }
        }
    }
}

fn build_log_line(diagnostics: &Diagnostics) -> String {
    // Lifetime Limiter
    let processed_frames = diagnostics.processed_frames.load(Ordering::Relaxed);
    let ceiling_hit_frames = diagnostics.ceiling_hit_frames.load(Ordering::Relaxed);
    let peak_limit_frames = diagnostics.peak_limit_frames.load(Ordering::Relaxed);
    let rms_limit_frames = diagnostics.rms_limit_frames.load(Ordering::Relaxed);
    let peak_hold_events = diagnostics.peak_hold_events.load(Ordering::Relaxed);

    let peak_gain_min_db =
        diagnostics.peak_gain_min_mdb.load(Ordering::Relaxed) as f32 / 1000.0;
    let rms_gain_min_db =
        diagnostics.rms_gain_min_mdb.load(Ordering::Relaxed) as f32 / 1000.0;

    // 2 秒 Limiter Window
    let win_processed_frames =
        diagnostics.window_processed_frames.swap(0, Ordering::Relaxed);
    let win_ceiling_hit_frames =
        diagnostics.window_ceiling_hit_frames.swap(0, Ordering::Relaxed);
    let win_peak_limit_frames =
        diagnostics.window_peak_limit_frames.swap(0, Ordering::Relaxed);
    let win_rms_limit_frames =
        diagnostics.window_rms_limit_frames.swap(0, Ordering::Relaxed);
    let win_peak_hold_events =
        diagnostics.window_peak_hold_events.swap(0, Ordering::Relaxed);

    let win_peak_gain_min_db =
        diagnostics.window_peak_gain_min_mdb.swap(0, Ordering::Relaxed) as f32 / 1000.0;
    let win_rms_gain_min_db =
        diagnostics.window_rms_gain_min_mdb.swap(0, Ordering::Relaxed) as f32 / 1000.0;

    let win_peak_limit_pct = percent(win_peak_limit_frames, win_processed_frames);
    let win_rms_limit_pct = percent(win_rms_limit_frames, win_processed_frames);
    let win_ceiling_hit_pct = percent(win_ceiling_hit_frames, win_processed_frames);

    // Ring 当前值 / Window
    let ring_capacity_frames = diagnostics.ring_capacity_frames.load(Ordering::Relaxed);
    let ring_target_frames = diagnostics.ring_target_frames.load(Ordering::Relaxed);
    let ring_fill_current = diagnostics.ring_fill_current_frames.load(Ordering::Relaxed);
    let ring_drift_low_corr =
        diagnostics.ring_drift_low_corrections.load(Ordering::Relaxed);
    let ring_drift_high_corr =
        diagnostics.ring_drift_high_corrections.load(Ordering::Relaxed);
    let ring_drift_low_wm =
        diagnostics.ring_drift_low_watermark_frames.load(Ordering::Relaxed);
    let ring_drift_high_wm =
        diagnostics.ring_drift_high_watermark_frames.load(Ordering::Relaxed);

    let ring_fill_min_raw = diagnostics
        .ring_fill_window_min_frames
        .swap(u64::MAX, Ordering::Relaxed);
    let ring_fill_max_raw = diagnostics
        .ring_fill_window_max_frames
        .swap(0, Ordering::Relaxed);
    let ring_fill_sum = diagnostics
        .ring_fill_window_sum_frames
        .swap(0, Ordering::Relaxed);
    let ring_fill_samples = diagnostics
        .ring_fill_window_samples
        .swap(0, Ordering::Relaxed);

    let (ring_fill_min, ring_fill_max, ring_fill_avg) = if ring_fill_samples == 0 {
        (
            ring_fill_current,
            ring_fill_current,
            ring_fill_current as f64,
        )
    } else {
        (
            if ring_fill_min_raw == u64::MAX {
                ring_fill_current
            } else {
                ring_fill_min_raw
            },
            ring_fill_max_raw,
            ring_fill_sum as f64 / ring_fill_samples as f64,
        )
    };

    let ring_fill_pct = if ring_capacity_frames > 0 {
        ring_fill_current as f64 * 100.0 / ring_capacity_frames as f64
    } else {
        0.0
    };

    // 软件链路延迟估算：只包含 Ring 当前占用 + Limiter Lookahead。
    // 不包含 capture driver / playback driver / OS / DAC 的物理延迟。
    let sample_rate_hz = diagnostics.audio_sample_rate_hz.load(Ordering::Relaxed);
    let lookahead_ms =
        diagnostics.limiter_lookahead_ms_x1000.load(Ordering::Relaxed) as f64 / 1000.0;

    let ring_fill_current_ms = if sample_rate_hz > 0 {
        ring_fill_current as f64 * 1000.0 / sample_rate_hz as f64
    } else {
        0.0
    };

    let software_latency_est_ms = if sample_rate_hz > 0 {
        ring_fill_current_ms + lookahead_ms
    } else {
        0.0
    };

    format!(
        "[{}] \
ui_ticks={} input_callbacks={} output_callbacks={} input_errors={} output_errors={} \
limiter_lock_misses={} ring_push_drops={} output_underruns={} \
ring_drift_low_corr={} ring_drift_high_corr={} ring_drift_low_wm={} ring_drift_high_wm={} \
audio_session_starts={} audio_reconnect_attempts={} sample_rate_hz={} \
processed_frames={} ceiling_hit_frames={} peak_limit_frames={} rms_limit_frames={} peak_hold_events={} \
peak_gain_min_db={:.3} rms_gain_min_db={:.3} \
win_processed_frames={} win_ceiling_hit_frames={} win_peak_limit_frames={} win_rms_limit_frames={} win_peak_hold_events={} \
win_peak_gain_min_db={:.3} win_rms_gain_min_db={:.3} \
win_ceiling_hit_pct={:.3} win_peak_limit_pct={:.3} win_rms_limit_pct={:.3} \
ring_capacity_frames={} ring_target_frames={} ring_fill_current={} ring_fill_min={} ring_fill_max={} ring_fill_avg={:.1} ring_fill_pct={:.1} \
ring_fill_current_ms={:.2} limiter_lookahead_ms={:.2} software_latency_est_ms={:.2}",
        now_seconds(),
        diagnostics.ui_ticks.load(Ordering::Relaxed),
        diagnostics.input_callbacks.load(Ordering::Relaxed),
        diagnostics.output_callbacks.load(Ordering::Relaxed),
        diagnostics.input_errors.load(Ordering::Relaxed),
        diagnostics.output_errors.load(Ordering::Relaxed),
        diagnostics.limiter_lock_misses.load(Ordering::Relaxed),
        diagnostics.ring_push_drops.load(Ordering::Relaxed),
        diagnostics.output_underruns.load(Ordering::Relaxed),
        ring_drift_low_corr,
        ring_drift_high_corr,
        ring_drift_low_wm,
        ring_drift_high_wm,
        diagnostics.audio_session_starts.load(Ordering::Relaxed),
        diagnostics.audio_reconnect_attempts.load(Ordering::Relaxed),
        sample_rate_hz,
        processed_frames,
        ceiling_hit_frames,
        peak_limit_frames,
        rms_limit_frames,
        peak_hold_events,
        peak_gain_min_db,
        rms_gain_min_db,
        win_processed_frames,
        win_ceiling_hit_frames,
        win_peak_limit_frames,
        win_rms_limit_frames,
        win_peak_hold_events,
        win_peak_gain_min_db,
        win_rms_gain_min_db,
        win_ceiling_hit_pct,
        win_peak_limit_pct,
        win_rms_limit_pct,
        ring_capacity_frames,
        ring_target_frames,
        ring_fill_current,
        ring_fill_min,
        ring_fill_max,
        ring_fill_avg,
        ring_fill_pct,
        ring_fill_current_ms,
        lookahead_ms,
        software_latency_est_ms,
    )
}

/// ============================================================
/// Session / reset API
/// ============================================================

pub fn reset_limiter_stats() {
    let d = global();

    d.processed_frames.store(0, Ordering::Relaxed);
    d.ceiling_hit_frames.store(0, Ordering::Relaxed);
    d.peak_limit_frames.store(0, Ordering::Relaxed);
    d.rms_limit_frames.store(0, Ordering::Relaxed);
    d.peak_hold_events.store(0, Ordering::Relaxed);
    d.peak_gain_min_mdb.store(0, Ordering::Relaxed);
    d.rms_gain_min_mdb.store(0, Ordering::Relaxed);

    d.audio_session_starts.store(0, Ordering::Relaxed);
    d.audio_reconnect_attempts.store(0, Ordering::Relaxed);
    d.ring_drift_low_corrections.store(0, Ordering::Relaxed);
    d.ring_drift_high_corrections.store(0, Ordering::Relaxed);

    reset_limiter_window(d);
}

fn reset_limiter_window(d: &Diagnostics) {
    d.window_processed_frames.store(0, Ordering::Relaxed);
    d.window_ceiling_hit_frames.store(0, Ordering::Relaxed);
    d.window_peak_limit_frames.store(0, Ordering::Relaxed);
    d.window_rms_limit_frames.store(0, Ordering::Relaxed);
    d.window_peak_hold_events.store(0, Ordering::Relaxed);
    d.window_peak_gain_min_mdb.store(0, Ordering::Relaxed);
    d.window_rms_gain_min_mdb.store(0, Ordering::Relaxed);
}

pub fn configure_audio_monitor(sample_rate_hz: f32, lookahead_ms: f32) {
    let d = global();

    let sample_rate = if sample_rate_hz.is_finite() && sample_rate_hz > 0.0 {
        sample_rate_hz.round() as u64
    } else {
        0
    };

    let lookahead_x1000 = if lookahead_ms.is_finite() && lookahead_ms > 0.0 {
        (lookahead_ms * 1000.0).round() as u64
    } else {
        0
    };

    d.audio_sample_rate_hz.store(sample_rate, Ordering::Relaxed);
    d.limiter_lookahead_ms_x1000
        .store(lookahead_x1000, Ordering::Relaxed);
}

#[inline]
pub fn audio_session_started() {
    global()
        .audio_session_starts
        .fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub fn audio_reconnect_attempt() {
    global()
        .audio_reconnect_attempts
        .fetch_add(1, Ordering::Relaxed);
}

/// ============================================================
/// Ring monitor
/// ============================================================

pub fn configure_ring_monitor(target_frames: usize, capacity_frames: usize) {
    let d = global();
    let target_frames = target_frames as u64;
    let capacity_frames = capacity_frames as u64;

    d.ring_target_frames.store(target_frames, Ordering::Relaxed);
    d.ring_capacity_frames.store(capacity_frames, Ordering::Relaxed);
    d.ring_fill_current_frames
        .store(target_frames, Ordering::Relaxed);

    d.ring_fill_window_min_frames
        .store(u64::MAX, Ordering::Relaxed);
    d.ring_fill_window_max_frames.store(0, Ordering::Relaxed);
    d.ring_fill_window_sum_frames.store(0, Ordering::Relaxed);
    d.ring_fill_window_samples.store(0, Ordering::Relaxed);
}

pub fn configure_ring_drift_monitor(
    low_watermark_frames: usize,
    high_watermark_frames: usize,
) {
    let d = global();
    d.ring_drift_low_watermark_frames
        .store(low_watermark_frames as u64, Ordering::Relaxed);
    d.ring_drift_high_watermark_frames
        .store(high_watermark_frames as u64, Ordering::Relaxed);
}

#[inline]
pub fn ring_fill_sample(fill_frames: usize) {
    let d = global();
    let fill_frames = fill_frames as u64;

    d.ring_fill_current_frames
        .store(fill_frames, Ordering::Relaxed);

    update_min_u64(&d.ring_fill_window_min_frames, fill_frames);
    update_max_u64(&d.ring_fill_window_max_frames, fill_frames);

    d.ring_fill_window_sum_frames
        .fetch_add(fill_frames, Ordering::Relaxed);
    d.ring_fill_window_samples.fetch_add(1, Ordering::Relaxed);
}

/// ============================================================
/// 基础 Diagnostics API
/// ============================================================

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
    global()
        .limiter_lock_misses
        .fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub fn ring_push_drop() {
    global().ring_push_drops.fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub fn output_underrun() {
    global()
        .output_underruns
        .fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub fn ring_drift_low_correction() {
    global()
        .ring_drift_low_corrections
        .fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub fn ring_drift_high_correction() {
    global()
        .ring_drift_high_corrections
        .fetch_add(1, Ordering::Relaxed);
}

/// ============================================================
/// Limiter batch
/// ============================================================

pub fn limiter_batch(
    processed_frames: u64,
    ceiling_hit_frames: u64,
    peak_limit_frames: u64,
    rms_limit_frames: u64,
    peak_hold_events: u64,
    peak_gain_min: f32,
    rms_gain_min: f32,
) {
    let d = global();

    // Lifetime
    d.processed_frames
        .fetch_add(processed_frames, Ordering::Relaxed);
    d.ceiling_hit_frames
        .fetch_add(ceiling_hit_frames, Ordering::Relaxed);
    d.peak_limit_frames
        .fetch_add(peak_limit_frames, Ordering::Relaxed);
    d.rms_limit_frames
        .fetch_add(rms_limit_frames, Ordering::Relaxed);
    d.peak_hold_events
        .fetch_add(peak_hold_events, Ordering::Relaxed);

    update_min_gain_db(&d.peak_gain_min_mdb, peak_gain_min);
    update_min_gain_db(&d.rms_gain_min_mdb, rms_gain_min);

    // 2 秒 Window
    d.window_processed_frames
        .fetch_add(processed_frames, Ordering::Relaxed);
    d.window_ceiling_hit_frames
        .fetch_add(ceiling_hit_frames, Ordering::Relaxed);
    d.window_peak_limit_frames
        .fetch_add(peak_limit_frames, Ordering::Relaxed);
    d.window_rms_limit_frames
        .fetch_add(rms_limit_frames, Ordering::Relaxed);
    d.window_peak_hold_events
        .fetch_add(peak_hold_events, Ordering::Relaxed);

    update_min_gain_db(&d.window_peak_gain_min_mdb, peak_gain_min);
    update_min_gain_db(&d.window_rms_gain_min_mdb, rms_gain_min);
}

/// ============================================================
/// Helpers
/// ============================================================

#[inline]
fn percent(part: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        part as f64 * 100.0 / total as f64
    }
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

#[inline]
fn update_min_u64(target: &AtomicU64, value: u64) {
    let mut current = target.load(Ordering::Relaxed);

    while value < current {
        match target.compare_exchange_weak(
            current,
            value,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(actual) => current = actual,
        }
    }
}

#[inline]
fn update_max_u64(target: &AtomicU64, value: u64) {
    let mut current = target.load(Ordering::Relaxed);

    while value > current {
        match target.compare_exchange_weak(
            current,
            value,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(actual) => current = actual,
        }
    }
}

/// ============================================================
/// Rotating log writer
/// ============================================================

struct RotatingLogWriter {
    path: PathBuf,
    writer: Option<BufWriter<File>>,
    bytes_written: u64,
}

impl RotatingLogWriter {
    fn open(path: PathBuf) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;

        let bytes_written = file.metadata().map(|m| m.len()).unwrap_or(0);

        let mut this = Self {
            path,
            writer: Some(BufWriter::new(file)),
            bytes_written,
        };

        if this.bytes_written >= LOG_MAX_BYTES {
            this.rotate()?;
        }

        Ok(this)
    }

    fn write_line(&mut self, line: &str) -> io::Result<()> {
        let incoming = line.len() as u64 + 1;

        if self.bytes_written.saturating_add(incoming) > LOG_MAX_BYTES {
            self.rotate()?;
        }

        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "log writer closed"))?;

        writer.write_all(line.as_bytes())?;
        writer.write_all(b"\n")?;
        // 每 2 秒只有一行；flush 能保证崩溃前日志尽量落盘，且不需要反复 open/close。
        writer.flush()?;
        self.bytes_written = self.bytes_written.saturating_add(incoming);
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        if let Some(mut writer) = self.writer.take() {
            writer.flush()?;
        }

        // .1 是最新备份，.2 是更旧备份。
        for index in (1..=LOG_BACKUP_COUNT).rev() {
            let source = if index == 1 {
                self.path.clone()
            } else {
                rotated_path(&self.path, index - 1)
            };

            let destination = rotated_path(&self.path, index);

            if destination.exists() {
                let _ = fs::remove_file(&destination);
            }

            if source.exists() {
                fs::rename(&source, &destination)?;
            }
        }

        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.path)?;

        self.writer = Some(BufWriter::new(file));
        self.bytes_written = 0;
        Ok(())
    }
}

fn rotated_path(path: &Path, index: usize) -> PathBuf {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("soundlock_debug");
    let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("log");

    path.with_file_name(format!("{stem}.{index}.{extension}"))
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

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}