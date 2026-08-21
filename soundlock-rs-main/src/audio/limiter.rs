use crate::config::{
    LimiterParams,
    RuntimeLimiterParams,
};
use std::sync::Arc;

/// ============================================================
/// 固定 DSP 参数
/// ============================================================

/// 最终 Peak Ceiling。
const PEAK_THRESHOLD_DB: f32 = -3.0;

/// RMS Detector 时间常数。
///
/// 固定，不放 UI。
const RMS_DETECTOR_MS: u32 = 10;

/// RMS Soft Knee。
///
/// 固定，不放 UI。
const RMS_KNEE_DB: f32 = 6.0;

/// Peak Attack。
///
/// 配合 5ms Lookahead 使用。
const PEAK_ATTACK_MS: u32 = 1;

/// Lookahead 时间。
const LOOKAHEAD_MS: u32 = 5;

/// Pre-Gain 参数改变后的平滑时间。
///
/// 防止 UI 拖动 Pre-Gain 时产生突变 / click。
const PRE_GAIN_SMOOTH_MS: u32 = 20;

/// Gain 低于这个值时，
/// Diagnostics 认为 Limiter 正在工作。
///
/// 0.999 约等于 -0.0087 dB。
const LIMIT_ACTIVE_GAIN: f32 = 0.999;

/// Limiter 本地累计多少 Stereo Frame
/// 后批量提交一次 Diagnostics。
///
/// 避免每个 Sample 都写全局 Atomic。
const DIAGNOSTIC_FLUSH_FRAMES: u64 = 4096;

/// ============================================================
/// Loudness Limiter
/// ============================================================
///
/// 最终职责：
///
/// RuntimeLimiterParams
///         ↓
///      Pre-Gain
///         ↓
/// Stereo Linked RMS
///         ↓
/// RMS Soft Knee Limiter
///         ↓
/// Stereo Linked Peak
///         ↓
/// Peak Hold
///         ↓
/// Peak Attack / Release
///         ↓
/// RMS / Peak 取更严格 Gain
///         ↓
///     5ms Lookahead
///         ↓
///    -3 dBFS Ceiling
///
/// ============================================================
///
/// 重要：
///
/// LoudnessLimiter 不再持有：
///
/// Arc<Mutex<Config>>
///
/// Audio Callback 不会：
///
/// - Mutex
/// - RwLock
/// - try_lock
/// - 等待 UI
///
/// UI 参数通过 RuntimeLimiterParams
/// 使用 Atomic + version 发布。
pub struct LoudnessLimiter {
    // ========================================================
    // 实时参数通道
    // ========================================================

    /// UI / 控制线程发布参数的无锁通道。
    runtime_params:
        Arc<RuntimeLimiterParams>,

    /// Limiter 当前正在使用的参数快照。
    ///
    /// set_sample_rate() 重新计算系数时直接使用这里，
    /// 不需要访问 Config。
    current_params:
        LimiterParams,

    /// 上一次成功同步的参数版本。
    ///
    /// 初始化为 u64::MAX，
    /// 强制第一次 Audio Callback
    /// 再读取一次 RuntimeLimiterParams。
    last_parameter_version: u64,

    // ========================================================
    // Sample Rate
    // ========================================================

    sample_rate: f32,

    // ========================================================
    // Pre-Gain
    // ========================================================

    /// 当前目标 Pre-Gain。
    pre_gain_target_linear: f32,

    /// 实际正在使用的平滑后 Pre-Gain。
    pre_gain_smoother: f32,

    /// Pre-Gain 20ms 平滑系数。
    pre_gain_smooth_coeff: f32,

    // ========================================================
    // RMS
    // ========================================================

    /// 当前 RMS Threshold。
    threshold_db: f32,

    /// RMS Gain Attack。
    attack_coeff: f32,

    /// RMS Gain Release。
    release_coeff: f32,

    /// 固定 10ms RMS Detector。
    rms_detector_coeff: f32,

    /// Stereo Linked RMS 能量状态。
    fullband_rms: f32,

    /// RMS Limiter 当前 Gain。
    gain_smoother: f32,

    // ========================================================
    // Peak
    // ========================================================

    /// -3 dBFS Peak Ceiling 线性值。
    peak_threshold_linear: f32,

    /// Peak 1ms Attack。
    peak_attack_coeff: f32,

