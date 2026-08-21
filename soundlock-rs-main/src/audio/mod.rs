pub mod limiter;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::StreamConfig;

pub use limiter::{LoudnessLimiter, LOOKAHEAD_MS};

use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::HeapRb;

use crate::config::{Config, RuntimeLimiterParams};
use crate::AppState;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const TARGET_RING_LATENCY_MS: f32 = 150.0;
const RECONNECT_FADE_MS: f32 = 10.0;
const CONTROL_POLL: Duration = Duration::from_millis(50);
const RECONNECT_BACKOFF_MIN_MS: u64 = 250;
const RECONNECT_BACKOFF_MAX_MS: u64 = 2000;

/// 每次用户点击“启动”都会生成新 generation。
/// 旧 Supervisor 看到 generation 不匹配后会退出，避免快速停止/重启产生双 Stream。
static AUDIO_GENERATION: AtomicU64 = AtomicU64::new(0);
static ACTIVE_AUDIO_THREADS: AtomicU64 = AtomicU64::new(0);

/// 只串行化 Supervisor / Stream 生命周期，不进入任何实时 callback。
/// 保证快速“停止 -> 启动”时旧 Stream 完整销毁后才创建新 Stream。
static AUDIO_SESSION_LOCK: Mutex<()> = Mutex::new(());

struct AudioThreadGuard;

