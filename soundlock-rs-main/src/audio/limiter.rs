use crate::config::Config;
use std::f32::consts::PI;
use std::sync::{Arc, Mutex};

/// ============================================================
/// PUBG Sound Lock 参数
/// ============================================================

/// Peak 最终上限。
///
/// 枪声、爆炸等极端瞬态最终不会超过 -3 dBFS。
const PEAK_THRESHOLD_DB: f32 = -3.0;

/// RMS 检测器时间常数。
///
/// 注意：
/// 这不是 UI 中的 Attack。
/// 它控制 RMS 检测器观察声音能量变化的速度。
const RMS_DETECTOR_MS: u32 = 10;

/// RMS Soft Knee 宽度。
///
/// 6 dB 表示在阈值上下各约 3 dB 的范围内
/// 平滑进入限制状态，减少突然“压下去”的感觉。
const RMS_KNEE_DB: f32 = 6.0;

/// Peak Attack。
///
/// 配合 Lookahead 使用。
const PEAK_ATTACK_MS: u32 = 1;

/// Peak Release。
///
/// 与 RMS Release 完全独立。
const PEAK_RELEASE_MS: u32 = 50;

/// Lookahead 时间。
///
/// 48 kHz 下：
///
/// 2 ms = 96 个 stereo frame。
///
/// Limiter 可以提前看到枪声瞬态，
/// 在真正输出枪声之前先把 Gain 压下来。
const LOOKAHEAD_MS: u32 = 5;

/// ============================================================
/// 脚步增强参数
/// ============================================================

/// PUBG 中脚步、衣物摩擦等细节比较明显的存在感频段。
///
/// 这里不是把整个高频暴力拉起来，
/// 而是轻度突出约 2.5 kHz 附近。
const FOOTSTEP_EQ_FREQ_HZ: f32 = 2500.0;

/// Presence EQ 最大提升。
const FOOTSTEP_EQ_GAIN_DB: f32 = 0.5;

/// EQ Q 值。
///
/// 数值不高，让增强范围稍宽，
/// 避免声音过于尖锐。
const FOOTSTEP_EQ_Q: f32 = 0.9;

/// 小声音最大额外提升。
///
/// 与动态 EQ 配合时，
/// 脚步等较小声音会更容易听见。
const FOOTSTEP_UPWARD_GAIN_DB: f32 = 2.0;

/// 太安静时不增强。
///
/// 避免把非常低的底噪无限抬起来。
const DETAIL_MIN_DB: f32 = -60.0;

/// 这个音量附近脚步增强达到最大。
const DETAIL_FULL_DB: f32 = -38.0;

/// 到这个音量以后逐渐关闭脚步增强。
///
/// 枪声、爆炸等大声音不会继续吃这 3 dB + 3 dB 增强。
const DETAIL_OFF_DB: f32 = -20.0;
const DETAIL_PEAK_FULL_DB: f32 = -30.0;

/// Peak 到这个值以后，立即关闭当前帧的脚步增强
const DETAIL_PEAK_OFF_DB: f32 = -18.0;
/// 小声音出现后，脚步增强逐渐打开。
const DETAIL_ENABLE_MS: u32 = 30;

/// 突然出现枪声等大声音时，
/// 快速关闭脚步增强。
const DETAIL_DISABLE_MS: u32 = 5;
/// 短促 Peak 低于这个值时，不影响脚步增强

/// ============================================================
/// 简单 Biquad Peaking EQ
/// ============================================================

#[derive(Clone, Copy)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,

    z1: f32,
    z2: f32,
}

impl Biquad {
    fn new() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,

            z1: 0.0,
            z2: 0.0,
        }
    }

    /// 设置 Peaking EQ。
    ///
    /// 使用标准 RBJ Audio EQ Cookbook 形式。
    fn set_peaking(
        &mut self,
        sample_rate: f32,
        frequency: f32,
        q: f32,
        gain_db: f32,
    ) {
        if sample_rate <= 0.0 {
            return;
        }

        let frequency =
            frequency.clamp(20.0, sample_rate * 0.45);

        let a =
            10.0f32.powf(gain_db / 40.0);

        let omega =
            2.0 * PI * frequency / sample_rate;

        let sin_omega = omega.sin();
        let cos_omega = omega.cos();

        let alpha =
            sin_omega / (2.0 * q.max(0.01));

        let b0 =
            1.0 + alpha * a;

        let b1 =
            -2.0 * cos_omega;

        let b2 =
            1.0 - alpha * a;

        let a0 =
            1.0 + alpha / a;

        let a1 =
            -2.0 * cos_omega;

        let a2 =
            1.0 - alpha / a;

        self.b0 = b0 / a0;
        self.b1 = b1 / a0;
        self.b2 = b2 / a0;

        self.a1 = a1 / a0;
        self.a2 = a2 / a0;
    }

    #[inline]
    fn process(&mut self, sample: f32) -> f32 {
        // Transposed Direct Form II
        let output =
            self.b0 * sample + self.z1;

        self.z1 =
            self.b1 * sample
                - self.a1 * output
                + self.z2;

        self.z2 =
            self.b2 * sample
                - self.a2 * output;

        output
    }

    fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

