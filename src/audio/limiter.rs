use std::sync::{Arc, Mutex};

use crate::config::Config;

pub struct LoudnessLimiter {
    // 用于定期更新缓存参数，不在音频回调中直接使用
    config: Arc<Mutex<Config>>,
    // 当前平滑后的增益值
    current_gain: f32,
    // 缓存参数，避免在音频回调中碰锁
    cached_threshold_db: f32,
    cached_attack_ms: f32,
    cached_release_ms: f32,
    // 采样率，由外部设置
    sample_rate: f32,
}

impl LoudnessLimiter {
    pub fn new(config: Arc<Mutex<Config>>) -> Self {
        // 初始化时尝试读取一次配置作为缓存，失败则使用默认值
        let (threshold, attack, release) = match config.try_lock() {
            Ok(cfg) => (
                cfg.threshold_db,
                cfg.attack_ms as f32,
                cfg.release_ms as f32,
            ),
            Err(_) => (-20.0, 10.0, 50.0), // 安全的默认值
        };
        Self {
            config,
            current_gain: 1.0,
            cached_threshold_db: threshold,
            cached_attack_ms: attack,
            cached_release_ms: release,
            sample_rate: 44100.0, // 临时默认，会被 mod.rs 正确设置
        }
    }

    /// 由音频流启动后调用，设置实际采样率
    pub fn set_sample_rate(&mut self, rate: f32) {
        self.sample_rate = rate;
    }

    /// 定期调用（如每秒一次），用 try_lock 安全刷新缓存参数
    pub fn update_parameters(&mut self) {
        if let Ok(cfg) = self.config.try_lock() {
            self.cached_threshold_db = cfg.threshold_db;
            self.cached_attack_ms = cfg.attack_ms as f32;
            self.cached_release_ms = cfg.release_ms as f32;
        }
    }

    /// 计算当前帧的平滑增益
    /// `rms`       : 当前帧的 RMS 值
    /// `num_samples` : 本帧包含的采样数
    /// 返回整个帧应用的统一增益（帧内恒定，帧间线性插值）
    pub fn calculate_gain(&mut self, input_rms: f32, num_samples: usize) -> f32 {
        // 1. 计算目标增益
        let input_rms = input_rms.max(1e-10);
        let input_db = 20.0 * input_rms.log10();
        let excess_db = input_db - self.cached_threshold_db;

        let target_gain = if excess_db > 0.0 {
            10_f32.powf(-excess_db / 20.0)
        } else {
            1.0
        };

        // 2. 根据 attack/release 时间计算本帧允许的总步进量（帧级平滑）
        let (attack_samples, release_samples) = (
            self.cached_attack_ms / 1000.0 * self.sample_rate,
            self.cached_release_ms / 1000.0 * self.sample_rate,
        );

        let step = if target_gain < self.current_gain {
            // attack：需要下降
            let total_steps = attack_samples.max(1.0);
            (self.current_gain - target_gain) / total_steps
        } else {
            // release：需要上升
            let total_steps = release_samples.max(1.0);
            (target_gain - self.current_gain) / total_steps
        };

        // 3. 模拟 num_samples 个采样点上的线性滑动，得到本帧结束时的增益
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

        // 4. 本帧全部采样点使用同一个增益（已经过帧级平滑）
        self.current_gain
    }
}
