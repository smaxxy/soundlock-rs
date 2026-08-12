use crate::config::{Config, LimiterMode};
use std::sync::{Arc, Mutex};

pub struct LoudnessLimiter {
    config: Arc<Mutex<Config>>,
    pub mode: LimiterMode,

    sample_rate: f32,

    // 缓存参数
    threshold_db: f32,
    threshold_linear: f32,
    crest_strong: f32,
    crest_mild: f32,
    crossover_freq: f32,

    // 全频 RMS 平滑状态
    fullband_rms: f32,
    gain_smoother: f32,

    // 分频状态
    lowpass_r: f32,
    high_rms: f32,
    high_gain: f32,

    // 智能自适应状态
    avg_energy: f32,
    peak_energy: f32,
    adaptive_gain: f32,
}

impl LoudnessLimiter {
    pub fn new(config: Arc<Mutex<Config>>) -> Self {
        let (mode, threshold_db, crossover_freq, crest_strong, crest_mild) = config
            .try_lock()
            .map(|cfg| {
                (
                    cfg.limiter_mode,
                    cfg.threshold_db,
                    cfg.crossover_freq,
                    cfg.crest_strong,
                    cfg.crest_mild,
                )
            })
            .unwrap_or_default();

        let threshold_linear = 10.0f32.powf(threshold_db / 20.0);

        Self {
            config,
            mode,
            sample_rate: 48000.0,
            threshold_db,
            threshold_linear,
            crest_strong,
            crest_mild,
            crossover_freq,
            fullband_rms: 0.0,
            gain_smoother: 1.0,
            lowpass_r: 0.0,
            high_rms: 0.0,
            high_gain: 1.0,
            avg_energy: 0.0,
            peak_energy: 0.0,
            adaptive_gain: 1.0,
        }
    }

    pub fn set_sample_rate(&mut self, sr: f32) {
        self.sample_rate = sr;
    }

    pub fn update_parameters(&mut self) {
        if let Ok(cfg) = self.config.try_lock() {
            self.mode = cfg.limiter_mode;
            self.threshold_db = cfg.threshold_db;
            self.crossover_freq = cfg.crossover_freq;
            self.threshold_linear = 10.0f32.powf(self.threshold_db / 20.0);
            self.crest_strong = cfg.crest_strong;
            self.crest_mild = cfg.crest_mild;
        }
    }

    fn calculate_gain(&self, rms: f32) -> f32 {
        if rms <= 0.0 {
            return 1.0;
        }
        (self.threshold_linear / rms).min(1.0)
    }

    pub fn process_sample_fullband(&mut self, sample: f32) -> f32 {
        let alpha = 0.002;
        self.fullband_rms = alpha * sample * sample + (1.0 - alpha) * self.fullband_rms;
        let rms = self.fullband_rms.sqrt();
        let target_gain = self.calculate_gain(rms);
        let smooth_coeff = 0.005;
        self.gain_smoother += smooth_coeff * (target_gain - self.gain_smoother);
        self.gain_smoother = self.gain_smoother.clamp(0.0, 1.0);
        sample * self.gain_smoother
    }

    pub fn process_sample_multiband(&mut self, sample: f32) -> f32 {
        let crossover = self.crossover_freq.max(1.0);
        let dt = 1.0 / self.sample_rate;
        let omega = 2.0 * std::f32::consts::PI * crossover;
        let alpha = dt / (1.0 / omega + dt);
        self.lowpass_r = sample * alpha + self.lowpass_r * (1.0 - alpha);
        let low = self.lowpass_r;
        let high = sample - low;
        let alpha_rms = 0.001;
        self.high_rms = alpha_rms * high * high + (1.0 - alpha_rms) * self.high_rms;
        let rms = self.high_rms.sqrt();
        let target_gain = self.calculate_gain(rms);
        let smooth_coeff = 0.01;
        self.high_gain += smooth_coeff * (target_gain - self.high_gain);
        self.high_gain = self.high_gain.clamp(0.0, 1.0);
        low + high * self.high_gain
    }

    pub fn process_sample_adaptive(&mut self, sample: f32) -> f32 {
        let abs_sample = sample.abs();

        let peak_alpha = 0.01;
        self.peak_energy = peak_alpha * abs_sample + (1.0 - peak_alpha) * self.peak_energy;
        let avg_alpha = 0.0005;
        self.avg_energy = avg_alpha * abs_sample + (1.0 - avg_alpha) * self.avg_energy;

        let avg = self.avg_energy.max(1e-10);
        let peak = self.peak_energy.max(1e-10);
        let crest_factor = peak / avg;

        let target_gain = if crest_factor > self.crest_strong {
            // 强爆发 → 强力压缩，但不完全静音（下限0.15）
            (self.threshold_linear / peak).clamp(0.15, 1.0)
        } else if crest_factor > self.crest_mild {
            // 中等爆发 → 轻微压缩（下限0.7，几乎不压脚步声）
            (self.threshold_linear / peak).clamp(0.7, 1.0)
        } else {
            1.0
        };

        let coeff = if target_gain < self.adaptive_gain { 0.3 } else { 0.01 };
        self.adaptive_gain = self.adaptive_gain * (1.0 - coeff) + target_gain * coeff;
        self.adaptive_gain = self.adaptive_gain.clamp(0.0, 1.0);

        sample * self.adaptive_gain
    }
}