/// ============================================================
/// Loudness Limiter
/// ============================================================

pub struct LoudnessLimiter {
    config: Arc<Mutex<Config>>,

    /// 当前真实采样率
    sample_rate: f32,

    // ========================================================
    // RMS 全频控制
    // ========================================================

    /// RMS 阈值 dBFS
    threshold_db: f32,

    /// RMS 阈值线性值
    threshold_linear: f32,

    /// RMS Attack
    attack_coeff: f32,

    /// RMS Release
    release_coeff: f32,

    /// RMS Detector 系数
    rms_detector_coeff: f32,

    /// RMS 能量状态
    fullband_rms: f32,

    /// RMS Gain
    gain_smoother: f32,

    // ========================================================
    // Peak 瞬态保护
    // ========================================================

    /// Peak 阈值
    peak_threshold_linear: f32,

    /// Peak Attack
    peak_attack_coeff: f32,

    /// Peak Release
    peak_release_coeff: f32,

    /// Peak Gain
    peak_gain_smoother: f32,

    /// Lookahead 期间需要保持的 Peak 目标增益。
    ///
    /// 主要用于非常短的枪声瞬态。
    peak_hold_gain: f32,

    /// Peak Hold 剩余 frame 数。
    peak_hold_counter: usize,

    // ========================================================
    // Lookahead
    // ========================================================

    /// Lookahead frame 数量。
    lookahead_samples: usize,

    /// 左声道延迟缓冲区
    lookahead_l: Vec<f32>,

    /// 右声道延迟缓冲区
    lookahead_r: Vec<f32>,

    /// 当前 ring buffer 位置
    lookahead_pos: usize,

    // ========================================================
    // 脚步 / 小声音增强
    // ========================================================

    /// 原始输入的 RMS 能量状态。
    ///
    /// 用它决定当前是否应该开启脚步增强。
    detail_rms: f32,

    /// 当前脚步增强程度：
    ///
    /// 0.0 = 完全关闭
    /// 1.0 = 最大增强
    detail_amount_smoother: f32,

    /// 小声音增强打开速度
    detail_enable_coeff: f32,

    /// 大声音出现时增强关闭速度
    detail_disable_coeff: f32,

    /// 左声道 Presence EQ
    footstep_eq_l: Biquad,

    /// 右声道 Presence EQ
    footstep_eq_r: Biquad,
}

