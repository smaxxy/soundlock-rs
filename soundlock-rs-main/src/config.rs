use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{
    AtomicU32,
    AtomicU64,
    Ordering,
};
use std::sync::{
    Arc,
    Mutex,
};

/// ============================================================
/// 持久化配置
/// ============================================================
///
/// Config 负责：
///
/// - UI 当前设置
/// - 保存到 config.toml
/// - 启动时从 config.toml 恢复
/// - 输入 / 输出设备 ID
///
/// 注意：
///
/// 音频实时线程以后不会直接读取 Config，
/// 也不会 try_lock 这个 Mutex。
///
/// Limiter 的实时参数通过 RuntimeLimiterParams
/// 单独传递。
#[derive(
    Clone,
    Serialize,
    Deserialize,
    Debug,
)]
#[serde(default)]
pub struct Config {
    /// RMS 持续响度阈值
    pub threshold_db: f32,

    /// RMS Gain Attack
    pub attack_ms: u32,

    /// RMS Gain Release
    pub release_ms: u32,

    /// Limiter 前的 Pre-Gain
    ///
    /// UI：
    /// 0 ~ 12 dB
    pub pre_gain_db: f32,

    /// Peak Limiter 独立 Release
    ///
    /// UI：
    /// 20 ~ 150 ms
    pub peak_release_ms: u32,

    /// 输入设备
    pub target_input_device_id:
        Option<String>,

    /// 输出设备
    pub target_output_device_id:
        Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            threshold_db: -16.0,

            attack_ms: 10,

            release_ms: 50,

            pre_gain_db: 6.0,

            peak_release_ms: 50,

            target_input_device_id: None,

            target_output_device_id: None,
        }
    }
}

/// ============================================================
/// Limiter 参数快照
/// ============================================================
///
/// 这是 Limiter 真正关心的参数。
///
/// 不包含：
///
/// - 输入设备
/// - 输出设备
/// - 文件路径
/// - UI 状态
///
/// 后面 LoudnessLimiter 将只接收这一组参数，
/// 不再持有整个 Config。
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
)]
pub struct LimiterParams {
    pub threshold_db: f32,

    pub attack_ms: u32,

    pub release_ms: u32,

    pub pre_gain_db: f32,

    pub peak_release_ms: u32,
}

impl Default for LimiterParams {
    fn default() -> Self {
        Self {
            threshold_db: -16.0,

            attack_ms: 10,

            release_ms: 50,

            pre_gain_db: 6.0,

            peak_release_ms: 50,
        }
    }
}

impl LimiterParams {
    /// 从 Config 提取 Limiter 所需参数。
    pub fn from_config(
        config: &Config,
    ) -> Self {
        Self {
            threshold_db:
                sanitize_threshold_db(
                    config.threshold_db,
                ),

            attack_ms:
                config.attack_ms
                    .clamp(1, 300),

            release_ms:
                config.release_ms
                    .clamp(1, 300),

            pre_gain_db:
                sanitize_pre_gain_db(
                    config.pre_gain_db,
                ),

            peak_release_ms:
                config.peak_release_ms
                    .clamp(20, 150),
        }
    }

    /// 再做一次边界保护。
    ///
    /// 即使未来参数不是直接来自 Config，
    /// 进入实时参数通道以前也保持合法。
    pub fn sanitized(
        self,
    ) -> Self {
        Self {
            threshold_db:
                sanitize_threshold_db(
                    self.threshold_db,
                ),

            attack_ms:
                self.attack_ms
                    .clamp(1, 300),

            release_ms:
                self.release_ms
                    .clamp(1, 300),

            pre_gain_db:
                sanitize_pre_gain_db(
                    self.pre_gain_db,
                ),

            peak_release_ms:
                self.peak_release_ms
                    .clamp(20, 150),
        }
    }
}

