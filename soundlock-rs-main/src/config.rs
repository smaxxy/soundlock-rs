use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_THRESHOLD_DB: f32 = -16.0;
const DEFAULT_ATTACK_MS: u32 = 10;
const DEFAULT_RELEASE_MS: u32 = 50;
const DEFAULT_PRE_GAIN_DB: f32 = 6.0;
const DEFAULT_PEAK_RELEASE_MS: u32 = 50;
const DEFAULT_CROSSHAIR_ENABLED: bool = false;
const DEFAULT_CROSSHAIR_COLOR: u32 = 0x0078FF;
const DEFAULT_CROSSHAIR_SIZE: u32 = 18;

static SAVE_COUNTER: AtomicU64 = AtomicU64::new(0);
static SAVE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct Config {
    pub threshold_db: f32,
    pub attack_ms: u32,
    pub release_ms: u32,
    pub pre_gain_db: f32,
    pub peak_release_ms: u32,
    pub target_input_device_id: Option<String>,
    pub target_output_device_id: Option<String>,
    pub crosshair_enabled: bool,
    pub crosshair_color: u32,
    pub crosshair_size: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            threshold_db: DEFAULT_THRESHOLD_DB,
            attack_ms: DEFAULT_ATTACK_MS,
            release_ms: DEFAULT_RELEASE_MS,
            pre_gain_db: DEFAULT_PRE_GAIN_DB,
            peak_release_ms: DEFAULT_PEAK_RELEASE_MS,
            target_input_device_id: None,
            target_output_device_id: None,
            crosshair_enabled: DEFAULT_CROSSHAIR_ENABLED,
            crosshair_color: DEFAULT_CROSSHAIR_COLOR,
            crosshair_size: DEFAULT_CROSSHAIR_SIZE,
        }
    }
}

/// Limiter 真正需要的 5 个运行时参数。
///
/// 设备 ID 不进入实时参数通道，避免设备选择变化造成无意义的 DSP 系数重算。
#[derive(Clone, Copy, Debug, PartialEq)]
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
            threshold_db: DEFAULT_THRESHOLD_DB,
            attack_ms: DEFAULT_ATTACK_MS,
            release_ms: DEFAULT_RELEASE_MS,
            pre_gain_db: DEFAULT_PRE_GAIN_DB,
            peak_release_ms: DEFAULT_PEAK_RELEASE_MS,
        }
    }
}

impl LimiterParams {
    pub fn from_config(config: &Config) -> Self {
        Self {
            threshold_db: config.threshold_db,
            attack_ms: config.attack_ms,
            release_ms: config.release_ms,
            pre_gain_db: config.pre_gain_db,
            peak_release_ms: config.peak_release_ms,
        }
        .sanitized()
    }

    pub fn sanitized(mut self) -> Self {
        if !self.threshold_db.is_finite() {
            self.threshold_db = DEFAULT_THRESHOLD_DB;
        }
        self.threshold_db = self.threshold_db.clamp(-60.0, 0.0);

        self.attack_ms = self.attack_ms.clamp(1, 300);
        self.release_ms = self.release_ms.clamp(1, 300);

        if !self.pre_gain_db.is_finite() {
            self.pre_gain_db = DEFAULT_PRE_GAIN_DB;
        }
        self.pre_gain_db = self.pre_gain_db.clamp(0.0, 12.0);

        self.peak_release_ms = self.peak_release_ms.clamp(20, 150);
        self
    }
}

/// UI / 控制线程 -> Audio Callback 的无锁参数通道。
///
/// 写端由 publish_lock 串行化；Audio Callback 永远不会碰这个锁。
/// parameter_version 使用偶/奇版本协议：
/// - 偶数：稳定快照
/// - 奇数：写端正在发布
pub struct RuntimeLimiterParams {
    threshold_db_bits: AtomicU32,
    attack_ms: AtomicU32,
    release_ms: AtomicU32,
    pre_gain_db_bits: AtomicU32,
    peak_release_ms: AtomicU32,
    parameter_version: AtomicU64,
    publish_lock: Mutex<()>,
}

impl RuntimeLimiterParams {
    pub fn new(config: &Config) -> Self {
        Self::from_params(LimiterParams::from_config(config))
    }

    pub fn from_params(params: LimiterParams) -> Self {
        let params = params.sanitized();

        Self {
            threshold_db_bits: AtomicU32::new(params.threshold_db.to_bits()),
            attack_ms: AtomicU32::new(params.attack_ms),
            release_ms: AtomicU32::new(params.release_ms),
            pre_gain_db_bits: AtomicU32::new(params.pre_gain_db.to_bits()),
            peak_release_ms: AtomicU32::new(params.peak_release_ms),
            parameter_version: AtomicU64::new(0),
            publish_lock: Mutex::new(()),
        }
    }

    pub fn publish_config(&self, config: &Config) {
        self.publish(LimiterParams::from_config(config));
    }

