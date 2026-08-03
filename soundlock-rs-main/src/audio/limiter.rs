use crate::config::{Config, LimiterMode};
use std::sync::{Arc, Mutex};

pub struct LoudnessLimiter {
    config: Arc<Mutex<Config>>,
    pub mode: LimiterMode,

    sample_rate: f32,
    // 全频 RMS 平滑状态
    fullband_rms: f32,
    // 增益平滑系数（全频用）
    gain_smoother: f32,

    // 分频状态
    lowpass_l: f32,
    lowpass_r: f32,
    highpass_l: f32,
    highpass_r: f32,
    high_rms: f32,
    high_gain: f32,
}

impl LoudnessLimiter {
    pub fn new(config: Arc<Mutex<Config>>) -> Self {
        let mode = config
            .try_lock()
            .map(|cfg| cfg.limiter_mode)
            .unwrap_or_default();
        Self {
            config,
            mode,
            sample_rate: 48000.0,
            fullband_rms: 0.0,
            gain_smoother: 1.0,
            lowpass_l: 0.0,
            lowpass_r: 0.0,
            highpass_l: 0.0,
            highpass_r: 0.0,
            high_rms: 0.0,
            high_gain: 1.0,
        }
    }

    pub fn set_sample_rate(&mut self, sr: f32) {
        self.sample_rate = sr;
    }

    /// 参数同步（会被 Audio 线程每秒调用一次）
    pub fn update_parameters(&mut self) {
        if let Ok(cfg) = self.config.try_lock() {
            self.mode = cfg.limiter_mode;
        }
    }

    /// 计算增益（保留给 WinAPI 模式，也用于全频每样本处理）
    pub fn calculate_gain(&mut self, rms: f32, _peak: f32) -> f32 {
        let threshold_db = self.config
            .try_lock()
            .map(|cfg| cfg.threshold_db)
            .unwrap_or(-16.0);

        if rms <= 0.0 {
            return 1.0;
        }

        let db = 20.0 * rms.log10();
        if db > threshold_db {
            let reduction = db - threshold_db;
            let target_linear = 10.0f32.powf(-reduction / 20.0);
            // 硬增益应用，不在这里做平滑（平滑交给调用方）
            target_linear
        } else {
            1.0
        }
    }

    /// 全频处理（每样本调用）
    pub fn process_sample_fullband(&mut self, sample: f32) -> f32 {
        // 非常快的 RMS 跟踪器（tau ~ 10ms @48k）
        let alpha = 0.002; // 简化平滑系数
        self.fullband_rms = alpha * sample * sample + (1.0 - alpha) * self.fullband_rms;
        let rms = self.fullband_rms.sqrt();

        let target_gain = self.calculate_gain(rms, sample.abs());

        // 增益平滑（attack/release 由外层的 GainSmother 实现，这里简单线性平滑）
        let smooth_coeff = 0.005;
        self.gain_smoother += smooth_coeff * (target_gain - self.gain_smoother);
        self.gain_smoother = self.gain_smoother.clamp(0.0, 1.0);

        sample * self.gain_smoother
    }

    /// 分频处理（每样本调用）
    pub fn process_sample_multiband(&mut self, sample: f32) -> f32 {
        let crossover_freq = self.config
            .try_lock()
            .map(|cfg| cfg.crossover_freq)
            .unwrap_or(300.0);

        // 简单的一阶 IIR 分频（系数与采样率相关）
        let dt = 1.0 / self.sample_rate;
        let omega = 2.0 * std::f32::consts::PI * crossover_freq;
        let alpha_lp = 1.0 / (1.0 + omega * dt / 2.0); // 未使用，可优化
        let rc = 1.0 / (omega);
        let alpha = dt / (rc + dt);
        let alpha = alpha.clamp(0.0, 1.0);

        // 低通
        self.lowpass_r = sample * alpha + self.lowpass_r * (1.0 - alpha);
        let low = self.lowpass_r;

        // 高通 = 原信号 - 低通
        let high = sample - low;

        // 高频 RMS 跟踪
        let alpha_rms = 0.001;
        self.high_rms = alpha_rms * high * high + (1.0 - alpha_rms) * self.high_rms;
        let rms = self.high_rms.sqrt();

        let threshold_db = self.config
            .try_lock()
            .map(|cfg| cfg.threshold_db)
            .unwrap_or(-16.0);

        let db = 20.0 * (rms.max(1e-10)).log10();
        let target_gain = if db > threshold_db {
            10.0f32.powf(-(db - threshold_db) / 20.0)
        } else {
            1.0
        };

        // 增益平滑
        let smooth_coeff = 0.01;
        self.high_gain += smooth_coeff * (target_gain - self.high_gain);
        self.high_gain = self.high_gain.clamp(0.0, 1.0);

        // 最终输出 = 低通（直通） + 高通受限
        low + high * self.high_gain
    }
}