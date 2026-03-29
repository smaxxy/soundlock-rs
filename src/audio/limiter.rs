use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

use crate::config::Config;

pub struct LoudnessLimiter {
    config: Arc<Mutex<Config>>,
    current_gain: f32,
    last_update: Instant,
}

impl LoudnessLimiter {
    pub fn new(confog: Arc<Mutex<Config>>) -> Self {
        Self {
            config: confog,
            current_gain: 1.0,
            last_update: Instant::now(),
        }
    }

    pub fn calculate_gain(&mut self, input_rms: f32) -> f32 {
        let threshold_db;
        let attack_ms;
        let release_ms;

        {
            let config = self.config.lock().unwrap();
            threshold_db = config.threshold_db;
            attack_ms = config.attack_ms as f32;
            release_ms = config.release_ms as f32;
        }

        let now = Instant::now();
        let elapsed = now.duration_since(self.last_update).as_secs_f32();
        self.last_update = now;

        let input_rms = input_rms.max(1e-10);

        let input_db = 20.0 * input_rms.log10();
        let excess_db = input_db - threshold_db;

        if excess_db > 0.0 {
            let target_gain = 10_f32.powf(-excess_db / 20.0);

            let attack_coeff = 1.0 - (-elapsed * 1000.0 / attack_ms).exp();

            self.current_gain += attack_coeff * (target_gain - self.current_gain);
        } else {
            let release_coeff = 1.0 - (-elapsed * 1000.0 / release_ms).exp();
            self.current_gain += release_coeff * (1.0 - self.current_gain);
        }

        self.current_gain = self.current_gain.clamp(0.0, 1.0);
        self.current_gain
    }
}