    /// Peak Release。
    ///
    /// 来自 UI：
    /// 20 ~ 150 ms。
    peak_release_coeff: f32,

    /// Peak Limiter 当前 Gain。
    peak_gain_smoother: f32,

    /// 当前 Hold 期间最严格的 Peak Gain。
    peak_hold_gain: f32,

    /// Peak Hold 剩余 Stereo Frame。
    peak_hold_counter: usize,

    /// 当前是否处于一次 Peak Hold Event。
    ///
    /// 用于 Diagnostics：
    /// 连续 Peak 只统计为一个事件。
    peak_hold_active: bool,

    // ========================================================
    // Lookahead
    // ========================================================

    /// Lookahead Stereo Frame 数量。
    lookahead_samples: usize,

    /// 左声道 Lookahead Ring Buffer。
    lookahead_l: Vec<f32>,

    /// 右声道 Lookahead Ring Buffer。
    lookahead_r: Vec<f32>,

    /// Ring Buffer 当前位置。
    lookahead_pos: usize,

    // ========================================================
    // 本地 Diagnostics
    // ========================================================
    //
    // 先在 Limiter 本地累积，
    // 每 4096 frame 再批量提交，
    // 避免实时线程每帧频繁 Atomic 操作。

    diagnostic_processed_frames: u64,

    diagnostic_ceiling_hit_frames: u64,

    diagnostic_peak_limit_frames: u64,

    diagnostic_peak_hold_events: u64,

    diagnostic_peak_gain_min: f32,

    diagnostic_rms_gain_min: f32,
}

impl LoudnessLimiter {
    /// ========================================================
    /// 创建 Limiter
    /// ========================================================
    ///
    /// 注意：
    ///
    /// 构造参数已经从：
    ///
    /// Arc<Mutex<Config>>
    ///
    /// 改为：
    ///
    /// Arc<RuntimeLimiterParams>
    ///
    /// 下一步 audio/mod.rs 必须对应修改。
    pub fn new(
        runtime_params:
            Arc<RuntimeLimiterParams>,
    ) -> Self {
        // ====================================================
        // 初始化参数快照
        // ====================================================
        //
        // snapshot() 只发生在创建 Limiter 时，
        // 不属于 Audio Callback 实时路径。
        let params =
            runtime_params
                .snapshot()
                .sanitized();

        // 创建时临时使用 48kHz。
        //
        // Audio Stream 创建完成以后，
        // set_sample_rate() 会设置真实 SR。
        let sample_rate =
            48_000.0;

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
            // Runtime params
            runtime_params,

            current_params:
                params,

            // 故意不读取 runtime_params.version()。
            //
            // snapshot() 与 version() 如果分开调用，
            // 中间可能恰好发生一次 publish。
            //
            // 使用 MAX 可以保证第一次
            // begin_audio_callback()
            // 必定重新同步一次最新参数。
            last_parameter_version:
                u64::MAX,

            // Sample rate
            sample_rate,

            // Pre-Gain
            pre_gain_target_linear:
                pre_gain_linear,

            pre_gain_smoother:
                pre_gain_linear,

            pre_gain_smooth_coeff:
                Self::calc_coeff(
                    PRE_GAIN_SMOOTH_MS,
                    sample_rate,
                ),

            // RMS
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

            // Peak
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
                    params
                        .peak_release_ms,
                    sample_rate,
                ),

            peak_gain_smoother:
                1.0,

            peak_hold_gain:
                1.0,

            peak_hold_counter:
                0,

            peak_hold_active:
                false,

            // Lookahead
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

            // Diagnostics
            diagnostic_processed_frames:
                0,

            diagnostic_ceiling_hit_frames:
                0,

            diagnostic_peak_limit_frames:
                0,

            diagnostic_peak_hold_events:
                0,

            diagnostic_peak_gain_min:
                1.0,

