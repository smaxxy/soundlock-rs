use crate::config::Config;
use std::sync::{Arc, Mutex};

pub struct LoudnessLimiter {
    config: Arc<Mutex<Config>>,
    sample_rate: f32,

    threshold_linear: f32,
    attack_coeff: f32,
    release_coeff: f32,

    fullband_rms: f32,
    gain_smoother: f32,
}

impl LoudnessLimiter {
    pub fn new(config: Arc<Mutex<Config>>) -> Self {
        // 从配置读取初始值
        let (threshold_db, attack_ms, release_ms) = config
            .try_lock()
            .map(|cfg| (cfg.threshold_db, cfg.attack_ms, cfg.release_ms))
            .unwrap_or((-16.0, 10, 50));

        let sample_rate = 48000.0;
        let threshold_linear = 10.0f32.powf(threshold_db / 20.0);
        let attack_coeff = Self::calc_coeff(attack_ms, sample_rate);
        let release_coeff = Self::calc_coeff(release_ms, sample_rate);

        Self {
            config,
            sample_rate,
            threshold_linear,
            attack_coeff,
            release_coeff,
            fullband_rms: 0.0,
            gain_smoother: 1.0,
        }
    }

    pub fn set_sample_rate(&mut self, sr: f32) {
        self.sample_rate = sr;
        // 重新计算系数（需在外部更新参数时重新传入 attack/release）
    }

    /// 每秒调用一次，从 config 同步参数
    pub fn update_parameters(&mut self) {
        if let Ok(cfg) = self.config.try_lock() {
            self.threshold_linear = 10.0f32.powf(cfg.threshold_db / 20.0);
            self.attack_coeff = Self::calc_coeff(cfg.attack_ms, self.sample_rate);
            self.release_coeff = Self::calc_coeff(cfg.release_ms, self.sample_rate);
        }
    }

    /// 全频处理（每样本调用）
    pub fn process_sample(&mut self, sample: f32) -> f32 {
        let alpha = 0.002;
        self.fullband_rms = alpha * sample * sample + (1.0 - alpha) * self.fullband_rms;
        let rms = self.fullband_rms.sqrt();

        // 线性增益计算
        let target_gain = if rms <= 0.0 {
            1.0
        } else {
            (self.threshold_linear / rms).min(1.0)
        };

        // 根据目标增益方向选择 attack 或 release 系数
        let coeff = if target_gain < self.gain_smoother {
            self.attack_coeff
        } else {
            self.release_coeff
        };

        // 一阶低通平滑
        self.gain_smoother = target_gain * (1.0 - coeff) + self.gain_smoother * coeff;
        self.gain_smoother = self.gain_smoother.clamp(0.0, 1.0);

        sample * self.gain_smoother
    }

    fn calc_coeff(time_ms: u32, sr: f32) -> f32 {
        let time_s = time_ms as f32 / 1000.0;
        let samples = time_s * sr;
        if samples <= 0.0 {
            0.0
        } else {
            (-1.0 / samples).exp()
        }
    }
}