impl LoudnessLimiter {
    pub fn new(config: Arc<Mutex<Config>>) -> Self {
        // ========================================================
        // 读取配置
        // ========================================================

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

        let sample_rate = 48000.0;

        // ========================================================
        // RMS
        // ========================================================

        let threshold_linear =
            Self::db_to_linear(threshold_db);

        let attack_coeff =
            Self::calc_coeff(
                attack_ms,
                sample_rate,
            );

        let release_coeff =
            Self::calc_coeff(
                release_ms,
                sample_rate,
            );

        let rms_detector_coeff =
            Self::calc_coeff(
                RMS_DETECTOR_MS,
                sample_rate,
            );

        // ========================================================
        // Peak
        // ========================================================

        let peak_threshold_linear =
            Self::db_to_linear(
                PEAK_THRESHOLD_DB,
            );

        let peak_attack_coeff =
            Self::calc_coeff(
                PEAK_ATTACK_MS,
                sample_rate,
            );

        let peak_release_coeff =
            Self::calc_coeff(
                PEAK_RELEASE_MS,
                sample_rate,
            );

        // ========================================================
        // Lookahead
        // ========================================================

        let lookahead_samples =
            Self::calc_lookahead_samples(
                LOOKAHEAD_MS,
                sample_rate,
            );

        let lookahead_l =
            vec![0.0; lookahead_samples];

        let lookahead_r =
            vec![0.0; lookahead_samples];

        // ========================================================
        // Detail / Footstep
        // ========================================================

        let detail_enable_coeff =
            Self::calc_coeff(
                DETAIL_ENABLE_MS,
                sample_rate,
            );

        let detail_disable_coeff =
            Self::calc_coeff(
                DETAIL_DISABLE_MS,
                sample_rate,
            );

        let mut footstep_eq_l =
            Biquad::new();

        let mut footstep_eq_r =
            Biquad::new();

        footstep_eq_l.set_peaking(
            sample_rate,
            FOOTSTEP_EQ_FREQ_HZ,
            FOOTSTEP_EQ_Q,
            FOOTSTEP_EQ_GAIN_DB,
        );

        footstep_eq_r.set_peaking(
            sample_rate,
            FOOTSTEP_EQ_FREQ_HZ,
            FOOTSTEP_EQ_Q,
            FOOTSTEP_EQ_GAIN_DB,
        );

        Self {
            config,

            sample_rate,

            // RMS
            threshold_db,
            threshold_linear,
            attack_coeff,
            release_coeff,
            rms_detector_coeff,
            fullband_rms: 0.0,
            gain_smoother: 1.0,

            // Peak
            peak_threshold_linear,
            peak_attack_coeff,
            peak_release_coeff,
            peak_gain_smoother: 1.0,
            peak_hold_gain: 1.0,
            peak_hold_counter: 0,

            // Lookahead
            lookahead_samples,
            lookahead_l,
            lookahead_r,
            lookahead_pos: 0,

            // Detail
            detail_rms: 0.0,
            detail_amount_smoother: 0.0,
            detail_enable_coeff,
            detail_disable_coeff,
            footstep_eq_l,
            footstep_eq_r,
        }
    }

    /// ========================================================
    /// 设置真实采样率
    /// ========================================================

    pub fn set_sample_rate(&mut self, sr: f32) {
        if !sr.is_finite() || sr < 8000.0 {
            return;
        }

        self.sample_rate = sr;

        // RMS UI 参数
        if let Ok(cfg) = self.config.try_lock() {
            self.attack_coeff =
                Self::calc_coeff(
                    cfg.attack_ms,
                    sr,
                );

            self.release_coeff =
                Self::calc_coeff(
                    cfg.release_ms,
                    sr,
                );
        }

        // RMS detector
        self.rms_detector_coeff =
            Self::calc_coeff(
                RMS_DETECTOR_MS,
                sr,
            );

        // Peak
        self.peak_attack_coeff =
            Self::calc_coeff(
                PEAK_ATTACK_MS,
                sr,
            );

        self.peak_release_coeff =
            Self::calc_coeff(
                PEAK_RELEASE_MS,
                sr,
            );

        // Detail
        self.detail_enable_coeff =
            Self::calc_coeff(
                DETAIL_ENABLE_MS,
                sr,
            );

        self.detail_disable_coeff =
            Self::calc_coeff(
                DETAIL_DISABLE_MS,
                sr,
            );

        // EQ
        self.footstep_eq_l.set_peaking(
            sr,
            FOOTSTEP_EQ_FREQ_HZ,
            FOOTSTEP_EQ_Q,
            FOOTSTEP_EQ_GAIN_DB,
        );

        self.footstep_eq_r.set_peaking(
            sr,
            FOOTSTEP_EQ_FREQ_HZ,
            FOOTSTEP_EQ_Q,
            FOOTSTEP_EQ_GAIN_DB,
        );

        self.footstep_eq_l.reset();
        self.footstep_eq_r.reset();

        // Lookahead
        self.lookahead_samples =
            Self::calc_lookahead_samples(
                LOOKAHEAD_MS,
                sr,
            );

        self.lookahead_l =
            vec![0.0; self.lookahead_samples];

        self.lookahead_r =
            vec![0.0; self.lookahead_samples];

        self.lookahead_pos = 0;

        self.peak_hold_gain = 1.0;
        self.peak_hold_counter = 0;
    }

    /// ========================================================
    /// 同步 UI 参数
    /// ========================================================

