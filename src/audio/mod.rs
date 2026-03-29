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

        if config.lock().unwrap().operation_mode == OperationMode::Cable {
            run_limiter_loop_cable(Arc::clone(&state), Arc::clone(&config));
        } else {
            run_limiter_loop_winapi(Arc::clone(&state), Arc::clone(&config));
        }

        log::info!("Audio limiter thread stopped");
    });
}

// we just assume that the input and output devices are selected and available
// or crash.
fn run_limiter_loop_cable(state: Arc<Mutex<AppState>>, config: Arc<Mutex<Config>>) {
    let mut limiter = LoudnessLimiter::new(Arc::clone(&config));

    let host = cpal::default_host();
    let input_device = host
        .input_devices()
        .unwrap()
        .find(|d| {
            d.id().unwrap().to_string()
                == *config
                    .lock()
                    .unwrap()
                    .target_input_device_id
                    .as_ref()
                    .unwrap()
                    .to_string()
        })
        .unwrap();

    let output_device = host
        .output_devices()
        .unwrap()
        .find(|d| {
            d.id().unwrap().to_string()
                == *config
                    .lock()
                    .unwrap()
                    .target_output_device_id
                    .as_ref()
                    .unwrap()
                    .to_string()
        })
        .unwrap();

    let config: StreamConfig = input_device
        .default_input_config()
        .expect("Failed to get input config")
        .into();

    let latency_ms = 150.0f32;
    let latency_frames = (latency_ms / 1_000.0) * config.sample_rate as f32;
    let latency_samples = (latency_frames as usize) * config.channels as usize;

    let ring = HeapRb::<f32>::new(latency_samples * 2);
    let (mut producer, mut consumer) = ring.split();

    for _ in 0..latency_samples {
        producer.try_push(0.0).unwrap();
    }

    let input_data_fn = move |data: &[f32], _: &cpal::InputCallbackInfo| {
        let mut fell_behind = false;

        let rms = (data.iter().map(|&s| s * s).sum::<f32>() / data.len() as f32).sqrt();
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

    log::info!("Building streams with config: {:?}", config);

    let input_stream = input_device
        .build_input_stream(&config, input_data_fn, err_fn, None)
        .expect("Failed to build input stream");

    let output_stream = output_device
        .build_output_stream(&config, output_data_fn, err_fn, None)
        .expect("Failed to build output stream");

    log::info!("Starting streams with {}ms latency", latency_ms);
    input_stream.play().expect("Failed to start input stream");
    output_stream.play().expect("Failed to start output stream");

    while state.lock().unwrap().is_limiting {
        std::thread::sleep(Duration::from_secs(1));
    }

    log::info!("Stopping audio limiter loop");
    drop(input_stream);
    drop(output_stream);
}

fn run_limiter_loop_winapi(state: Arc<Mutex<AppState>>, config: Arc<Mutex<Config>>) {
    let mut limiter = LoudnessLimiter::new(Arc::clone(&config));
    let volume_ctrl =
        VolumeController::for_process(config.lock().unwrap().target_pid.unwrap()).unwrap();

    let original_volume = volume_ctrl.get_original_volume();

    let mut last_gain = 0f32;

    loop {
        if !state.lock().unwrap().is_limiting {
            break;
        }

        let scan_interval_ms;
        let volume_change_percentage_threshold;

        {
            let config = config.lock().unwrap();
            scan_interval_ms = config.scan_interval_ms;
            volume_change_percentage_threshold = config.volume_change_percentage_threshold;
        }

        if let Ok(rms) = volume_ctrl.get_current_rms() {
            let gain = limiter.calculate_gain(rms);

            if (last_gain - gain).abs() > volume_change_percentage_threshold {
                last_gain = gain;
            } else {
                continue;
            }

            let target_volume = (original_volume * gain).clamp(0.0, 1.0);
            volume_ctrl.set_volume(target_volume).unwrap();
        }

        std::thread::sleep(Duration::from_millis(scan_interval_ms as u64));
    }

    log::info!("Restoring original volume: {:.2}", original_volume);
    volume_ctrl.restore().unwrap();
}