            diagnostic_rms_gain_min:
                1.0,
        }
    }

    /// ========================================================
    /// Audio Callback 开始
    /// ========================================================
    ///
    /// 下一步 audio/mod.rs 中：
    ///
    /// 每次 Audio Callback 开头调用一次。
    ///
    /// 例如：
    ///
    /// limiter.begin_audio_callback();
    ///
    /// 然后再循环处理该 Callback
    /// 里面所有 Stereo Frame。
    ///
    /// ========================================================
    ///
    /// 正常情况下参数没有变化时：
    ///
    /// 这里只做一次 Atomic version load。
    ///
    /// 参数有变化时：
    ///
    /// - 读取新参数
    /// - 重新计算相关系数
    ///
    /// 不会：
    ///
    /// - Mutex
    /// - 等待
    /// - spin
    /// - 内存分配
    #[inline]
    pub fn begin_audio_callback(
        &mut self,
    ) {
        let mut version =
            self.last_parameter_version;

        if let Some(params) =
            self
                .runtime_params
                .load_if_changed(
                    &mut version,
                )
        {
            // 只有拿到了完整稳定快照，
            // 才更新版本和 Limiter 参数。
            self.last_parameter_version =
                version;

            self.apply_runtime_params(
                params,
            );
        }
    }

    /// ========================================================
    /// 应用新的 Runtime 参数
    /// ========================================================
    ///
    /// 只在 parameter_version 变化时调用。
    ///
    /// exp / powf 的计算因此不是每 Sample 执行。
    #[inline]
    fn apply_runtime_params(
        &mut self,
        params: LimiterParams,
    ) {
        let params =
            params.sanitized();

        self.current_params =
            params;

        // RMS Threshold
        self.threshold_db =
            params.threshold_db;

        // RMS Attack
        self.attack_coeff =
            Self::calc_coeff(
                params.attack_ms,
                self.sample_rate,
            );

        // RMS Release
        self.release_coeff =
            Self::calc_coeff(
                params.release_ms,
                self.sample_rate,
            );

        // Peak Release
        self.peak_release_coeff =
            Self::calc_coeff(
                params
                    .peak_release_ms,
                self.sample_rate,
            );

        // Pre-Gain 只改变 Target。
        //
        // 实际 pre_gain_smoother
        // 会继续按 20ms 平滑过去，
        // 防止滑块变化产生 click。
        self.pre_gain_target_linear =
            Self::db_to_linear(
                params.pre_gain_db,
            );
    }

    /// ========================================================
    /// 设置真实采样率
    /// ========================================================
    ///
    /// 这里彻底不访问 Config。
    ///
    /// 所有用户参数都从 current_params 重算。
    pub fn set_sample_rate(
        &mut self,
        sr: f32,
    ) {
        if !sr.is_finite()
            || sr < 8000.0
        {
            return;
        }

        self.sample_rate = sr;

        // ====================================================
        // 用户可调 RMS 参数
        // ====================================================

        self.threshold_db =
            self
                .current_params
                .threshold_db;

        self.attack_coeff =
            Self::calc_coeff(
                self
                    .current_params
                    .attack_ms,
                sr,
            );

        self.release_coeff =
            Self::calc_coeff(
                self
                    .current_params
                    .release_ms,
                sr,
            );

        // ====================================================
        // Pre-Gain
        // ====================================================

        self.pre_gain_target_linear =
            Self::db_to_linear(
                self
                    .current_params
                    .pre_gain_db,
            );

        self.pre_gain_smooth_coeff =
            Self::calc_coeff(
                PRE_GAIN_SMOOTH_MS,
                sr,
            );

        // 注意：
        //
        // 不把 pre_gain_smoother
        // 强行跳到 target。
        //
        // 保留当前值，
        // 避免采样率变化时额外产生 Gain Jump。

        // ====================================================
        // 固定 RMS Detector
        // ====================================================

        self.rms_detector_coeff =
            Self::calc_coeff(
                RMS_DETECTOR_MS,
                sr,
            );

        // ====================================================
        // Peak
        // ====================================================

        self.peak_attack_coeff =
            Self::calc_coeff(
                PEAK_ATTACK_MS,
                sr,
            );

        self.peak_release_coeff =
            Self::calc_coeff(
                self
                    .current_params
                    .peak_release_ms,
                sr,
            );

        // ====================================================
        // Lookahead
        // ====================================================
        //
        // 当前仍然维持原来的行为：
        //
        // SR 改变时重建 Lookahead Buffer。
        //
        // 如果未来支持运行中热切换采样率，
        // 这里会产生约一个 Lookahead 窗口的空数据。
        //
        // 这是已知的未来优化项，
        // 本轮不扩大修改范围。

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

        self.lookahead_pos = 0;

        // Lookahead 被清空以后，
        // Peak Hold 状态也必须同步清空。
        self.peak_hold_gain = 1.0;
        self.peak_hold_counter = 0;
        self.peak_hold_active = false;
    }

    /// ========================================================
    /// 处理一个 Stereo Frame
    /// ========================================================
    ///
    /// 重要：
    ///
    /// 这个函数本身不检查参数版本。
    ///
    /// 原因：
    ///
    /// 48kHz 下每秒调用约 48,000 次，
    /// 没必要每 Sample Atomic load。
    ///
    /// audio/mod.rs 必须在每个 callback 开始调用：
    ///
    /// begin_audio_callback()
    ///
    /// 然后再处理本 callback 的全部 frame。
    #[inline]
    pub fn process_stereo_frame(
        &mut self,
        left: f32,
        right: f32,
    ) -> (f32, f32) {
        // ====================================================
        // Diagnostics
        // ====================================================

        self.diagnostic_processed_frames += 1;

        // ====================================================
        // 1. 输入异常保护
        // ====================================================

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

        // ====================================================
        // 2. Pre-Gain 平滑
        // ====================================================

        self.pre_gain_smoother =
            self
                .pre_gain_target_linear
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
                * self
                    .pre_gain_smoother;

        let boosted_right =
            right
                * self
                    .pre_gain_smoother;

        // ====================================================
        // 3. Stereo Linked RMS Detector
        // ====================================================
        //
        // 使用左右声道较大的 Energy。
        //
        // 防止单侧枪声因为 L/R 平均
        // 被低估约 3dB。

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
            self
                .fullband_rms
                .max(0.0)
                .sqrt();

        let rms_db =
            Self::linear_to_db(
                rms,
            );

        // ====================================================
        // 4. RMS Soft Knee Limiter
        // ====================================================

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
            self
                .gain_smoother
                .clamp(
                    0.0,
                    1.0,
                );

        self.diagnostic_rms_gain_min =
            self
                .diagnostic_rms_gain_min
                .min(
                    self
                        .gain_smoother,
                );

        // ====================================================
        // 5. Stereo Linked Peak Detector
        // ====================================================

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

        // ====================================================
        // 6. Peak Hold
        // ====================================================
        //
        // Hold 期间保存最严格 Peak Gain。
        //
        // 后续较小 Peak
        // 不允许提前放松 Gain。

        if peak_target_gain < 1.0 {
            // 新的一次 Peak Event。
            if !self.peak_hold_active {
                self.diagnostic_peak_hold_events += 1;

                self.peak_hold_active = true;
            }

            self.peak_hold_gain =
                self
                    .peak_hold_gain
                    .min(
                        peak_target_gain,
                    );

            self.peak_hold_counter =
                self.lookahead_samples;
        } else if self.peak_hold_counter > 0 {
            self.peak_hold_counter -= 1;
        } else {
            self.peak_hold_gain = 1.0;

            self.peak_hold_active = false;
        }

        let peak_control_target =
            self.peak_hold_gain;

        // ====================================================
        // 7. Peak Attack / Release
        // ====================================================

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
            self
                .peak_gain_smoother
                .clamp(
                    0.0,
                    1.0,
                );

        // ====================================================
        // Peak Diagnostics
        // ====================================================

        if self.peak_gain_smoother
            < LIMIT_ACTIVE_GAIN
        {
            self.diagnostic_peak_limit_frames += 1;
        }

        self.diagnostic_peak_gain_min =
            self
                .diagnostic_peak_gain_min
                .min(
                    self
                        .peak_gain_smoother,
                );

        // ====================================================
        // 8. RMS / Peak 取更严格的 Gain
        // ====================================================

        let final_gain =
            self
                .gain_smoother
                .min(
                    self
                        .peak_gain_smoother,
                );

        // ====================================================
        // 9. 5ms Lookahead
        // ====================================================

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

        self.lookahead_pos += 1;

        if self.lookahead_pos
            >= self
                .lookahead_samples
        {
            self.lookahead_pos = 0;
        }

        // ====================================================
        // 10. 应用 Linked Gain
        // ====================================================

        let processed_left =
            delayed_left
                * final_gain;

        let processed_right =
            delayed_right
                * final_gain;

        // ====================================================
        // 11. Ceiling Diagnostics
        // ====================================================
        //
        // 一组 L/R 算一个 Stereo Frame。

        if processed_left.abs()
            > self
                .peak_threshold_linear
            || processed_right.abs()
                > self
                    .peak_threshold_linear
        {
            self.diagnostic_ceiling_hit_frames += 1;
        }

        // ====================================================
        // 12. 最终 -3dBFS Ceiling
        // ====================================================

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

        // ====================================================
        // Diagnostics 批量 Flush
        // ====================================================

        self.maybe_flush_diagnostics();

        (
            output_left,
            output_right,
        )
    }

    /// ========================================================
    /// 处理 Interleaved Stereo Buffer
    /// ========================================================
    ///
    /// L R L R L R...
    ///
    /// 这里本身就是一个 Buffer 级接口，
    /// 所以参数只在整个 Buffer 开始同步一次。
    pub fn process_interleaved_stereo(
        &mut self,
        data: &mut [f32],
    ) {
        self.begin_audio_callback();

        for frame in
            data.chunks_exact_mut(2)
        {
            let (
                left,
                right,
            ) =
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
    /// PUBG Stereo 不应通过这个接口
    /// 分别处理左右声道。
    ///
    /// 单独使用 Mono 接口时，
    /// 没有外层 Buffer Hook，
    /// 因此这里同步一次参数。
    pub fn process_sample(
        &mut self,
        sample: f32,
    ) -> f32 {
        self.begin_audio_callback();

        let (
            output,
            _,
        ) =
            self.process_stereo_frame(
                sample,
                sample,
            );

        output
    }

    /// ========================================================
    /// Diagnostics：是否需要 Flush
    /// ========================================================

    #[inline]
    fn maybe_flush_diagnostics(
        &mut self,
    ) {
        if self
            .diagnostic_processed_frames
            >= DIAGNOSTIC_FLUSH_FRAMES
        {
            self.flush_diagnostics();
        }
    }

    /// ========================================================
    /// Diagnostics：批量提交
    /// ========================================================

    fn flush_diagnostics(
        &mut self,
    ) {
        if self
            .diagnostic_processed_frames
            == 0
        {
            return;
        }

        crate::diagnostics::limiter_batch(
            self
                .diagnostic_processed_frames,

            self
                .diagnostic_ceiling_hit_frames,

            self
                .diagnostic_peak_limit_frames,

            self
                .diagnostic_peak_hold_events,

            self
                .diagnostic_peak_gain_min,

            self
                .diagnostic_rms_gain_min,
        );

        self.diagnostic_processed_frames =
            0;

        self.diagnostic_ceiling_hit_frames =
            0;

        self.diagnostic_peak_limit_frames =
            0;

        self.diagnostic_peak_hold_events =
            0;

        self.diagnostic_peak_gain_min =
            1.0;

        self.diagnostic_rms_gain_min =
            1.0;
    }

    /// ========================================================
    /// RMS Soft Knee Limiter Gain
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

        // Hard Knee fallback。
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
            if level_db <= knee_start {
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

    /// ========================================================
    /// Lookahead ms → Stereo Frame
    /// ========================================================

    fn calc_lookahead_samples(
        time_ms: u32,
        sr: f32,
    ) -> usize {
        let samples =
            (
                sr
                    * time_ms
                        as f32
                    / 1000.0
            )
                .round()
                as usize;

        samples.max(1)
    }

    /// ========================================================
    /// 时间常数 → 一阶指数系数
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
            (
                -1.0
                    / samples
            )
                .exp()
        }
    }

    /// ========================================================
    /// dB → Linear
    /// ========================================================

    #[inline]
    fn db_to_linear(
        db: f32,
    ) -> f32 {
        10.0f32.powf(
            db / 20.0,
        )
    }

    /// ========================================================
    /// Linear → dB
    /// ========================================================

    #[inline]
    fn linear_to_db(
        value: f32,
    ) -> f32 {
        if value
            <= 0.000001
        {
            -120.0
        } else {
            20.0
                * value.log10()
        }
    }
}

/// ============================================================
/// Limiter 被销毁前提交最后不足 4096 Frame 的 Diagnostics
/// ============================================================

impl Drop for LoudnessLimiter {
    fn drop(&mut self) {
        self.flush_diagnostics();
    }
}