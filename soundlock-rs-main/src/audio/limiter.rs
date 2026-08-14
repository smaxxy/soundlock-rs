use crate::config::Config;
use std::sync::{Arc, Mutex};

/// ============================================================
/// PUBG Sound Lock 参数
/// ============================================================
///
/// RMS：负责控制持续性的高音量声音。
///
/// Peak：负责保护枪声、手雷、爆炸等突然出现的高峰值。
///
/// 目前 Peak 不提供 UI 调节，先固定为 -3 dBFS。
const PEAK_THRESHOLD_DB: f32 = -3.0;

/// Peak 保护反应时间。
///
/// 1 ms 对 PUBG 的枪声/爆炸等瞬态比较合适。
/// 它比 RMS 的 attack 快很多，但不会用极端的 0 ms 硬削波。
const PEAK_ATTACK_MS: u32 = 1;

pub struct LoudnessLimiter {
    config: Arc<Mutex<Config>>,

    /// 当前真实音频采样率
    sample_rate: f32,

    // ========================================================
    // RMS 全频控制
    // ========================================================

    /// RMS 阈值
    threshold_linear: f32,

    /// RMS attack 系数
    attack_coeff: f32,

    /// RMS release 系数
    release_coeff: f32,

    /// 全频 RMS 状态
    fullband_rms: f32,

    /// RMS 增益平滑
    gain_smoother: f32,

    // ========================================================
    // Peak 瞬态保护
    // ========================================================

    /// Peak 阈值
    peak_threshold_linear: f32,

    /// Peak attack 系数
    peak_attack_coeff: f32,

    /// Peak 当前增益
    peak_gain_smoother: f32,
}

impl LoudnessLimiter {
    pub fn new(config: Arc<Mutex<Config>>) -> Self {
        // 从配置读取初始参数
        let (threshold_db, attack_ms, release_ms) = config
            .try_lock()
            .map(|cfg| {
                (
                    cfg.threshold_db,
                    cfg.attack_ms,
                    cfg.release_ms,
                )
            })
            .unwrap_or((-16.0, 10, 50));

        // 默认初始化采样率
        let sample_rate = 48000.0;

        // ========================================================
        // RMS 参数
        // ========================================================

        let threshold_linear =
            10.0f32.powf(threshold_db / 20.0);

        let attack_coeff =
            Self::calc_coeff(attack_ms, sample_rate);

        let release_coeff =
            Self::calc_coeff(release_ms, sample_rate);

        // ========================================================
        // Peak 参数
        // ========================================================

        let peak_threshold_linear =
            10.0f32.powf(PEAK_THRESHOLD_DB / 20.0);

        let peak_attack_coeff =
            Self::calc_coeff(PEAK_ATTACK_MS, sample_rate);

        Self {
            config,
            sample_rate,

            // RMS
            threshold_linear,
            attack_coeff,
            release_coeff,
            fullband_rms: 0.0,
            gain_smoother: 1.0,

            // Peak
            peak_threshold_linear,
            peak_attack_coeff,
            peak_gain_smoother: 1.0,
        }
    }

    /// 设置真实采样率。
    ///
    /// 这里不仅更新 sample_rate，
    /// 还会重新计算 RMS 和 Peak 的时间系数。
    pub fn set_sample_rate(&mut self, sr: f32) {
        self.sample_rate = sr;

        if let Ok(cfg) = self.config.try_lock() {
            self.attack_coeff =
                Self::calc_coeff(cfg.attack_ms, sr);

            self.release_coeff =
                Self::calc_coeff(cfg.release_ms, sr);
        }

        // Peak attack 也必须根据采样率重新计算。
        self.peak_attack_coeff =
            Self::calc_coeff(PEAK_ATTACK_MS, sr);
    }

    /// 每秒同步一次配置参数。
    pub fn update_parameters(&mut self) {
        if let Ok(cfg) = self.config.try_lock() {
            self.threshold_linear =
                10.0f32.powf(cfg.threshold_db / 20.0);

            self.attack_coeff =
                Self::calc_coeff(
                    cfg.attack_ms,
                    self.sample_rate,
                );

            self.release_coeff =
                Self::calc_coeff(
                    cfg.release_ms,
                    self.sample_rate,
                );
        }
    }

    /// 每个音频 sample 调用一次。
    ///
    /// 当前结构：
    ///
    ///     输入
    ///       │
    ///       ├── RMS 检测 ──→ RMS Gain
    ///       │
    ///       └── Peak 检测 → Peak Gain
    ///                         │
    ///                         ↓
    ///                  取更严格的 Gain
    ///                         │
    ///                         ↓
    ///                       输出
    ///
    pub fn process_sample(&mut self, sample: f32) -> f32 {
        // ========================================================
        // 1. RMS 检测
        // ========================================================

        let alpha = 0.002;

        self.fullband_rms =
            alpha * sample * sample
                + (1.0 - alpha) * self.fullband_rms;

        let rms = self.fullband_rms.sqrt();

        // RMS 目标增益
        let rms_target_gain = if rms <= 0.0 {
            1.0
        } else {
            (self.threshold_linear / rms).min(1.0)
        };

        // RMS 根据增益变化方向选择 attack / release
        let rms_coeff =
            if rms_target_gain < self.gain_smoother {
                self.attack_coeff
            } else {
                self.release_coeff
            };

        // RMS 一阶平滑
        self.gain_smoother =
            rms_target_gain * (1.0 - rms_coeff)
                + self.gain_smoother * rms_coeff;

        self.gain_smoother =
            self.gain_smoother.clamp(0.0, 1.0);

        // ========================================================
        // 2. Peak 检测
        // ========================================================

        let peak = sample.abs();

        // Peak 超过 -3 dBFS 时开始需要衰减。
        let peak_target_gain =
            if peak <= 0.0 {
                1.0
            } else {
                (self.peak_threshold_linear / peak)
                    .min(1.0)
            };

        // Peak attack 非常快。
        //
        // release 沿用 RMS release，让枪声结束后
        // 不会突然猛地把增益拉回来。
        let peak_coeff =
            if peak_target_gain < self.peak_gain_smoother {
                self.peak_attack_coeff
            } else {
                self.release_coeff
            };

        // Peak 增益平滑
        self.peak_gain_smoother =
            peak_target_gain * (1.0 - peak_coeff)
                + self.peak_gain_smoother * peak_coeff;

        self.peak_gain_smoother =
            self.peak_gain_smoother.clamp(0.0, 1.0);

        // ========================================================
        // 3. RMS + Peak 双层保护
        // ========================================================

        // 哪个要求压得更多，就采用哪个。
        let final_gain =
            self.gain_smoother.min(self.peak_gain_smoother);

        let processed = sample * final_gain;

        // ========================================================
        // 4. 最终 Peak Ceiling
        // ========================================================
        //
        // 正常情况下通常不会触发。
        //
        // 它只是最后一道保险，防止极端瞬态在 Peak
        // 平滑器还没完全反应过来之前超过阈值。
        processed.clamp(
            -self.peak_threshold_linear,
            self.peak_threshold_linear,
        )
    }

    /// 根据时间常数计算一阶滤波器系数。
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