    pub fn publish(&self, params: LimiterParams) {
        let params = params.sanitized();

        // 写端不是实时线程，可以阻塞；即使 Mutex 曾 poison 也恢复使用。
        let _guard = match self.publish_lock.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        // even -> odd，告诉读端“当前正在发布”。
        self.parameter_version.fetch_add(1, Ordering::AcqRel);

        self.threshold_db_bits
            .store(params.threshold_db.to_bits(), Ordering::Relaxed);
        self.attack_ms.store(params.attack_ms, Ordering::Relaxed);
        self.release_ms.store(params.release_ms, Ordering::Relaxed);
        self.pre_gain_db_bits
            .store(params.pre_gain_db.to_bits(), Ordering::Relaxed);
        self.peak_release_ms
            .store(params.peak_release_ms, Ordering::Relaxed);

        // odd -> even；Release 保证参数写入对 Acquire 读端可见。
        self.parameter_version.fetch_add(1, Ordering::Release);
    }

    /// Audio Callback 使用：不锁、不等待、不分配。
    ///
    /// 如果正好撞上写端，直接返回 None，下一次 callback 再读取；
    /// 不在实时线程里 spin。
    #[inline]
    pub fn load_if_changed(&self, last_version: &mut u64) -> Option<LimiterParams> {
        let version_before = self.parameter_version.load(Ordering::Acquire);

        if version_before & 1 != 0 || version_before == *last_version {
            return None;
        }

        let params = self.load_relaxed();
        let version_after = self.parameter_version.load(Ordering::Acquire);

        if version_before != version_after || version_after & 1 != 0 {
            return None;
        }

        *last_version = version_after;
        Some(params.sanitized())
    }

    /// 初始化 / 测试路径使用的稳定快照。
    /// 不是实时 Audio Callback API。
    pub fn snapshot(&self) -> LimiterParams {
        loop {
            let version_before = self.parameter_version.load(Ordering::Acquire);

            if version_before & 1 != 0 {
                std::hint::spin_loop();
                continue;
            }

            let params = self.load_relaxed();
            let version_after = self.parameter_version.load(Ordering::Acquire);

            if version_before == version_after && version_after & 1 == 0 {
                return params.sanitized();
            }

            std::hint::spin_loop();
        }
    }

    #[allow(dead_code)]
    pub fn version(&self) -> u64 {
        self.parameter_version.load(Ordering::Acquire)
    }

    #[inline]
    fn load_relaxed(&self) -> LimiterParams {
        LimiterParams {
            threshold_db: f32::from_bits(self.threshold_db_bits.load(Ordering::Relaxed)),
            attack_ms: self.attack_ms.load(Ordering::Relaxed),
            release_ms: self.release_ms.load(Ordering::Relaxed),
            pre_gain_db: f32::from_bits(self.pre_gain_db_bits.load(Ordering::Relaxed)),
            peak_release_ms: self.peak_release_ms.load(Ordering::Relaxed),
        }
    }
}

impl Config {
    fn path() -> PathBuf {
        if let Ok(appdata) = std::env::var("APPDATA") {
            PathBuf::from(appdata)
                .join("SoundLockRust")
                .join("config.toml")
        } else {
            PathBuf::from("config.toml")
        }
    }

    pub fn load() -> Result<Arc<Mutex<Config>>, Box<dyn Error>> {
        let path = Self::path();

        if !path.exists() {
            return Ok(Arc::new(Mutex::new(Self::default())));
        }

        let text = fs::read_to_string(&path)?;

        let mut config: Config = match toml::from_str(&text) {
            Ok(config) => config,
            Err(error) => {
                // 配置内容损坏时先保留原文件，再使用默认值启动。
                // 这样不会因为一个坏 TOML 导致整个程序无法打开。
                match backup_malformed_config(&path) {
                    Ok(backup) => {
                        log::error!("配置文件解析失败，已备份到 {}: {}", backup.display(), error);
                    }
                    Err(backup_error) => {
                        log::error!(
                            "配置文件解析失败且备份失败: {}; backup error: {}",
                            error,
                            backup_error
                        );
                    }
                }

                Self::default()
            }
        };

        config.sanitize();
        Ok(Arc::new(Mutex::new(config)))
    }

    /// 小配置文件的 crash-safe 保存：
    /// 1. 同目录写唯一临时文件
    /// 2. flush + sync_all
    /// 3. Windows 使用 MoveFileExW(REPLACE_EXISTING | WRITE_THROUGH) 原子替换
    ///
    /// SAVE_LOCK 只存在于普通 UI/保存线程，Audio Callback 永远不会触碰。
    pub fn save(&self) -> Result<(), Box<dyn Error>> {
        let _save_guard = match SAVE_LOCK.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        let path = Self::path();

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut config = self.clone();
        config.sanitize();

        let text = toml::to_string_pretty(&config)?;
        let temp_path = unique_temp_path(&path);

        let write_result = (|| -> io::Result<()> {
            let mut file = File::create(&temp_path)?;
            file.write_all(text.as_bytes())?;
            file.flush()?;
            file.sync_all()?;
            atomic_replace(&temp_path, &path)?;
            Ok(())
        })();

        if write_result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }

