use crate::config::{
    LimiterParams,
    RuntimeLimiterParams,
};
use std::sync::Arc;

const PEAK_THRESHOLD_DB: f32 = -3.0;
const RMS_DETECTOR_MS: u32 = 10;
const RMS_KNEE_DB: f32 = 6.0;
const PEAK_ATTACK_MS: u32 = 1;
const LOOKAHEAD_MS: u32 = 5;
const PRE_GAIN_SMOOTH_MS: u32 = 20;

pub struct LoudnessLimiter {
    // Runtime 参数
    runtime_params: Arc<RuntimeLimiterParams>,
    current_params: LimiterParams,
    last_parameter_version: u64,

    sample_rate: f32,

    // Pre-Gain
    pre_gain_target_linear: f32,
    pre_gain_smoother: f32,
    pre_gain_smooth_coeff: f32,

    // RMS
    threshold_db: f32,
    attack_coeff: f32,
    release_coeff: f32,
    rms_detector_coeff: f32,
    fullband_rms: f32,
    gain_smoother: f32,

    // Peak
    peak_threshold_linear: f32,
    peak_attack_coeff: f32,
    peak_release_coeff: f32,
    peak_gain_smoother: f32,

    peak_hold_gain: f32,
    peak_hold_counter: usize,

    // Lookahead
    lookahead_samples: usize,
    lookahead_l: Vec<f32>,
    lookahead_r: Vec<f32>,
    lookahead_pos: usize,
}

impl LoudnessLimiter {
    pub fn new(
        runtime_params: Arc<RuntimeLimiterParams>,
    ) -> Self {
        let params =
            runtime_params
                .snapshot()
                .sanitized();

        let sample_rate =
            48_000.0f32;

        let pre_gain_linear =
            Self::db_to_linear(
                params.pre_gain_db,
            );

        let lookahead_samples =
            Self::calc_lookahead_samples(
                LOOKAHEAD_MS,
                sample_rate,
            );

        Self {
            runtime_params,
            current_params: params,

            // 强制首个回调同步参数，避免 snapshot 与 version 之间的发布竞态。
            last_parameter_version:
                u64::MAX,

            sample_rate,

            pre_gain_target_linear:
                pre_gain_linear,

            pre_gain_smoother:
                pre_gain_linear,

            pre_gain_smooth_coeff:
                Self::calc_coeff(
                    PRE_GAIN_SMOOTH_MS,
                    sample_rate,
                ),

            threshold_db:
                params.threshold_db,

            attack_coeff:
                Self::calc_coeff(
                    params.attack_ms,
                    sample_rate,
                ),

            release_coeff:
                Self::calc_coeff(
                    params.release_ms,
                    sample_rate,
                ),

            rms_detector_coeff:
                Self::calc_coeff(
                    RMS_DETECTOR_MS,
                    sample_rate,
                ),

            fullband_rms:
                0.0,

            gain_smoother:
                1.0,

            peak_threshold_linear:
                Self::db_to_linear(
                    PEAK_THRESHOLD_DB,
                ),

            peak_attack_coeff:
                Self::calc_coeff(
                    PEAK_ATTACK_MS,
                    sample_rate,
                ),

            peak_release_coeff:
                Self::calc_coeff(
                    params.peak_release_ms,
                    sample_rate,
                ),

            peak_gain_smoother:
                1.0,

            peak_hold_gain:
                1.0,

            peak_hold_counter:
                0,

            lookahead_samples,

            lookahead_l:
                vec![
                    0.0;
                    lookahead_samples
                ],

            lookahead_r:
                vec![
                    0.0;
                    lookahead_samples
                ],

            lookahead_pos:
                0,
        }
    }

    /// 每个 Audio Callback 开头调用一次
    ///
    /// 不锁、不等待、不分配；参数未变化时只做少量 Atomic load。
    #[inline]
    pub fn begin_audio_callback(
        &mut self,
    ) {
        let mut version =
            self.last_parameter_version;

        if let Some(params) =
            self.runtime_params
                .load_if_changed(
                    &mut version,
                )
        {
            self.last_parameter_version =
                version;

            self.apply_runtime_params(
                params,
            );
        }
    }

    #[inline]
    fn apply_runtime_params(
        &mut self,
        params: LimiterParams,
    ) {
        let params =
            params.sanitized();

        self.current_params =
            params;

        self.threshold_db =
            params.threshold_db;

        self.attack_coeff =
            Self::calc_coeff(
                params.attack_ms,
                self.sample_rate,
            );

        self.release_coeff =
            Self::calc_coeff(
                params.release_ms,
                self.sample_rate,
            );

        self.peak_release_coeff =
            Self::calc_coeff(
                params.peak_release_ms,
                self.sample_rate,
            );

        // 只修改 target，由 20ms smoother 避免 UI 调参时瞬间跳变。
        self.pre_gain_target_linear =
            Self::db_to_linear(
                params.pre_gain_db,
            );
    }