/// ============================================================
/// 无锁 Limiter 实时参数
/// ============================================================
///
/// 这是 UI / 控制线程 与 音频实时线程之间的桥梁。
///
/// 核心原则：
///
/// 音频线程：
///
/// - 不 Mutex
/// - 不 RwLock
/// - 不等待 UI
/// - 不分配内存
///
/// UI / 控制线程：
///
/// - 修改 Config
/// - 发布新的 LimiterParams
///
/// Audio Callback：
///
/// - 每个 Callback 开头检查 version
/// - 参数没有变化：什么也不做
/// - 参数发生变化：读取一次新参数
/// - 重新计算 Limiter 系数
///
/// ============================================================
///
/// parameter_version 使用偶数 / 奇数协议：
///
/// 偶数：
///     参数处于稳定状态
///
/// 奇数：
///     UI 正在发布一组新参数
///
/// 如果音频线程刚好碰见奇数版本，
/// 直接跳过本 Callback，
/// 下一次 Callback 再读。
///
/// 因此：
///
/// 音频线程永远不会等待发布线程。
pub struct RuntimeLimiterParams {
    threshold_db_bits: AtomicU32,

    attack_ms: AtomicU32,

    release_ms: AtomicU32,

    pre_gain_db_bits: AtomicU32,

    peak_release_ms: AtomicU32,

    /// 偶数 = 稳定
    /// 奇数 = 正在更新
    parameter_version: AtomicU64,

    /// 只用于序列化“发布线程”。
    ///
    /// Audio Callback 永远不会访问这个锁。
    ///
    /// 当前 UI 本身基本是单写者，
    /// 但保留这个锁可以防止未来其它控制线程
    /// 同时 publish 导致 version 协议被破坏。
    publish_lock: Mutex<()>,
}

impl RuntimeLimiterParams {
    /// 根据 Config 创建实时参数通道。
    pub fn new(
        config: &Config,
    ) -> Self {
        Self::from_params(
            LimiterParams::from_config(
                config,
            ),
        )
    }

    /// 根据 LimiterParams 创建实时参数通道。
    pub fn from_params(
        params: LimiterParams,
    ) -> Self {
        let params =
            params.sanitized();

        Self {
            threshold_db_bits:
                AtomicU32::new(
                    params
                        .threshold_db
                        .to_bits(),
                ),

            attack_ms:
                AtomicU32::new(
                    params.attack_ms,
                ),

            release_ms:
                AtomicU32::new(
                    params.release_ms,
                ),

            pre_gain_db_bits:
                AtomicU32::new(
                    params
                        .pre_gain_db
                        .to_bits(),
                ),

            peak_release_ms:
                AtomicU32::new(
                    params
                        .peak_release_ms,
                ),

            // 从 0 开始：
            // 偶数，表示稳定状态。
            parameter_version:
                AtomicU64::new(0),

            publish_lock:
                Mutex::new(()),
        }
    }

    /// ========================================================
    /// 发布 Config 中最新的 Limiter 参数
    /// ========================================================

    pub fn publish_config(
        &self,
        config: &Config,
    ) {
        self.publish(
            LimiterParams::from_config(
                config,
            ),
        );
    }

