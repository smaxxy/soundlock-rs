use std::sync::{Arc, Mutex};

use crate::config::Config;

pub struct LoudnessLimiter {
    config: Arc<Mutex<Config>>,
    // 当前平滑后的增益值（全频段用，仍保留供 WinAPI 使用）
    current_gain: f32,
    // 缓存参数
    cached_threshold_db: f32,
    cached_attack_ms: f32,
    cached_release_ms: f32,
    cached_crossover_freq: f32,   // 分频点 (Hz)
    // 采样率，由外部设置
    sample_rate: f32,

    // 多频段处理状态
    hp_x1: f32,                    // 高通滤波器上一个输入
    hp_y1: f32,                    // 高通滤波器上一个输出
    high_rms_smooth: f32,          // 高频 RMS 平滑值
    high_gain: f32,                // 高频当前增益

    // 高频增益平滑所需的采样步数
    high_attack_steps: f32,
    high_release_steps: f32,

    // 滤波器系数
    alpha: f32,                    // 高通滤波器系数
    rms_alpha: f32,                // RMS 平滑系数
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

    // 立刻用默认采样率计算滤波系数，确保功能立即可用
    limiter.recalc_filter_coeffs();
    limiter
}

    /// 由音频流启动后调用，设置实际采样率，并重新计算所有相关系数
    pub fn set_sample_rate(&mut self, rate: f32) {
        self.sample_rate = rate;
        self.recalc_filter_coeffs();
    }

    /// 定期调用，用 try_lock 安全刷新缓存参数，并重新计算系数
    pub fn update_parameters(&mut self) {
        if let Ok(cfg) = self.config.try_lock() {
            self.cached_threshold_db = cfg.threshold_db;
            self.cached_attack_ms = cfg.attack_ms as f32;
            self.cached_release_ms = cfg.release_ms as f32;
            self.cached_crossover_freq = cfg.crossover_freq;
            self.recalc_filter_coeffs();
        }
    }

    /// 重新计算滤波器系数及平滑步进值
    fn recalc_filter_coeffs(&mut self) {
        let cutoff = self.cached_crossover_freq;
        let dt = 1.0 / self.sample_rate;
        let rc = 1.0 / (2.0 * std::f32::consts::PI * cutoff);
        self.alpha = dt / (rc + dt);

        self.rms_alpha = 1.0 - (-1.0 / (0.01 * self.sample_rate)).exp();

        self.high_attack_steps = self.cached_attack_ms / 1000.0 * self.sample_rate;
        self.high_release_steps = self.cached_release_ms / 1000.0 * self.sample_rate;
    }

    /// Cable 模式专用：处理单个采样点，返回处理后的音频值（低频直通 + 高频*动态增益）
    pub fn process_sample_multiband(&mut self, sample: f32) -> f32 {
        // 高通滤波：high = alpha * (上一次输出 + 当前输入 - 上一次输入)
        let high = self.alpha * (self.hp_y1 + sample - self.hp_x1);
        self.hp_x1 = sample;
        self.hp_y1 = high;

        // 低频 = 原始 - 高频
        let low = sample - high;

        // 更新高频 RMS 平滑值
        self.high_rms_smooth += self.rms_alpha * (high.abs() - self.high_rms_smooth);

        // 计算高频目标增益
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

        // 高频增益平滑（逐采样线性步进）
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

        // 混合输出
        low + high * self.high_gain
    }

    // 以下两个方法保留给 WinAPI 模式使用
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

        let (attack_samples, release_samples) = (
            self.cached_attack_ms / 1000.0 * self.sample_rate,
            self.cached_release_ms / 1000.0 * self.sample_rate,
        );

        let step = if target_gain < self.current_gain {
            (self.current_gain - target_gain) / attack_samples.max(1.0)
        } else {
            (target_gain - self.current_gain) / release_samples.max(1.0)
        };

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
        self.current_gain
    }
}