    /// 设置真实采样率
    ///
    /// 应在 Stream 创建前调用。该方法会重建 Lookahead Buffer，不可在实时输出中调用。
    pub fn set_sample_rate(
        &mut self,
        sr: f32,
    ) {
        if !sr.is_finite()
            || sr < 8000.0
        {
            return;
        }

        self.sample_rate =
            sr;

        let params =
            self.current_params
                .sanitized();

        self.current_params =
            params;

        self.threshold_db =
            params.threshold_db;

        self.attack_coeff =
            Self::calc_coeff(
                params.attack_ms,
                sr,
            );

        self.release_coeff =
            Self::calc_coeff(
                params.release_ms,
                sr,
            );

        self.peak_release_coeff =
            Self::calc_coeff(
                params.peak_release_ms,
                sr,
            );

        self.pre_gain_target_linear =
            Self::db_to_linear(
                params.pre_gain_db,
            );

        self.pre_gain_smooth_coeff =
            Self::calc_coeff(
                PRE_GAIN_SMOOTH_MS,
                sr,
            );

        self.rms_detector_coeff =
            Self::calc_coeff(
                RMS_DETECTOR_MS,
                sr,
            );

        self.peak_attack_coeff =
            Self::calc_coeff(
                PEAK_ATTACK_MS,
                sr,
            );

        self.lookahead_samples =
            Self::calc_lookahead_samples(
                LOOKAHEAD_MS,
                sr,
            );

        self.lookahead_l =
            vec![
                0.0;
                self.lookahead_samples
            ];

        self.lookahead_r =
            vec![
                0.0;
                self.lookahead_samples
            ];

        self.lookahead_pos =
            0;

        self.peak_hold_gain =
            1.0;

        self.peak_hold_counter =
            0;
    }

    /// 处理一个 Stereo Frame
    ///
    /// 本函数不主动同步 Runtime 参数；生产路径须在每个 callback 开头调用
    /// `begin_audio_callback()`。
    #[inline]
    pub fn process_stereo_frame(
        &mut self,
        left: f32,
        right: f32,
    ) -> (f32, f32) {
        let left =
            if left.is_finite() {
                left
            } else {
                0.0
            };

        let right =
            if right.is_finite() {
                right
            } else {
                0.0
            };

        // 1. Pre-Gain
        self.pre_gain_smoother =
            self.pre_gain_target_linear
                * (
                    1.0
                        - self
                            .pre_gain_smooth_coeff
                )
                + self
                    .pre_gain_smoother
                    * self
                        .pre_gain_smooth_coeff;

        let boosted_left =
            left
                * self.pre_gain_smoother;

        let boosted_right =
            right
                * self.pre_gain_smoother;

        // 2. Stereo Linked RMS
        // 使用左右较大的 energy，
        // 避免单边强信号被平均掉。
        let energy =
            (
                boosted_left
                    * boosted_left
            )
                .max(
                    boosted_right
                        * boosted_right,
                );

        self.fullband_rms =
            energy
                * (
                    1.0
                        - self
                            .rms_detector_coeff
                )
                + self
                    .fullband_rms
                    * self
                        .rms_detector_coeff;

        let rms =
            self.fullband_rms
                .max(0.0)
                .sqrt();

        let rms_db =
            Self::linear_to_db(
                rms,
            );

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
                * (
                    1.0
                        - rms_coeff
                )
                + self
                    .gain_smoother
                    * rms_coeff;

        self.gain_smoother =
            self.gain_smoother
                .clamp(
                    0.0,
                    1.0,
                );

        // 3. Stereo Linked Peak
        let peak =
            boosted_left
                .abs()
                .max(
                    boosted_right
                        .abs(),
                );

        let peak_target_gain =
            if peak <= 0.0 {
                1.0
            } else {
                (
                    self
                        .peak_threshold_linear
                        / peak
                )
                    .min(1.0)
            };

        // 4. Peak Hold
        if peak_target_gain
            < 1.0
        {
            // Hold 内只允许压得更多，
            // 不能被后续较小 Peak 提前放回来。
            self.peak_hold_gain =
                self.peak_hold_gain
                    .min(
                        peak_target_gain,
                    );

            self.peak_hold_counter =
                self.lookahead_samples;
        } else if self.peak_hold_counter
            > 0
        {
            self.peak_hold_counter -=
                1;
        } else {
            self.peak_hold_gain =
                1.0;
        }

        // 5. Peak Attack / Release
        let peak_control_target =
            self.peak_hold_gain;

        let peak_coeff =
            if peak_control_target
                < self
                    .peak_gain_smoother
            {
                self.peak_attack_coeff
            } else {
                self.peak_release_coeff
            };

        self.peak_gain_smoother =
            peak_control_target
                * (
                    1.0
                        - peak_coeff
                )
                + self
                    .peak_gain_smoother
                    * peak_coeff;

        self.peak_gain_smoother =
            self.peak_gain_smoother
                .clamp(
                    0.0,
                    1.0,
                );

        // 6. RMS + Peak
        let final_gain =
            self.gain_smoother
                .min(
                    self
                        .peak_gain_smoother,
                );

        // 7. 5ms Lookahead
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
        ] = boosted_left;