    /// ========================================================
    /// 发布一组新的 Limiter 参数
    /// ========================================================
    ///
    /// 这个函数只应该由：
    ///
    /// - UI
    /// - 普通控制线程
    ///
    /// 调用。
    ///
    /// 不允许 Audio Callback 调用。
    pub fn publish(
        &self,
        params: LimiterParams,
    ) {
        let params =
            params.sanitized();

        // 这个 Mutex 不在实时线程使用。
        //
        // 作用只是保证未来即使有多个
        // 非实时发布者，也不会同时写参数。
        let _guard =
            match self
                .publish_lock
                .lock()
            {
                Ok(guard) => guard,

                // 即使 Mutex 曾发生 poison，
                // 参数发布仍然可以继续。
                Err(poisoned) =>
                    poisoned.into_inner(),
            };

        // ====================================================
        // 进入“正在更新”状态
        // ====================================================
        //
        // 稳定版本一定是偶数。
        // +1 后变成奇数。
        self.parameter_version
            .fetch_add(
                1,
                Ordering::AcqRel,
            );

        // ====================================================
        // 写入参数
        // ====================================================

        self.threshold_db_bits.store(
            params
                .threshold_db
                .to_bits(),
            Ordering::Relaxed,
        );

        self.attack_ms.store(
            params.attack_ms,
            Ordering::Relaxed,
        );

        self.release_ms.store(
            params.release_ms,
            Ordering::Relaxed,
        );

        self.pre_gain_db_bits.store(
            params
                .pre_gain_db
                .to_bits(),
            Ordering::Relaxed,
        );

        self.peak_release_ms.store(
            params.peak_release_ms,
            Ordering::Relaxed,
        );

        // ====================================================
        // 发布完成
        // ====================================================
        //
        // 再 +1，恢复成偶数。
        //
        // Release 保证前面的参数写入
        // 在音频线程看到新版本时已经可见。
        self.parameter_version
            .fetch_add(
                1,
                Ordering::Release,
            );
    }

    /// ========================================================
    /// Audio Callback：
    /// 参数变化时读取新快照
    /// ========================================================
    ///
    /// last_version：
    ///
    /// 由 LoudnessLimiter 自己保存。
    ///
    /// 返回：
    ///
    /// None
    ///     参数没变化
    ///     或 UI 此刻正在更新
    ///
    /// Some(params)
    ///     得到一组完整稳定的新参数
    ///
    /// 这里：
    ///
    /// - 不加锁
    /// - 不等待
    /// - 不 spin
    /// - 不分配内存
    #[inline]
    pub fn load_if_changed(
        &self,
        last_version: &mut u64,
    ) -> Option<LimiterParams> {
        let version_before =
            self.parameter_version
                .load(
                    Ordering::Acquire,
                );

        // 奇数：
        // UI 正在发布参数。
        //
        // 直接使用旧参数处理本 Callback。
        if version_before & 1 != 0 {
            return None;
        }

        // 版本完全没变。
        if version_before
            == *last_version
        {
            return None;
        }

        let params =
            self.load_relaxed();

        // 再读一次版本。
        //
        // 防止我们读取参数期间
        // UI 又开始了下一次 publish。
        let version_after =
            self.parameter_version
                .load(
                    Ordering::Acquire,
                );

        if version_before
            != version_after
            || version_after & 1 != 0
        {
            // 快照不稳定。
            //
            // 不重试、不自旋。
            // 下一 Callback 再同步。
            return None;
        }

        *last_version =
            version_after;

        Some(params)
    }

    /// ========================================================
    /// 获取当前稳定参数
    /// ========================================================
    ///
    /// 主要用于初始化，
    /// 不建议 Audio Callback 每帧调用。
    pub fn snapshot(
        &self,
    ) -> LimiterParams {
        loop {
            let version_before =
                self.parameter_version
                    .load(
                        Ordering::Acquire,
                    );

            if version_before & 1 != 0 {
                std::hint::spin_loop();
                continue;
            }

            let params =
                self.load_relaxed();

            let version_after =
                self.parameter_version
                    .load(
                        Ordering::Acquire,
                    );

            if version_before
                == version_after
                && version_after & 1 == 0
            {
                return params;
            }
        }
    }

    /// 当前参数版本。
    ///
    /// 以后 LoudnessLimiter 初始化时使用。
    pub fn version(
        &self,
    ) -> u64 {
        self.parameter_version
            .load(
                Ordering::Acquire,
            )
    }

