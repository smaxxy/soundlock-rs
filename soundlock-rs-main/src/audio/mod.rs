pub mod limiter;
pub mod volume;

use cpal::StreamConfig;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
pub use limiter::LoudnessLimiter;
use ringbuf::HeapRb;
use ringbuf::traits::{Consumer, Producer, Split};
pub use volume::VolumeController;

use crate::config::{Config, LimiterMode};
use crate::{AppState, config::OperationMode};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

pub fn start_limiter(state: Arc<Mutex<AppState>>, config: Arc<Mutex<Config>>) {
    std::thread::spawn(move || {
        let mode = match config.try_lock() {
            Ok(cfg) => cfg.operation_mode,
            Err(_) => {
                log::error!("Config lock poisoned, stopping limiter thread");
                return;
            }
        };
        if mode == OperationMode::Cable {
            run_limiter_loop_cable(state, config);
        } 
    });
}

fn run_limiter_loop_cable(state: Arc<Mutex<AppState>>, config: Arc<Mutex<Config>>) {
    let limiter = Arc::new(Mutex::new(LoudnessLimiter::new(Arc::clone(&config))));

    let (input_device_id, output_device_id) = match config.try_lock() {
        Ok(cfg) => (cfg.target_input_device_id.clone(), cfg.target_output_device_id.clone()),
        Err(_) => {
            log::error!("Config lock poisoned");
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

    let input_device = match input_devices
        .into_iter()
        .find(|d| d.id().ok().map(|id| id.to_string() == input_device_id).unwrap_or(false))
    {
        Some(d) => d,
        None => {
            log::error!("Input device not found");
            return;
        }
    };
    let output_device = match output_devices
        .into_iter()
        .find(|d| d.id().ok().map(|id| id.to_string() == output_device_id).unwrap_or(false))
    {
        Some(d) => d,
        None => {
            log::error!("Output device not found");
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

    if let Ok(mut l) = limiter.lock() {
        l.set_sample_rate(stream_config.sample_rate as f32);
    }

    let latency_ms = 150.0f32;
    let latency_frames = (latency_ms / 1_000.0) * stream_config.sample_rate as f32;
    let latency_samples = (latency_frames as usize) * stream_config.channels as usize;

    let ring = HeapRb::<f32>::new(latency_samples * 2);
    let (mut producer, mut consumer) = ring.split();
    for _ in 0..latency_samples {
        let _ = producer.try_push(0.0);
    }

    let limiter_clone = Arc::clone(&limiter);

    let input_data_fn = move |data: &[f32], _: &cpal::InputCallbackInfo| {
        if let Ok(mut l) = limiter_clone.try_lock() {
            let mode = l.mode;
            for &sample in data {
                let final_sample = if mode == LimiterMode::Multiband {
                    l.process_sample_multiband(sample)
                } else {
                    l.process_sample_fullband(sample)
                };
                if producer.try_push(final_sample).is_err() {
                    // buffer full, sample dropped
                }
            }
        } else {
            for &sample in data {
                if producer.try_push(sample).is_err() {}
            }
        }
    };

    let output_data_fn = move |out_data: &mut [f32], _: &cpal::OutputCallbackInfo| {
        let mut fell_behind = false;
        let mut last_sample = 0.0f32;
        for sample in out_data.iter_mut() {
            *sample = match consumer.try_pop() {
                Some(s) => {
                    last_sample = s;
                    s
                }
                None => {
                    fell_behind = true;
                    last_sample
                }
            };
        }
        if fell_behind {
            log::warn!("Input buffer empty");
        }
    };

    let err_fn = |err| log::error!("Stream error: {}", err);

    let input_stream = match input_device.build_input_stream(
        &stream_config,
        input_data_fn,
        err_fn,
        None,
    ) {
        Ok(s) => s,
        Err(e) => {
            log::error!("Failed to build input stream: {}", e);
            return;
        }
    };
    let output_stream = match output_device.build_output_stream(
        &stream_config,
        output_data_fn,
        err_fn,
        None,
    ) {
        Ok(s) => s,
        Err(e) => {
            log::error!("Failed to build output stream: {}", e);
            return;
        }
    };

    if let Err(e) = input_stream.play() {
        log::error!("Failed to start input stream: {}", e);
        return;
    }
    if let Err(e) = output_stream.play() {
        log::error!("Failed to start output stream: {}", e);
        return;
    }

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
        if let Ok(mut l) = limiter.lock() {
            l.update_parameters();
        }
        std::thread::sleep(Duration::from_secs(1));
    }

    drop(input_stream);
    drop(output_stream);
}

