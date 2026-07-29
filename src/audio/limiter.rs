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
            match self.config.try_lock() {
                Ok(cfg) => {
                    threshold_db = cfg.threshold_db;
                    attack_ms = cfg.attack_ms as f32;
                    release_ms = cfg.release_ms as f32;
                }
                Err(_) => {
                    // 锁被占用或已中毒，直接用上一次计算出的增益值
                    // 保证音频处理永不卡死、永不崩溃
                    return self.current_gain;
                }
            }
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
