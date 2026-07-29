pub mod limiter;
pub mod session;
pub mod volume;

use cpal::StreamConfig;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
pub use limiter::LoudnessLimiter;
use ringbuf::HeapRb;
use ringbuf::traits::{Consumer, Producer, Split};
pub use volume::VolumeController;

use crate::config::Config;
use crate::{AppState, config::OperationMode};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

pub fn start_limiter(state: Arc<Mutex<AppState>>, config: Arc<Mutex<Config>>) {
    std::thread::spawn(move || {
        log::info!("Audio limiter thread started");

        let mode = match config.try_lock() {
            Ok(cfg) => cfg.operation_mode,
            Err(_) => {
                log::error!("Config lock poisoned, stopping limiter thread");
                return;
            }
        };

        if mode == OperationMode::Cable {
            run_limiter_loop_cable(state, config);
        } else {
            run_limiter_loop_winapi(state, config);
        }

        log::info!("Audio limiter thread stopped");
    });
}

fn run_limiter_loop_cable(state: Arc<Mutex<AppState>>, config: Arc<Mutex<Config>>) {
    let mut limiter = LoudnessLimiter::new(Arc::clone(&config));

    // 安全获取设备 ID
    let (input_device_id, output_device_id) = match config.try_lock() {
        Ok(cfg) => (
            cfg.target_input_device_id.clone(),
            cfg.target_output_device_id.clone(),
        ),
        Err(_) => {
            log::error!("Config lock poisoned, cannot read device IDs");
            return;
        }
    };

    let input_device_id = match input_device_id {
        Some(id) => id,
        None => {
            log::error!("No input device selected");
            return;
        }
    };
    let output_device_id = match output_device_id {
        Some(id) => id,
        None => {
            log::error!("No output device selected");
            return;
        }
    };

    let host = cpal::default_host();

    let input_devices = match host.input_devices() {
        Ok(d) => d,
        Err(e) => {
            log::error!("Failed to get input devices: {}", e);
            return;
        }
    };

    let output_devices = match host.output_devices() {
        Ok(d) => d,
        Err(e) => {
            log::error!("Failed to get output devices: {}", e);
            return;
        }
    };

    let input_device = match input_devices.into_iter().find(|d| {
        d.id().ok().map(|id| id.to_string() == input_device_id).unwrap_or(false)
    }) {
        Some(d) => d,
        None => {
            log::error!("Input device not found (maybe unplugged)");
            return;
        }
    };

    let output_device = match output_devices.into_iter().find(|d| {
        d.id().ok().map(|id| id.to_string() == output_device_id).unwrap_or(false)
    }) {
        Some(d) => d,
        None => {
            log::error!("Output device not found (maybe unplugged)");
            return;
        }
    };

    let stream_config: StreamConfig = match input_device.default_input_config() {
        Ok(cfg) => cfg.into(),
        Err(e) => {
            log::error!("Failed to get input config: {}", e);
            return;
        }
    };

    let latency_ms = 150.0f32;
    let latency_frames = (latency_ms / 1_000.0) * stream_config.sample_rate as f32;
    let latency_samples = (latency_frames as usize) * stream_config.channels as usize;

    let ring = HeapRb::<f32>::new(latency_samples * 2);
    let (mut producer, mut consumer) = ring.split();

    for _ in 0..latency_samples {
        let _ = producer.try_push(0.0);
    }

    let input_data_fn = move |data: &[f32], _: &cpal::InputCallbackInfo| {
        let mut fell_behind = false;

        // 防止空切片导致除以零
        let rms = if data.is_empty() {
            0.0
        } else {
            (data.iter().map(|&s| s * s).sum::<f32>() / data.len() as f32).sqrt()
        };
        let gain = limiter.calculate_gain(rms);

        for &sample in data {
            if producer.try_push(sample * gain).is_err() {
                fell_behind = true;
            }
        }
        if fell_behind {
            log::warn!("Output buffer full - try increasing latency");
        }
    };

    let output_data_fn = move |out_data: &mut [f32], _: &cpal::OutputCallbackInfo| {
        let mut fell_behind = false;

        for sample in out_data.iter_mut() {
            *sample = match consumer.try_pop() {
                Some(s) => s,
                None => {
                    fell_behind = true;
                    0.0
                }
            };
        }

        if fell_behind {
            log::warn!("Input buffer empty - try increasing latency");
        }
    };

    let err_fn = |err| log::error!("Stream error: {}", err);

    log::info!("Building streams with config: {:?}", stream_config);

    let input_stream = match input_device.build_input_stream(&stream_config, input_data_fn, err_fn, None) {
        Ok(s) => s,
        Err(e) => {
            log::error!("Failed to build input stream: {}", e);
            return;
        }
    };

    let output_stream = match output_device.build_output_stream(&stream_config, output_data_fn, err_fn, None) {
        Ok(s) => s,
        Err(e) => {
            log::error!("Failed to build output stream: {}", e);
            return;
        }
    };

    log::info!("Starting streams with {}ms latency", latency_ms);
    if let Err(e) = input_stream.play() {
        log::error!("Failed to start input stream: {}", e);
        return;
    }
    if let Err(e) = output_stream.play() {
        log::error!("Failed to start output stream: {}", e);
        return;
    }

    // 主循环：安全等待停止信号
    loop {
        let should_continue = loop {
            match state.try_lock() {
                Ok(s) => break s.is_limiting,
                Err(std::sync::TryLockError::WouldBlock) => {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(_) => break false,
            }
        };
        if !should_continue {
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }

    log::info!("Stopping audio limiter loop");
    drop(input_stream);
    drop(output_stream);
}

fn run_limiter_loop_winapi(state: Arc<Mutex<AppState>>, config: Arc<Mutex<Config>>) {
    let mut limiter = LoudnessLimiter::new(Arc::clone(&config));

    let target_pid = match config.try_lock() {
        Ok(cfg) => match cfg.target_pid {
            Some(pid) => pid,
            None => {
                log::error!("No target PID configured for Windows API mode");
                return;
            }
        },
        Err(_) => {
            log::error!("Config lock poisoned, cannot read PID");
            return;
        }
    };

    let volume_ctrl = match VolumeController::for_process(target_pid) {
        Ok(ctrl) => ctrl,
        Err(e) => {
            log::error!("Failed to create volume controller for PID {}: {}", target_pid, e);
            return;
        }
    };

    let mut last_gain = 0.0f32;

    loop {
        let should_continue = loop {
            match state.try_lock() {
                Ok(s) => break s.is_limiting,
                Err(std::sync::TryLockError::WouldBlock) => {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(_) => break false,
            }
        };
        if !should_continue {
            break;
        }

        let (scan_interval_ms, volume_change_percentage_threshold) = {
            match config.try_lock() {
                Ok(cfg) => (cfg.scan_interval_ms, cfg.volume_change_percentage_threshold),
                Err(_) => {
                    log::error!("Config lock poisoned, stopping Windows API limiter");
                    break;
                }
            }
        };

        if let Ok(rms) = volume_ctrl.get_current_rms() {
            let gain = limiter.calculate_gain(rms);

            if (last_gain - gain).abs() <= volume_change_percentage_threshold {
                std::thread::sleep(Duration::from_millis(scan_interval_ms as u64));
                continue;
            }

            last_gain = gain;

            // 动态获取当前音量作为基准，避免手动调整音量后偏差
            if let Ok(current_vol) = volume_ctrl.get_volume() {
                let target_volume = (current_vol * gain).clamp(0.0, 1.0);
                if let Err(e) = volume_ctrl.set_volume(target_volume) {
                    log::error!("Failed to set volume: {}", e);
                }
            } else {
                log::error!("Failed to get current volume");
            }
        }

        std::thread::sleep(Duration::from_millis(scan_interval_ms as u64));
    }

    // 不再调用 restore，因为我们已经动态跟随系统音量，停止后用户当前音量即为最终值
    log::info!("Limiter stopped, volume remains at current level");
}
