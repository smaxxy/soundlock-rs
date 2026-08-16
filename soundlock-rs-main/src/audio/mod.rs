pub mod limiter;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::StreamConfig;

pub use limiter::LoudnessLimiter;

use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::HeapRb;

use crate::config::Config;
use crate::AppState;

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

pub fn start_limiter(
    state: Arc<Mutex<AppState>>,
    config: Arc<Mutex<Config>>,
) {
    std::thread::spawn(move || {
        run_limiter_loop_cable(state, config);
    });
}

fn run_limiter_loop_cable(
    state: Arc<Mutex<AppState>>,
    config: Arc<Mutex<Config>>,
) {
    let limiter =
        Arc::new(Mutex::new(
            LoudnessLimiter::new(
                Arc::clone(&config),
            ),
        ));

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
    // 设置真实采样率
    // ========================================================

    if let Ok(mut l) =
        limiter.lock()
    {
        l.set_sample_rate(
            stream_config.sample_rate as f32,
        );
    }

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

    let limiter_clone =
        Arc::clone(&limiter);

    let input_data_fn =
        move |
            data: &[f32],
            _: &cpal::InputCallbackInfo,
        | {
            crate::diagnostics::
                input_callback();

            // ================================================
            // 成功获得 Limiter 锁
            // ================================================

            if let Ok(mut l) =
                limiter_clone.try_lock()
            {
                // --------------------------------------------
                // 非常重要：
                //
                // data 是：
                //
                // L R L R L R L R ...
                //
                // 以前：
                //
                // for sample {
                //     process_sample(sample)
                // }
                //
                // 会导致左右声道被当成连续的 Mono sample。
                //
                // 现在必须：
                //
                // L + R
                //   ↓
                // process_stereo_frame()
                //
                // 左右共用同一套 RMS / Peak / Gain。
                // --------------------------------------------

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
                        l.process_stereo_frame(
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

                // 理论上 Stereo callback
                // 不应该出现奇数 sample。
                //
                // 这里仍做最后保险，
                // 避免极端情况下直接丢数据。
                for &sample in
                    frames.remainder()
                {
                    if producer
                        .try_push(sample)
                        .is_err()
                    {
                        crate::diagnostics::
                            ring_push_drop();
                    }
                }
            } else {
                // ============================================
                // Limiter 锁暂时拿不到
                // ============================================
                //
                // 不阻塞实时音频线程，
                // 直接原样送入 Ring Buffer。
                //
                crate::diagnostics::
                    limiter_lock_miss();

                for &sample in data {
                    if producer
                        .try_push(sample)
                        .is_err()
                    {
                        crate::diagnostics::
                            ring_push_drop();
                    }
                }
            }
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

        // 每秒同步 UI 参数。
        if let Ok(mut l) =
            limiter.try_lock()
        {
            l.update_parameters();
        }

        std::thread::sleep(
            Duration::from_secs(1),
        );
    }

    drop(input_stream);
    drop(output_stream);
}