        write_result?;
        Ok(())
    }

    pub fn limiter_params(&self) -> LimiterParams {
        LimiterParams::from_config(self)
    }

    fn sanitize(&mut self) {
        let params = self.limiter_params();
        self.threshold_db = params.threshold_db;
        self.attack_ms = params.attack_ms;
        self.release_ms = params.release_ms;
        self.pre_gain_db = params.pre_gain_db;
        self.peak_release_ms = params.peak_release_ms;
        self.crosshair_color &= 0x00FF_FFFF;
        self.crosshair_size = self.crosshair_size.clamp(4, 60);
    }
}

fn unique_temp_path(path: &Path) -> PathBuf {
    let sequence = SAVE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.toml");

    path.with_file_name(format!(".{file_name}.{pid}.{sequence}.tmp"))
}

fn backup_malformed_config(path: &Path) -> io::Result<PathBuf> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.toml");

    let mut backup = path.with_file_name(format!("{file_name}.corrupt-{timestamp}"));
    let mut suffix = 0u32;

    while backup.exists() {
        suffix += 1;
        backup = path.with_file_name(format!("{file_name}.corrupt-{timestamp}-{suffix}"));
    }

    // 优先 rename：原损坏文件不再在下次启动时反复触发解析失败。
    match fs::rename(path, &backup) {
        Ok(()) => Ok(backup),
        Err(rename_error) => {
            // 某些安全软件/文件系统可能暂时阻止 rename。
            // 至少尝试 copy，确保用户仍保有损坏原件。
            fs::copy(path, &backup)
                .map(|_| {
                    // copy 成功后尽量移除损坏原件，避免下次启动重复解析失败。
                    let _ = fs::remove_file(path);
                    backup
                })
                .map_err(|copy_error| {
                    io::Error::new(
                        copy_error.kind(),
                        format!("rename failed: {rename_error}; copy failed: {copy_error}"),
                    )
                })
        }
    }
}

#[cfg(windows)]
fn atomic_replace(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x0000_0001;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;

    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(
            lp_existing_file_name: *const u16,
            lp_new_file_name: *const u16,
            flags: u32,
        ) -> i32;
    }

    let source_wide: Vec<u16> = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let destination_wide: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let result = unsafe {
        MoveFileExW(
            source_wide.as_ptr(),
            destination_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };

    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn atomic_replace(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiter_params_are_sanitized() {
        let params = LimiterParams {
            threshold_db: f32::NAN,
            attack_ms: 0,
            release_ms: 9999,
            pre_gain_db: f32::INFINITY,
            peak_release_ms: 1,
        }
        .sanitized();

        assert_eq!(params.threshold_db, DEFAULT_THRESHOLD_DB);
        assert_eq!(params.attack_ms, 1);
        assert_eq!(params.release_ms, 300);
        assert_eq!(params.pre_gain_db, DEFAULT_PRE_GAIN_DB);
        assert_eq!(params.peak_release_ms, 20);
    }

    #[test]
    fn runtime_publish_is_visible_as_one_stable_snapshot() {
        let runtime = RuntimeLimiterParams::from_params(LimiterParams::default());
        let mut version = u64::MAX;

        // 首次读取必须能取得稳定快照。
        let _ = runtime.load_if_changed(&mut version).unwrap();

        let expected = LimiterParams {
            threshold_db: -21.5,
            attack_ms: 7,
            release_ms: 81,
            pre_gain_db: 8.0,
            peak_release_ms: 72,
        };

        runtime.publish(expected);
        let actual = runtime.load_if_changed(&mut version).unwrap();
        assert_eq!(actual, expected);

        // 没有新 publish 时不应重复返回参数。
        assert!(runtime.load_if_changed(&mut version).is_none());
    }

    #[test]
    fn old_config_without_crosshair_fields_uses_defaults() {
        let config: Config = toml::from_str(
            r#"
threshold_db = -18.0
attack_ms = 12
release_ms = 60
pre_gain_db = 4.0
peak_release_ms = 55
"#,
        )
        .expect("deserialize old config");

        assert!(!config.crosshair_enabled);
        assert_eq!(config.crosshair_color, DEFAULT_CROSSHAIR_COLOR);
        assert_eq!(config.crosshair_size, DEFAULT_CROSSHAIR_SIZE);
    }

    #[test]
    fn crosshair_settings_are_sanitized() {
        let mut config = Config {
            crosshair_color: 0xFFFF_FFFF,
            crosshair_size: 999,
            ..Config::default()
        };

        config.sanitize();

        assert_eq!(config.crosshair_color, 0x00FF_FFFF);
        assert_eq!(config.crosshair_size, 60);
    }
}