impl Drop for AudioThreadGuard {
    fn drop(&mut self) {
        ACTIVE_AUDIO_THREADS.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Supervisor 结束时只有在自己仍是“当前 generation”时才恢复 UI 状态。
/// 这样旧线程退出不会把刚启动的新 Session 的 is_limiting 错误清成 false。
struct LimitingStateGuard {
    state: Arc<Mutex<AppState>>,
    generation: u64,
}

impl LimitingStateGuard {
    fn new(state: Arc<Mutex<AppState>>, generation: u64) -> Self {
        Self { state, generation }
    }
}

impl Drop for LimitingStateGuard {
    fn drop(&mut self) {
        if AUDIO_GENERATION.load(Ordering::Acquire) == self.generation {
            set_limiting_state(&self.state, false);
        }
    }
}

/// 每个 Audio Session 结束时清掉 Ring/延迟监控，避免日志保留旧 Session 水位。
struct SessionMonitorGuard {
    generation: u64,
}

impl SessionMonitorGuard {
    fn new(generation: u64) -> Self {
        Self { generation }
    }
}

impl Drop for SessionMonitorGuard {
    fn drop(&mut self) {
        // 快速重启时，旧 generation 不能把新 Session 的监控清零。
        if AUDIO_GENERATION.load(Ordering::Acquire) == self.generation {
            crate::diagnostics::configure_ring_monitor(0, 0);
            crate::diagnostics::configure_audio_monitor(0.0, 0.0);
        }
    }
}

enum SessionExit {
    StopRequested,
    Retry {
        reason: String,
        ran_for: Duration,
    },
}

/// 普通控制线程使用；不是实时 Audio Callback，可以正常阻塞等待 Mutex。
fn set_limiting_state(state: &Arc<Mutex<AppState>>, value: bool) {
    match state.lock() {
        Ok(mut state) => state.is_limiting = value,
        Err(poisoned) => {
            log::warn!("AppState mutex poisoned; recovering state");
            poisoned.into_inner().is_limiting = value;
        }
    }
}

fn is_requested_to_run(state: &Arc<Mutex<AppState>>, generation: u64) -> bool {
    if crate::tray_state::SHOULD_EXIT.load(Ordering::Acquire) {
        return false;
    }

    if AUDIO_GENERATION.load(Ordering::Acquire) != generation {
        return false;
    }

    match state.lock() {
        Ok(state) => state.is_limiting,
        Err(poisoned) => {
            log::warn!("AppState mutex poisoned; recovering value");
            poisoned.into_inner().is_limiting
        }
    }
}

/// ============================================================
/// Public lifecycle API
/// ============================================================

pub fn start_limiter(
    state: Arc<Mutex<AppState>>,
    config: Arc<Mutex<Config>>,
    runtime_params: Arc<RuntimeLimiterParams>,
) {
    let generation = AUDIO_GENERATION
        .fetch_add(1, Ordering::AcqRel)
        .wrapping_add(1);

    let state_on_spawn_failure = Arc::clone(&state);

    // 先计数，spawn 失败再回滚，保证 main 的 shutdown wait 不漏线程。
    ACTIVE_AUDIO_THREADS.fetch_add(1, Ordering::AcqRel);

    let spawn_result = std::thread::Builder::new()
        .name(format!("sound-lock-audio-{generation}"))
        .spawn(move || {
            let _thread_guard = AudioThreadGuard;
            audio_supervisor(state, config, runtime_params, generation);
        });

    if let Err(error) = spawn_result {
        ACTIVE_AUDIO_THREADS.fetch_sub(1, Ordering::AcqRel);
        log::error!("Failed to spawn audio supervisor: {}", error);

        if AUDIO_GENERATION.load(Ordering::Acquire) == generation {
            set_limiting_state(&state_on_spawn_failure, false);
        }
    }
}

/// 主程序退出时等待 Audio Supervisor 自己观察 SHOULD_EXIT 并退出。
/// 不强杀实时线程，不持有任何 Audio Callback 锁。
pub fn wait_for_shutdown(timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;

    while ACTIVE_AUDIO_THREADS.load(Ordering::Acquire) != 0 {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    true
}

/// ============================================================
/// Audio Supervisor
/// ============================================================

fn audio_supervisor(
    state: Arc<Mutex<AppState>>,
    config: Arc<Mutex<Config>>,
    runtime_params: Arc<RuntimeLimiterParams>,
    generation: u64,
) {
    let _state_guard = LimitingStateGuard::new(Arc::clone(&state), generation);

    // 非实时生命周期锁：任何时刻最多只有一个 Supervisor 可以拥有 Stream。
    // 新 generation 会先让旧 Supervisor 退出，然后在这里接管。
    let _session_guard = match AUDIO_SESSION_LOCK.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            log::warn!("Audio session lifecycle mutex poisoned; recovering");
            poisoned.into_inner()
        }
    };

    // 等锁期间用户可能已经再次停止，先重新检查。
    if !is_requested_to_run(&state, generation) {
        return;
    }

    let mut backoff_ms = RECONNECT_BACKOFF_MIN_MS;

    loop {
        if !is_requested_to_run(&state, generation) {
            break;
        }

        match run_limiter_session(
            &state,
            &config,
            Arc::clone(&runtime_params),
            generation,
        ) {
            SessionExit::StopRequested => break,

            SessionExit::Retry { reason, ran_for } => {
                if !is_requested_to_run(&state, generation) {
                    break;
                }

                crate::diagnostics::audio_reconnect_attempt();

                // 如果上一 Session 已稳定运行较长时间，说明更像一次临时掉线，
                // 下一次从最短 250ms 重连开始。
                if ran_for >= Duration::from_secs(10) {
                    backoff_ms = RECONNECT_BACKOFF_MIN_MS;
                }

                log::warn!(
                    "Audio session unavailable: {}; retrying in {} ms",
                    reason,
                    backoff_ms
                );

                if !interruptible_sleep(
                    &state,
                    generation,
                    Duration::from_millis(backoff_ms),
                ) {
                    break;
                }

                backoff_ms = (backoff_ms.saturating_mul(2))
                    .min(RECONNECT_BACKOFF_MAX_MS);
            }
        }
    }
}

fn interruptible_sleep(
    state: &Arc<Mutex<AppState>>,
    generation: u64,
    duration: Duration,
) -> bool {
    let deadline = Instant::now() + duration;

    while Instant::now() < deadline {
        if !is_requested_to_run(state, generation) {
            return false;
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        std::thread::sleep(remaining.min(CONTROL_POLL));
    }

    is_requested_to_run(state, generation)
}

/// ============================================================
/// 单次 Audio Session
/// ============================================================

fn run_limiter_session(
    state: &Arc<Mutex<AppState>>,
    config: &Arc<Mutex<Config>>,
    runtime_params: Arc<RuntimeLimiterParams>,
    generation: u64,
) -> SessionExit {
    if !is_requested_to_run(state, generation) {
        return SessionExit::StopRequested;
    }

    // 初始化线程不是实时 callback，可以阻塞读取 Config。
    let (input_device_id, output_device_id) = match config.lock() {
        Ok(cfg) => (
            cfg.target_input_device_id.clone(),
            cfg.target_output_device_id.clone(),
        ),
        Err(poisoned) => {
            log::warn!("Config mutex poisoned while starting audio; recovering value");
            let cfg = poisoned.into_inner();
            (
                cfg.target_input_device_id.clone(),
                cfg.target_output_device_id.clone(),
            )
        }
    };

    let input_device_id = match input_device_id {
        Some(id) => id,
        None => {
            return SessionExit::Retry {
                reason: "no input device selected".to_owned(),
                ran_for: Duration::ZERO,
            }
        }
    };

    let output_device_id = match output_device_id {
        Some(id) => id,
        None => {
            return SessionExit::Retry {
                reason: "no output device selected".to_owned(),
                ran_for: Duration::ZERO,
            }
        }
    };

    let host = cpal::default_host();

    let input_devices = match host.input_devices() {
        Ok(devices) => devices,
        Err(error) => {
            return SessionExit::Retry {
                reason: format!("failed to enumerate input devices: {error}"),
                ran_for: Duration::ZERO,
            }
        }
    };

    let output_devices = match host.output_devices() {
        Ok(devices) => devices,
        Err(error) => {
            return SessionExit::Retry {
                reason: format!("failed to enumerate output devices: {error}"),
                ran_for: Duration::ZERO,
            }
        }
    };

    let input_device = match input_devices.into_iter().find(|device| {
        device
            .id()
            .ok()
            .map(|id| id.to_string() == input_device_id)
            .unwrap_or(false)
    }) {
        Some(device) => device,
        None => {
            return SessionExit::Retry {
                reason: "selected input device not found".to_owned(),
                ran_for: Duration::ZERO,
            }
        }
    };

    let output_device = match output_devices.into_iter().find(|device| {
        device
            .id()
            .ok()
            .map(|id| id.to_string() == output_device_id)
            .unwrap_or(false)
    }) {
        Some(device) => device,
        None => {
            return SessionExit::Retry {
                reason: "selected output device not found".to_owned(),
                ran_for: Duration::ZERO,
            }
        }
    };

    let stream_config: StreamConfig = match input_device.default_input_config() {
        Ok(config) => config.into(),
        Err(error) => {
            return SessionExit::Retry {
                reason: format!("failed to get input config: {error}"),
                ran_for: Duration::ZERO,
            }
        }
    };

    let channels = stream_config.channels as usize;
    if channels != 2 {
        return SessionExit::Retry {
            reason: format!(
                "PUBG limiter requires stereo input, current device has {channels} channel(s)"
            ),
            ran_for: Duration::ZERO,
        };
    }

    let sample_rate = stream_config.sample_rate as f32;

    crate::diagnostics::configure_audio_monitor(sample_rate, LOOKAHEAD_MS as f32);
    let _monitor_guard = SessionMonitorGuard::new(generation);

    // Limiter 被 move 进 Input Callback，整个 Session 中只有这一位 owner。
    let mut limiter = LoudnessLimiter::new(runtime_params);
    limiter.set_sample_rate(sample_rate);

    // ========================================================
    // Stereo Frame Ring Buffer
    // ========================================================
    let target_latency_frames = (((TARGET_RING_LATENCY_MS / 1000.0) * sample_rate)
        .round() as usize)
        .max(1);

    let ring_capacity_frames = target_latency_frames
        .saturating_mul(2)
        .max(2);

    let ring = HeapRb::<(f32, f32)>::new(ring_capacity_frames);
    let (mut producer, mut consumer) = ring.split();

    for _ in 0..target_latency_frames {
        let _ = producer.try_push((0.0, 0.0));
    }

    crate::diagnostics::configure_ring_monitor(
        target_latency_frames,
        ring_capacity_frames,
    );

    let stream_failed = Arc::new(AtomicBool::new(false));

    // 重建 Stream 后仅对最前面的 10ms 捕获音频做淡入。
    // 淡入发生在 Limiter 之后且 gain <= 1，不会改变 Ceiling，也不会改变稳定运行时 DSP。
    let fade_total_frames = ((sample_rate * RECONNECT_FADE_MS / 1000.0).round() as usize)
        .max(1);
    let mut fade_frame_index = 0usize;

    // ========================================================
    // Input Callback
    // ========================================================
    let input_data_fn = move |data: &[f32], _: &cpal::InputCallbackInfo| {
        crate::diagnostics::input_callback();
        limiter.begin_audio_callback();

        let mut index = 0usize;

        while index + 1 < data.len() {
            let (mut left, mut right) =
                limiter.process_stereo_frame(data[index], data[index + 1]);

            if fade_frame_index < fade_total_frames {
                let fade_gain = (fade_frame_index + 1) as f32 / fade_total_frames as f32;
                fade_frame_index += 1;
                left *= fade_gain;
                right *= fade_gain;
            }

            if producer.try_push((left, right)).is_err() {
                crate::diagnostics::ring_push_drop();
            }

            index += 2;
        }

        // 奇数 remainder sample 故意丢弃，绝不破坏 L/R frame 对齐。
        crate::diagnostics::ring_fill_sample(producer.occupied_len());
    };

    // ========================================================
    // Output Callback
    // ========================================================
    let output_data_fn = move |out_data: &mut [f32], _: &cpal::OutputCallbackInfo| {
        crate::diagnostics::output_callback();

        let mut fell_behind = false;
        let mut last_frame = (0.0f32, 0.0f32);
        let mut index = 0usize;

        while index + 1 < out_data.len() {
            match consumer.try_pop() {
                Some((left, right)) => {
                    last_frame = (left, right);
                    out_data[index] = left;
                    out_data[index + 1] = right;
                }
                None => {
                    fell_behind = true;
                    out_data[index] = last_frame.0;
                    out_data[index + 1] = last_frame.1;
                }
            }

            index += 2;
        }

        if index < out_data.len() {
            out_data[index] = 0.0;
        }

        if fell_behind {
            crate::diagnostics::output_underrun();
        }

        crate::diagnostics::ring_fill_sample(consumer.occupied_len());
    };

    // ========================================================
    // Error Callback
    // ========================================================
    let input_failed = Arc::clone(&stream_failed);
    let input_err_fn = move |_error| {
        crate::diagnostics::input_error();
        input_failed.store(true, Ordering::Release);
        // Error callback 只发信号，不做日志格式化 / I/O。
        // Supervisor 会在非实时控制线程记录重连原因。
    };

    let output_failed = Arc::clone(&stream_failed);
    let output_err_fn = move |_error| {
        crate::diagnostics::output_error();
        output_failed.store(true, Ordering::Release);
        // 同上：实时相关 callback 只做 Atomic 信号。
    };

    // ========================================================
    // Build / Start Stream
    // ========================================================
    let input_stream = match input_device.build_input_stream(
        &stream_config,
        input_data_fn,
        input_err_fn,
        None,
    ) {
        Ok(stream) => stream,
        Err(error) => {
            return SessionExit::Retry {
                reason: format!("failed to build input stream: {error}"),
                ran_for: Duration::ZERO,
            }
        }
    };

    // 保持现有工作路径：输出 Stream 使用与输入一致的 StreamConfig。
    // 如果未来需要支持不同采样率/声道，应单独加入 SRC，不在这里隐式改格式。
    let output_stream = match output_device.build_output_stream(
        &stream_config,
        output_data_fn,
        output_err_fn,
        None,
    ) {
        Ok(stream) => stream,
        Err(error) => {
            return SessionExit::Retry {
                reason: format!("failed to build output stream: {error}"),
                ran_for: Duration::ZERO,
            }
        }
    };

    if let Err(error) = input_stream.play() {
        return SessionExit::Retry {
            reason: format!("failed to start input stream: {error}"),
            ran_for: Duration::ZERO,
        };
    }

    if let Err(error) = output_stream.play() {
        return SessionExit::Retry {
            reason: format!("failed to start output stream: {error}"),
            ran_for: Duration::ZERO,
        };
    }

    crate::diagnostics::audio_session_started();
    let started_at = Instant::now();

    // ========================================================
    // Session 控制循环
    // ========================================================
    loop {
        if !is_requested_to_run(state, generation) {
            drop(input_stream);
            drop(output_stream);
            return SessionExit::StopRequested;
        }

        if stream_failed.load(Ordering::Acquire) {
            let ran_for = started_at.elapsed();
            drop(input_stream);
            drop(output_stream);
            return SessionExit::Retry {
                reason: "CPAL reported a runtime stream error".to_owned(),
                ran_for,
            };
        }

        std::thread::sleep(CONTROL_POLL);
    }
}