    pub fn update_parameters(&mut self) {
        if let Ok(cfg) = self.config.try_lock() {
            self.threshold_db =
                cfg.threshold_db;

            self.threshold_linear =
                Self::db_to_linear(
                    cfg.threshold_db,
                );

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

    /// ========================================================
    /// 处理一组 Stereo Frame
    /// ========================================================
    ///
    /// 必须：
    ///
    /// left  = 当前帧左声道
    /// right = 当前帧右声道
    ///
    /// 左右声道：
    /// - 分别做 EQ
    /// - 但是共用 RMS / Peak Detector
    /// - 共用最终 Gain
    ///
    /// 这样不会因为单边枪声导致左右声道压缩程度不同，
    /// 从而破坏 PUBG 方位感。
    #[inline]
    pub fn process_stereo_frame(
        &mut self,
        left: f32,
        right: f32,
    ) -> (f32, f32) {
        // ========================================================
        // 1. 检测原始声音大小
        // ========================================================

        let raw_energy =
            (left * left + right * right) * 0.5;

        self.detail_rms =
            raw_energy
                * (1.0 - self.rms_detector_coeff)
                + self.detail_rms
                    * self.rms_detector_coeff;

        let raw_rms =
            self.detail_rms.sqrt();

        let raw_db =
            Self::linear_to_db(raw_rms);
// 检测瞬时 Peak。
// RMS 对非常短的换弹声、金属点击声反应比较慢，
// 所以额外用 Peak 防止这些声音被脚步增强放大。
let raw_peak =
    left.abs().max(right.abs());

let raw_peak_db =
    Self::linear_to_db(raw_peak);
        // ========================================================
        // 2. 动态脚步增强 Amount
        // ========================================================

        let detail_target =
            Self::calc_detail_amount(raw_db);

        // 小声音出现：
        // 慢一点把细节增强打开。
        //
        // 突然来枪声：
        // 快速关闭增强。
        let detail_coeff =
            if detail_target
                > self.detail_amount_smoother
            {
                self.detail_enable_coeff
            } else {
                self.detail_disable_coeff
            };

        self.detail_amount_smoother =
            detail_target
                * (1.0 - detail_coeff)
                + self.detail_amount_smoother
                    * detail_coeff;

        self.detail_amount_smoother =
            self.detail_amount_smoother
                .clamp(0.0, 1.0);

       let peak_guard =
    Self::calc_detail_peak_guard(raw_peak_db);

let detail_amount =
    self.detail_amount_smoother
        * peak_guard;

        // ========================================================
        // 3. 动态 Presence EQ
        // ========================================================

        // EQ 始终运行，保证滤波器状态连续。
        //
        // 但只根据 detail_amount
        // 混入一部分 EQ 后的信号。
        let eq_left =
            self.footstep_eq_l.process(left);

        let eq_right =
            self.footstep_eq_r.process(right);

        let mut enhanced_left =
            left
                + (eq_left - left)
                    * detail_amount;

        let mut enhanced_right =
            right
                + (eq_right - right)
                    * detail_amount;

        // ========================================================
        // 4. 轻度 Upward Compression
        // ========================================================

        // 最大 +3 dB。
        //
        // 只在较小声音时生效。
        // 枪声一旦变大，detail_amount 会迅速下降。
        let upward_gain_db =
            FOOTSTEP_UPWARD_GAIN_DB
                * detail_amount;

        let upward_gain =
            Self::db_to_linear(
                upward_gain_db,
            );

        enhanced_left *= upward_gain;
        enhanced_right *= upward_gain;

        // ========================================================
        // 5. Stereo Linked RMS Detector
        // ========================================================

        let energy =
    (enhanced_left * enhanced_left)
        .max(enhanced_right * enhanced_right);

        self.fullband_rms =
            energy
                * (1.0 - self.rms_detector_coeff)
                + self.fullband_rms
                    * self.rms_detector_coeff;

        let rms =
            self.fullband_rms.sqrt();

        let rms_db =
            Self::linear_to_db(rms);

        // ========================================================
        // 6. RMS Soft Knee Limiter
        // ========================================================

        let rms_target_gain =
            Self::soft_knee_limiter_gain(
                rms_db,
                self.threshold_db,
                RMS_KNEE_DB,
            );

        let rms_coeff =
            if rms_target_gain
                < self.gain_smoother
            {
                self.attack_coeff
            } else {
                self.release_coeff
            };

        self.gain_smoother =
            rms_target_gain
                * (1.0 - rms_coeff)
                + self.gain_smoother
                    * rms_coeff;

        self.gain_smoother =
            self.gain_smoother
                .clamp(0.0, 1.0);

        // ========================================================
        // 7. Stereo Linked Peak Detector
        // ========================================================

        // 左右声道谁的 Peak 更大，就以谁为准。
        //
        // 但最终左右使用同一个 Gain。
        let peak =
            enhanced_left
                .abs()
                .max(enhanced_right.abs());

        let peak_target_gain =
            if peak <= 0.0 {
                1.0
            } else {
                (
                    self.peak_threshold_linear
                        / peak
                )
                    .min(1.0)
            };

        // ========================================================
        // 8. Peak Hold
        // ========================================================
        //
        // Lookahead 最大的问题是：
        //
        // 非常短的单个枪声 Peak
        // 可能只持续几个 sample。
        //
        // 所以发现 Peak 后，
        // 把它需要的 Gain 至少保持一个 Lookahead 窗口。
        if peak_target_gain < 1.0 {
    // Hold 期间只允许压得更多，
    // 不允许后面的较小 Peak 提前把 Gain 放回来。
    self.peak_hold_gain =
        self.peak_hold_gain.min(peak_target_gain);

    self.peak_hold_counter =
        self.lookahead_samples;
} else if self.peak_hold_counter > 0 {
    self.peak_hold_counter -= 1;
} else {
    self.peak_hold_gain = 1.0;
}

        let peak_control_target =
            self.peak_hold_gain;

        // ========================================================
        // 9. Peak Attack / Release
        // ========================================================

        let peak_coeff =
            if peak_control_target
                < self.peak_gain_smoother
            {
                self.peak_attack_coeff
            } else {
                self.peak_release_coeff
            };

        self.peak_gain_smoother =
            peak_control_target
                * (1.0 - peak_coeff)
                + self.peak_gain_smoother
                    * peak_coeff;

        self.peak_gain_smoother =
            self.peak_gain_smoother
                .clamp(0.0, 1.0);

        // ========================================================
        // 10. RMS + Peak
        // ========================================================

        let final_gain =
            self.gain_smoother
                .min(self.peak_gain_smoother);

        // ========================================================
        // 11. 5ms Lookahead
        // ========================================================
        //
        // 当前声音先进入 detector，
        // 但真正输出的是 5ms 以前的声音。
        //
        // 因此 Limiter 相当于提前 5ms
        // “知道”枪声马上要输出。
        let delayed_left =
            self.lookahead_l[
                self.lookahead_pos
            ];

        let delayed_right =
            self.lookahead_r[
                self.lookahead_pos
            ];

        self.lookahead_l[
            self.lookahead_pos
        ] = enhanced_left;

        self.lookahead_r[
            self.lookahead_pos
        ] = enhanced_right;

        self.lookahead_pos += 1;

        if self.lookahead_pos
            >= self.lookahead_samples
        {
            self.lookahead_pos = 0;
        }

        // ========================================================
        // 12. 应用 Linked Gain
        // ========================================================

        let processed_left =
            delayed_left * final_gain;

        let processed_right =
            delayed_right * final_gain;

        // ========================================================
        // 13. 最终 Ceiling
        // ========================================================
        //
        // Lookahead 加入以后，
        // 这里应该比旧版本少触发很多。
        //
        // 仍然保留作为最后保险。
        let output_left =
            processed_left.clamp(
                -self.peak_threshold_linear,
                self.peak_threshold_linear,
            );

        let output_right =
            processed_right.clamp(
                -self.peak_threshold_linear,
                self.peak_threshold_linear,
            );

        (output_left, output_right)
    }

    /// ========================================================
    /// 处理 interleaved Stereo buffer
    /// ========================================================
    ///
    /// 数据格式：
    ///
    /// L R L R L R L R
    ///
    /// 如果你的 callback 正好拿到这种 &mut [f32]，
    /// 可以直接调用这个函数。
    pub fn process_interleaved_stereo(
        &mut self,
        data: &mut [f32],
    ) {
        for frame in data.chunks_exact_mut(2) {
            let (left, right) =
                self.process_stereo_frame(
                    frame[0],
                    frame[1],
                );

            frame[0] = left;
            frame[1] = right;
        }
    }

    /// ========================================================
    /// Mono 兼容接口
    /// ========================================================
    ///
    /// 注意：
    ///
    /// PUBG Stereo 音频不要再左右声道分别调用这个函数。
    ///
    /// Stereo 必须调用 process_stereo_frame()
    /// 或 process_interleaved_stereo()。
    pub fn process_sample(
        &mut self,
        sample: f32,
    ) -> f32 {
        let (output, _) =
            self.process_stereo_frame(
                sample,
                sample,
            );

        output
    }

    /// ========================================================
    /// Soft Knee Limiter Gain
    /// ========================================================

    #[inline]
    fn soft_knee_limiter_gain(
        level_db: f32,
        threshold_db: f32,
        knee_db: f32,
    ) -> f32 {
        if !level_db.is_finite() {
            return 1.0;
        }

        if knee_db <= 0.0 {
            if level_db <= threshold_db {
                return 1.0;
            }

            return Self::db_to_linear(
                threshold_db - level_db,
            );
        }

        let half_knee =
            knee_db * 0.5;

        let knee_start =
            threshold_db - half_knee;

        let knee_end =
            threshold_db + half_knee;

        let gain_db =
            if level_db <= knee_start {
                0.0
            } else if level_db >= knee_end {
                threshold_db - level_db
            } else {
                // 无限压缩比 Limiter 的
                // 标准 quadratic soft knee。
                let x =
                    level_db - knee_start;

                -(x * x)
                    / (2.0 * knee_db)
            };

        Self::db_to_linear(gain_db)
            .clamp(0.0, 1.0)
    }

    /// ========================================================
    /// 根据当前响度计算脚步增强程度
    /// ========================================================

    #[inline]
    fn calc_detail_amount(
        level_db: f32,
    ) -> f32 {
        if !level_db.is_finite() {
            return 0.0;
        }

        // 极安静：
        // 不增强，避免提高底噪。
        if level_db <= DETAIL_MIN_DB {
            return 0.0;
        }

        // -60 → -38 dB：
        // 从 0 慢慢增加到最大。
        if level_db < DETAIL_FULL_DB {
            return (
                (level_db - DETAIL_MIN_DB)
                    / (
                        DETAIL_FULL_DB
                            - DETAIL_MIN_DB
                    )
            )
                .clamp(0.0, 1.0);
        }

        // -38 → -20 dB：
        // 从最大逐渐降低到 0。
        if level_db < DETAIL_OFF_DB {
            return (
                (
                    DETAIL_OFF_DB
                        - level_db
                )
                    / (
                        DETAIL_OFF_DB
                            - DETAIL_FULL_DB
                    )
            )
                .clamp(0.0, 1.0);
        }

        // 大声：
        // 完全关闭脚步增强。
        0.0
    }

    /// ========================================================
    /// Lookahead frame 数
    /// ========================================================
#[inline]
fn calc_detail_peak_guard(
    peak_db: f32,
) -> f32 {
    if !peak_db.is_finite() {
        return 0.0;
    }

    // 小 Peak：完全允许脚步增强
    if peak_db <= DETAIL_PEAK_FULL_DB {
        return 1.0;
    }

    // 大 Peak：完全禁止脚步增强
    if peak_db >= DETAIL_PEAK_OFF_DB {
        return 0.0;
    }

    // -24dB → -12dB
    // 从 1.0 平滑下降到 0.0
    (
        (DETAIL_PEAK_OFF_DB - peak_db)
            / (
                DETAIL_PEAK_OFF_DB
                    - DETAIL_PEAK_FULL_DB
            )
    )
        .clamp(0.0, 1.0)
}
    fn calc_lookahead_samples(
        time_ms: u32,
        sr: f32,
    ) -> usize {
        let samples =
            (
                sr
                    * time_ms as f32
                    / 1000.0
            )
                .round() as usize;

        samples.max(1)
    }

    /// ========================================================
    /// 时间常数 → 一阶滤波系数
    /// ========================================================

    fn calc_coeff(
        time_ms: u32,
        sr: f32,
    ) -> f32 {
        let time_s =
            time_ms as f32
                / 1000.0;

        let samples =
            time_s * sr;

        if samples <= 0.0 {
            0.0
        } else {
            (-1.0 / samples).exp()
        }
    }

    /// dB → linear
    #[inline]
    fn db_to_linear(
        db: f32,
    ) -> f32 {
        10.0f32.powf(
            db / 20.0,
        )
    }

    /// linear → dB
    #[inline]
    fn linear_to_db(
        value: f32,
    ) -> f32 {
        if value <= 0.000001 {
            -120.0
        } else {
            20.0 * value.log10()
        }
    }
}