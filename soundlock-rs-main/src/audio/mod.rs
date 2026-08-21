pub mod limiter;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::StreamConfig;

pub use limiter::LoudnessLimiter;

use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::HeapRb;

use crate::config::{Config, RuntimeLimiterParams};
use crate::AppState;

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

pub fn start_limiter(
    state: Arc<Mutex<AppState>>,
    config: Arc<Mutex<Config>>,
    runtime_params: Arc<RuntimeLimiterParams>,
) {
    std::thread::spawn(move || {
        run_limiter_loop_cable(
            state,
            config,
            runtime_params,
        );
    });
}

fn run_limiter_loop_cable(
    state: Arc<Mutex<AppState>>,
    config: Arc<Mutex<Config>>,
    runtime_params: Arc<RuntimeLimiterParams>,
) {
    // ========================================================
    // 读取输入 / 输出设备
    // ========================================================

    let (
        input_device_id,
        output_device_id,
    ) = match config.try_lock() {
        Ok(cfg) => (
            cfg.target_input_device_id.clone(),
            cfg.target_output_device_id.clone(),
        ),

        Err(_) => {
            log::error!(
                "Config lock poisoned"
            );
            return;
        }
    };

    let input_device_id =
        match input_device_id {
            Some(id) => id,

            None => {
                log::error!(
                    "No input device selected"
                );
                return;
            }
        };

    let output_device_id =
        match output_device_id {
            Some(id) => id,

            None => {
                log::error!(
                    "No output device selected"
                );
                return;
            }
        };

    // ========================================================
    // 获取设备
    // ========================================================

    let host =
        cpal::default_host();

    let input_devices =
        match host.input_devices() {
            Ok(d) => d,

            Err(e) => {
                log::error!(
                    "Failed to get input devices: {}",
                    e
                );
                return;
            }
        };

    let output_devices =
        match host.output_devices() {
            Ok(d) => d,

            Err(e) => {
                log::error!(
                    "Failed to get output devices: {}",
                    e
                );
                return;
            }
        };

    let input_device =
        match input_devices
            .into_iter()
            .find(|d| {
                d.id()
                    .ok()
                    .map(|id| {
                        id.to_string()
                            == input_device_id
                    })
                    .unwrap_or(false)
            })
        {
            Some(d) => d,

            None => {
                log::error!(
                    "Input device not found"
                );
                return;
            }
        };

    let output_device =
        match output_devices
            .into_iter()
            .find(|d| {
                d.id()
                    .ok()
                    .map(|id| {
                        id.to_string()
                            == output_device_id
                    })
                    .unwrap_or(false)
            })
        {
            Some(d) => d,

            None => {
                log::error!(
                    "Output device not found"
                );
                return;
            }
        };

    // ========================================================
    // 获取音频格式
    // ========================================================

    let stream_config: StreamConfig =
        match input_device
            .default_input_config()
        {
            Ok(cfg) => cfg.into(),

            Err(e) => {
                log::error!(
                    "Failed to get input config: {}",
                    e
                );
                return;
            }
        };

    // ========================================================
    // Stereo Link 要求双声道
    // ========================================================
    //
    // 新版 Limiter 是：
    //
    //     L ─┐
    //        ├→ 同一个 RMS / Peak Detector
    //     R ─┘
    //                ↓
    //          共用一个 Gain
    //
    // 所以这里明确要求 Stereo。
    //
    let channels =
        stream_config.channels as usize;

    if channels != 2 {
        log::error!(
            "PUBG limiter requires stereo input, \
             but current device has {} channel(s)",
            channels
        );

        return;
    }

    // ========================================================
    // 创建 Limiter，并设置真实采样率
    // ========================================================
    //
    // Limiter 不再放进 Arc<Mutex<...>>。
    //
    // 它会在下面被 move 进 Input Callback，
    // 由该实时回调独占整个生命周期。
    //
    // UI / 控制线程只通过 RuntimeLimiterParams
    // 发布参数，不直接访问 Limiter。

    let mut limiter =
        LoudnessLimiter::new(
            Arc::clone(&runtime_params),
        );

    limiter.set_sample_rate(
        stream_config.sample_rate as f32,
    );

    // ========================================================
    // Ring Buffer
    // ========================================================

    let latency_ms =
        150.0f32;

    let latency_frames =
        (
            latency_ms / 1_000.0
        )
            * stream_config.sample_rate
                as f32;

    let latency_samples =
        (latency_frames as usize)
            * channels;

    let ring =
        HeapRb::<f32>::new(
            latency_samples * 2,
        );

    let (
        mut producer,
        mut consumer,
    ) = ring.split();

    // 预填充静音，
    // 保证 output callback 启动时不会立即 underrun。
    for _ in 0..latency_samples {
        let _ =
            producer.try_push(0.0);
    }

    // ========================================================
    // Input Callback
    // ========================================================
    //
    // 当前数据流保持不变：
    //
    // Input
    //   ↓
    // Limiter
    //   ↓
    // Ring Buffer
    //   ↓
    // Output
    //
    // 与旧版不同的是：
    //
    // LoudnessLimiter 现在直接被这个 callback closure 独占，
    // 不再通过 Arc<Mutex<LoudnessLimiter>> 跨线程共享。
    //
    // 因此实时音频路径中：
    //
    // - 没有 limiter.lock()
    // - 没有 limiter.try_lock()
    // - 没有 limiter_lock_miss
    // - 不会因为 UI / 控制线程占锁而旁路 Limiter

    let input_data_fn =
        move |
            data: &[f32],
            _: &cpal::InputCallbackInfo,
        | {
            crate::diagnostics::
                input_callback();

            // ================================================
            // 每个 Audio Callback 只同步一次参数
            // ================================================
            //
            // 正常情况下只检查一次 Atomic version。
            //
            // 只有 UI 发布了新参数时，
            // 才会读取新快照并重新计算相关系数。
            //
            // process_stereo_frame() 内部不会再次检查版本。
            limiter.begin_audio_callback();

            // ================================================
            // Stereo Frame 处理
            // ================================================
            //
            // data:
            //
            // L R L R L R ...
            //
            // 左右声道必须作为同一个 Stereo Frame
            // 一起交给 Limiter，保证：
            //
            // - Stereo Linked RMS
            // - Stereo Linked Peak
            // - 左右共用同一个 Gain
            // - 不破坏 PUBG 方位感

            let mut frames =
                data.chunks_exact(2);

            for frame in
                &mut frames
            {
                let left =
                    frame[0];

                let right =
                    frame[1];

                let (
                    processed_left,
                    processed_right,
                ) =
                    limiter
                        .process_stereo_frame(
                            left,
                            right,
                        );

                // 左声道
                if producer
                    .try_push(
                        processed_left,
                    )
                    .is_err()
                {
                    crate::diagnostics::
                        ring_push_drop();
                }

                // 右声道
                if producer
                    .try_push(
                        processed_right,
                    )
                    .is_err()
                {
                    crate::diagnostics::
                        ring_push_drop();
                }
            }

            // Stereo interleaved callback 正常情况下
            // data.len() 必须是 2 的整数倍。
            //
            // 如果极端情况下出现 1 个 remainder sample，
            // 这里故意不把它单独写入 Ring Buffer。
            //
            // 原因：
            // 单独写入一个 sample 会让后续 L/R 交错相位错位，
            // 比丢掉这个异常 sample 更严重。
            let _ =
                frames.remainder();
        };

    // ========================================================
    // Output Callback
    // ========================================================

    let output_data_fn =
        move |
            out_data: &mut [f32],
            _: &cpal::OutputCallbackInfo,
        | {
            crate::diagnostics::
                output_callback();

            let mut fell_behind =
                false;

            let mut last_sample =
                0.0f32;

            for sample in
                out_data.iter_mut()
            {
                *sample =
                    match consumer.try_pop()
                    {
                        Some(s) => {
                            last_sample =
                                s;

                            s
                        }

                        None => {
                            fell_behind =
                                true;

                            // 保持最后一个 sample，
                            // 避免突然输出随机数据。
                            last_sample
                        }
                    };
            }

            if fell_behind {
                crate::diagnostics::
                    output_underrun();

                log::warn!(
                    "Input buffer empty"
                );
            }
        };

    // ========================================================
    // Error Callback
    // ========================================================

    let input_err_fn =
        |err| {
            crate::diagnostics::
                input_error();

            log::error!(
                "Input stream error: {}",
                err
            );
        };

    let output_err_fn =
        |err| {
            crate::diagnostics::
                output_error();

            log::error!(
                "Output stream error: {}",
                err
            );
        };

    // ========================================================
    // 创建 Stream
    // ========================================================

    let input_stream =
        match input_device
            .build_input_stream(
                &stream_config,
                input_data_fn,
                input_err_fn,
                None,
            )
        {
            Ok(s) => s,

            Err(e) => {
                log::error!(
                    "Failed to build input stream: {}",
                    e
                );
                return;
            }
        };

    let output_stream =
        match output_device
            .build_output_stream(
                &stream_config,
                output_data_fn,
                output_err_fn,
                None,
            )
        {
            Ok(s) => s,

            Err(e) => {
                log::error!(
                    "Failed to build output stream: {}",
                    e
                );
                return;
            }
        };

    // ========================================================
    // 启动
    // ========================================================

    if let Err(e) =
        input_stream.play()
    {
        log::error!(
            "Failed to start input stream: {}",
            e
        );

        return;
    }

    if let Err(e) =
        output_stream.play()
    {
        log::error!(
            "Failed to start output stream: {}",
            e
        );

        return;
    }

    // ========================================================
    // 主控制循环
    // ========================================================

    loop {
        let should_continue =
            loop {
                match state.try_lock() {
                    Ok(s) => {
                        break s.is_limiting;
                    }

                    Err(
                        std::sync::
                            TryLockError::
                            WouldBlock,
                    ) => {
                        std::thread::sleep(
                            Duration::
                                from_millis(10),
                        );

                        continue;
                    }

                    Err(_) => {
                        break false;
                    }
                }
            };

        // 检查退出标志
        let should_exit =
            crate::tray_state::
                SHOULD_EXIT
                .load(
                    std::sync::atomic::
                        Ordering::SeqCst,
                );

        if !should_continue
            || should_exit
        {
            break;
        }

        std::thread::sleep(
            Duration::from_secs(1),
        );
    }

    drop(input_stream);
    drop(output_stream);
}