        self.lookahead_r[
            self.lookahead_pos
        ] = boosted_right;

        self.lookahead_pos +=
            1;

        if self.lookahead_pos
            >= self.lookahead_samples
        {
            self.lookahead_pos =
                0;
        }

        // 8. 应用 Linked Gain
        let processed_left =
            delayed_left
                * final_gain;

        let processed_right =
            delayed_right
                * final_gain;

        // 9. Final Ceiling Clamp
        let output_left =
            processed_left
                .clamp(
                    -self
                        .peak_threshold_linear,
                    self
                        .peak_threshold_linear,
                );

        let output_right =
            processed_right
                .clamp(
                    -self
                        .peak_threshold_linear,
                    self
                        .peak_threshold_linear,
                );

        (
            output_left,
            output_right,
        )
    }

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
            if level_db
                <= threshold_db
            {
                return 1.0;
            }

            return Self::db_to_linear(
                threshold_db
                    - level_db,
            );
        }

        let half_knee =
            knee_db * 0.5;

        let knee_start =
            threshold_db
                - half_knee;

        let knee_end =
            threshold_db
                + half_knee;

        let gain_db =
            if level_db
                <= knee_start
            {
                0.0
            } else if level_db
                >= knee_end
            {
                threshold_db
                    - level_db
            } else {
                let x =
                    level_db
                        - knee_start;

                -(x * x)
                    / (
                        2.0
                            * knee_db
                    )
            };

        Self::db_to_linear(
            gain_db,
        )
            .clamp(
                0.0,
                1.0,
            )
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
                .round()
                as usize;

        samples.max(1)
    }

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
            (
                -1.0
                    / samples
            )
                .exp()
        }
    }

    #[inline]
    fn db_to_linear(
        db: f32,
    ) -> f32 {
        10.0f32.powf(
            db / 20.0,
        )
    }

    #[inline]
    fn linear_to_db(
        value: f32,
    ) -> f32 {
        if value <= 0.000001 {
            -120.0
        } else {
            20.0
                * value.log10()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LimiterParams, RuntimeLimiterParams};
    use std::sync::Arc;

    fn test_params() -> LimiterParams {
        LimiterParams {
            threshold_db: 0.0,
            attack_ms: 10,
            release_ms: 50,
            pre_gain_db: 0.0,
            peak_release_ms: 50,
        }
    }

    fn make_limiter(sample_rate: f32, params: LimiterParams) -> LoudnessLimiter {
        let runtime = Arc::new(RuntimeLimiterParams::from_params(params));
        let mut limiter = LoudnessLimiter::new(runtime);
        limiter.set_sample_rate(sample_rate);
        limiter.begin_audio_callback();
        limiter
    }

    #[test]
    fn startup_lookahead_outputs_silence_then_audio() {
        let mut limiter = make_limiter(48_000.0, test_params());
        let n = limiter.lookahead_samples;

        for _ in 0..n {
            let (l, r) = limiter.process_stereo_frame(0.5, 0.25);
            assert_eq!(l, 0.0);
            assert_eq!(r, 0.0);
        }

        let (l, r) = limiter.process_stereo_frame(0.5, 0.25);
        assert!(l.abs() > 0.0);
        assert!(r.abs() > 0.0);
    }

    #[test]
    fn ceiling_is_respected_at_common_sample_rates() {
        for sample_rate in [44_100.0, 48_000.0, 96_000.0] {
            let mut params = test_params();
            params.pre_gain_db = 12.0;
            let mut limiter = make_limiter(sample_rate, params);

            let frames = limiter.lookahead_samples + 2_000;
            for _ in 0..frames {
                let (l, r) = limiter.process_stereo_frame(1.0, -1.0);
                assert!(
                    l.abs() <= limiter.peak_threshold_linear + 1e-6,
                    "left exceeded ceiling at {sample_rate} Hz: {l}"
                );
                assert!(
                    r.abs() <= limiter.peak_threshold_linear + 1e-6,
                    "right exceeded ceiling at {sample_rate} Hz: {r}"
                );
            }
        }
    }

    #[test]
    fn non_finite_input_never_reaches_output() {
        let mut limiter = make_limiter(48_000.0, test_params());

        for index in 0..2_000 {
            let left = match index % 3 {
                0 => f32::NAN,
                1 => f32::INFINITY,
                _ => f32::NEG_INFINITY,
            };

            let right = match index % 3 {
                0 => f32::NEG_INFINITY,
                1 => f32::NAN,
                _ => f32::INFINITY,
            };

            let (l, r) = limiter.process_stereo_frame(left, right);
            assert!(l.is_finite());
            assert!(r.is_finite());
            assert!(l.abs() <= limiter.peak_threshold_linear + 1e-6);
            assert!(r.abs() <= limiter.peak_threshold_linear + 1e-6);
        }
    }

    #[test]
    fn stereo_ratio_is_preserved_when_no_ceiling_clamp_occurs() {
        let mut limiter = make_limiter(48_000.0, test_params());
        let frames = limiter.lookahead_samples + 1_000;
        let mut last = (0.0, 0.0);

        for _ in 0..frames {
            last = limiter.process_stereo_frame(0.2, 0.1);
        }

        assert!(last.1.abs() > 1e-6);
        let ratio = last.0 / last.1;
        assert!((ratio - 2.0).abs() < 1e-4, "ratio={ratio}");
    }

    #[test]
    fn one_sided_peak_uses_linked_gain_for_both_channels() {
        let mut limiter = make_limiter(48_000.0, test_params());

        // 先让 Lookahead 内稳定存在右声道低电平信号。
        for _ in 0..(limiter.lookahead_samples + 500) {
            let _ = limiter.process_stereo_frame(0.0, 0.1);
        }

        // 左边一个强瞬态，右边仍是 0.1。
        let _ = limiter.process_stereo_frame(1.0, 0.1);

        let mut delayed_impulse_frame = (0.0, 0.0);
        for _ in 0..limiter.lookahead_samples {
            delayed_impulse_frame = limiter.process_stereo_frame(0.0, 0.1);
        }

        assert!(
            delayed_impulse_frame.0.abs() <= limiter.peak_threshold_linear + 1e-5
        );
        assert!(
            delayed_impulse_frame.1.abs() < 0.095,
            "right channel was not linked strongly enough: {}",
            delayed_impulse_frame.1
        );
    }

    #[test]
    fn peak_hold_is_not_relaxed_by_a_smaller_over_threshold_peak() {
        let mut limiter = make_limiter(48_000.0, test_params());

        let _ = limiter.process_stereo_frame(1.0, 0.0);
        let first_hold = limiter.peak_hold_gain;
        assert!(first_hold < 1.0);

        // 0.8 仍高于 -3 dBFS ceiling，但比 1.0 的峰值更小。
        let _ = limiter.process_stereo_frame(0.8, 0.0);
        let second_hold = limiter.peak_hold_gain;

        assert!(second_hold <= first_hold + 1e-6);
        assert_eq!(limiter.peak_hold_counter, limiter.lookahead_samples);
    }

    #[test]
    fn pre_gain_change_is_smoothed_not_stepped() {
        let runtime = Arc::new(RuntimeLimiterParams::from_params(test_params()));
        let mut limiter = LoudnessLimiter::new(Arc::clone(&runtime));
        limiter.set_sample_rate(48_000.0);
        limiter.begin_audio_callback();

        let new_params = LimiterParams {
            pre_gain_db: 12.0,
            ..test_params()
        };
        runtime.publish(new_params);
        limiter.begin_audio_callback();

        let before = limiter.pre_gain_smoother;
        let target = limiter.pre_gain_target_linear;
        assert!(target > before);

        let _ = limiter.process_stereo_frame(0.01, 0.01);
        let after_one = limiter.pre_gain_smoother;

        assert!(after_one > before);
        assert!(after_one < target);

        for _ in 0..48_000 {
            let _ = limiter.process_stereo_frame(0.01, 0.01);
        }

        assert!(limiter.pre_gain_smoother <= target + 1e-5);
        assert!((limiter.pre_gain_smoother - target).abs() < 1e-3);
    }
}