    /// 实际读取 Atomics。
    #[inline]
    fn load_relaxed(
        &self,
    ) -> LimiterParams {
        LimiterParams {
            threshold_db:
                f32::from_bits(
                    self
                        .threshold_db_bits
                        .load(
                            Ordering::Relaxed,
                        ),
                ),

            attack_ms:
                self.attack_ms
                    .load(
                        Ordering::Relaxed,
                    ),

            release_ms:
                self.release_ms
                    .load(
                        Ordering::Relaxed,
                    ),

            pre_gain_db:
                f32::from_bits(
                    self
                        .pre_gain_db_bits
                        .load(
                            Ordering::Relaxed,
                        ),
                ),

            peak_release_ms:
                self.peak_release_ms
                    .load(
                        Ordering::Relaxed,
                    ),
        }
        .sanitized()
    }
}

impl Default
    for RuntimeLimiterParams
{
    fn default() -> Self {
        Self::new(
            &Config::default(),
        )
    }
}

/// ============================================================
/// Config
/// ============================================================

impl Config {
    fn path() -> PathBuf {
        if let Ok(appdata) =
            std::env::var(
                "APPDATA",
            )
        {
            PathBuf::from(appdata)
                .join(
                    "SoundLockRust",
                )
                .join(
                    "config.toml",
                )
        } else {
            PathBuf::from(
                "config.toml",
            )
        }
    }

    /// ========================================================
    /// 加载配置
    /// ========================================================

    pub fn load()
        -> Result<
            Arc<Mutex<Config>>,
            Box<dyn Error>,
        >
    {
        let path =
            Self::path();

        if !path.exists() {
            return Ok(
                Arc::new(
                    Mutex::new(
                        Self::default(),
                    ),
                ),
            );
        }

        let text =
            fs::read_to_string(
                path,
            )?;

        let mut config:
            Config =
            toml::from_str(
                &text,
            )?;

        config.sanitize();

        Ok(
            Arc::new(
                Mutex::new(
                    config,
                ),
            ),
        )
    }

    /// ========================================================
    /// 保存配置
    /// ========================================================
    ///
    /// 当前暂时保持原来的写法。
    ///
    /// “原子配置写入 + 损坏备份”
    /// 是后面独立的一项优化，
    /// 这一步先不混在实时线程重构里。
    pub fn save(
        &self,
    ) -> Result<
        (),
        Box<dyn Error>,
    > {
        let path =
            Self::path();

        if let Some(parent) =
            path.parent()
        {
            fs::create_dir_all(
                parent,
            )?;
        }

        let mut config =
            self.clone();

        config.sanitize();

        let text =
            toml::to_string_pretty(
                &config,
            )?;

        fs::write(
            path,
            text,
        )?;

        Ok(())
    }

    /// ========================================================
    /// 获取 Limiter 参数快照
    /// ========================================================

    pub fn limiter_params(
        &self,
    ) -> LimiterParams {
        LimiterParams::from_config(
            self,
        )
    }

    /// ========================================================
    /// 配置合法化
    /// ========================================================

    fn sanitize(
        &mut self,
    ) {
        self.threshold_db =
            sanitize_threshold_db(
                self.threshold_db,
            );

        self.attack_ms =
            self.attack_ms
                .clamp(
                    1,
                    300,
                );

        self.release_ms =
            self.release_ms
                .clamp(
                    1,
                    300,
                );

        self.pre_gain_db =
            sanitize_pre_gain_db(
                self.pre_gain_db,
            );

        self.peak_release_ms =
            self.peak_release_ms
                .clamp(
                    20,
                    150,
                );
    }
}

/// ============================================================
/// 参数清洗
/// ============================================================

#[inline]
fn sanitize_threshold_db(
    value: f32,
) -> f32 {
    if !value.is_finite() {
        -16.0
    } else {
        value.clamp(
            -60.0,
            0.0,
        )
    }
}

#[inline]
fn sanitize_pre_gain_db(
    value: f32,
) -> f32 {
    if !value.is_finite() {
        6.0
    } else {
        value.clamp(
            0.0,
            12.0,
        )
    }
}