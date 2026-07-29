use std::sync::{Arc, Mutex};
use crate::config::Config;

pub struct LoudnessLimiter {
    config: Arc<Mutex<Config>>,
    current_gain: f32,
    cached_threshold_db: f32,
    cached_attack_ms: f32,
    cached_release_ms: f32,
    cached_crossover_freq: f32,
    sample_rate: f32,
    hp_x1: f32,
    hp_y1: f32,
    high_rms_smooth: f32,
    high_gain: f32,
    high_attack_steps: f32,
    high_release_steps: f32,
    alpha: f32,
    rms_alpha: f32,
}

impl LoudnessLimiter {
    pub fn new(config: Arc<Mutex<Config>>) -> Self {
        let (threshold, attack, release, crossover) = match config.try_lock() {
            Ok(cfg) => (
                cfg.threshold_db,
                cfg.attack_ms as f32,
                cfg.release_ms as f32,
                cfg.crossover_freq,
            ),
            Err(_) => (-20.0, 10.0, 50.0, 300.0),
        };
        let mut limiter = Self {
            config,
            current_gain: 1.0,
            cached_threshold_db: threshold,
            cached_attack_ms: attack,
            cached_release_ms: release,
            cached_crossover_freq: crossover,
            sample_rate: 44100.0,
            hp_x1: 0.0,
            hp_y1: 0.0,
            high_rms_smooth: 0.0,
            high_gain: 1.0,
            high_attack_steps: 0.0,
            high_release_steps: 0.0,
            alpha: 0.0,
            rms_alpha: 0.0,
        };
        limiter.recalc_filter_coeffs();
        limiter
    }

    pub fn set_sample_rate(&mut self, rate: f32) {
        self.sample_rate = rate;
        self.recalc_filter_coeffs();
    }

    pub fn update_parameters(&mut self) {
        let (threshold, attack, release, crossover) = {
            match self.config.try_lock() {
                Ok(cfg) => (
                    cfg.threshold_db,
                    cfg.attack_ms as f32,
                    cfg.release_ms as f32,
                    cfg.crossover_freq,
                ),
                Err(_) => return,
            }
        };
        self.cached_threshold_db = threshold;
        self.cached_attack_ms = attack;
        self.cached_release_ms = release;
        self.cached_crossover_freq = crossover;
        self.recalc_filter_coeffs();
    }

    fn recalc_filter_coeffs(&mut self) {
        let cutoff = self.cached_crossover_freq;
        let dt = 1.0 / self.sample_rate;
        let rc = 1.0 / (2.0 * std::f32::consts::PI * cutoff);
        self.alpha = dt / (rc + dt);
        self.rms_alpha = 1.0 - (-1.0 / (0.01 * self.sample_rate)).exp();
        self.high_attack_steps = self.cached_attack_ms / 1000.0 * self.sample_rate;
        self.high_release_steps = self.cached_release_ms / 1000.0 * self.sample_rate;
    }

    pub fn process_sample_multiband(&mut self, sample: f32) -> f32 {
        // 如果系数意外为0，直通全频段，并应用全频段降音（安全兜底）
        if self.alpha <= 0.0 || self.high_attack_steps <= 0.0 {
            let rms = sample.abs();
            let gain = self.calculate_gain(rms, 1);
            return sample * gain;
        }

        let high = self.alpha * (self.hp_y1 + sample - self.hp_x1);
        self.hp_x1 = sample;
        self.hp_y1 = high;
        let low = sample - high;

        self.high_rms_smooth += self.rms_alpha * (high.abs() - self.high_rms_smooth);

        let target_high_gain = if self.high_rms_smooth > 1e-10 {
            let db = 20.0 * self.high_rms_smooth.log10();
            if db > self.cached_threshold_db {
                10_f32.powf(-(db - self.cached_threshold_db) / 20.0)
            } else {
                1.0
            }
        } else {
            1.0
        };

        let step = if target_high_gain < self.high_gain {
            (self.high_gain - target_high_gain) / self.high_attack_steps.max(1.0)
        } else {
            (target_high_gain - self.high_gain) / self.high_release_steps.max(1.0)
        };

        if (self.high_gain - target_high_gain).abs() <= step {
            self.high_gain = target_high_gain;
        } else if target_high_gain < self.high_gain {
            self.high_gain -= step;
        } else {
            self.high_gain += step;
        }

        low + high * self.high_gain
    }

    pub fn compute_target_gain(&self, rms: f32) -> f32 {
        let rms = rms.max(1e-10);
        let db = 20.0 * rms.log10();
        if db > self.cached_threshold_db {
            10_f32.powf(-(db - self.cached_threshold_db) / 20.0)
        } else {
            1.0
        }
    }

    pub fn calculate_gain(&mut self, input_rms: f32, num_samples: usize) -> f32 {
    let input_rms = input_rms.max(1e-10);
    let input_db = 20.0 * input_rms.log10();
    let excess_db = input_db - self.cached_threshold_db;

    let target_gain = if excess_db > 0.0 {
        10_f32.powf(-excess_db / 20.0)
    } else {
        1.0
    };

    let (attack_steps, release_steps) = (
        self.cached_attack_ms / 1000.0 * self.sample_rate,
        self.cached_release_ms / 1000.0 * self.sample_rate,
    );

    let step = if target_gain < self.current_gain {
        (self.current_gain - target_gain) / attack_steps.max(1.0)
    } else {
        (target_gain - self.current_gain) / release_steps.max(1.0)
    };

    // 逐采样滑动增益，而不是整个帧用一个值
    let mut gain = self.current_gain;
    for _ in 0..num_samples {
        if (gain - target_gain).abs() <= step {
            gain = target_gain;
            break;
        }
        if target_gain < gain {
            gain -= step;
        } else {
            gain += step;
        }
    }
    self.current_gain = gain;

    // 返回本帧最后一个采样点的增益（也可以返回平均值，但相差极小）
    self.current_gain
}
}
