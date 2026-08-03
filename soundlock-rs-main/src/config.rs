use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[derive(Clone, Serialize, Deserialize, Debug, Default, PartialEq, Copy)]
#[serde(rename_all = "snake_case")]
pub enum OperationMode {
    #[default]
    Cable,
}

// 新增：限幅器工作模式
#[derive(Clone, Serialize, Deserialize, Debug, Default, PartialEq, Copy)]
#[serde(rename_all = "snake_case")]
pub enum LimiterMode {
    #[default]
    Fullband,   // 全频最大音量限制（原行为）
    Multiband,  // 分频保护脚步声（只压高频）
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Config {
    #[serde(default)]
    pub target_pid: Option<u32>,

    #[serde(default = "default_threshold")]
    pub threshold_db: f32,

    #[serde(default = "default_attack_ms")]
    pub attack_ms: u32,

    #[serde(default = "default_release_ms")]
    pub release_ms: u32,

    #[serde(default = "default_scan_interval_ms")]
    pub scan_interval_ms: u32,

    #[serde(default = "default_volume_change_percentage_threshold")]
    pub volume_change_percentage_threshold: f32,

    #[serde(default)]
    pub operation_mode: OperationMode,

    #[serde(default)]
    pub target_input_device_id: Option<String>,

    #[serde(default)]
    pub target_output_device_id: Option<String>,

    // 新增：分频点 (Hz)
    #[serde(default = "default_crossover_freq")]
    pub crossover_freq: f32,

    // 新增：限幅模式
    #[serde(default)]
    pub limiter_mode: LimiterMode,
}

fn default_threshold() -> f32 {
    -16.0
}

fn default_attack_ms() -> u32 {
    10
}

fn default_release_ms() -> u32 {
    50
}

fn default_scan_interval_ms() -> u32 {
    80
}

fn default_volume_change_percentage_threshold() -> f32 {
    0.02
}

fn default_crossover_freq() -> f32 {
    300.0
}

impl Default for Config {
    fn default() -> Self {
        Self {
            target_pid: None,
            threshold_db: default_threshold(),
            attack_ms: default_attack_ms(),
            release_ms: default_release_ms(),
            scan_interval_ms: default_scan_interval_ms(),
            volume_change_percentage_threshold: default_volume_change_percentage_threshold(),
            operation_mode: OperationMode::default(),
            target_input_device_id: None,
            target_output_device_id: None,
            crossover_freq: default_crossover_freq(),
            limiter_mode: LimiterMode::default(),
        }
    }
}

impl Config {
    pub fn path() -> PathBuf {
        if let Some(mut path) = dirs::config_dir() {
            path.push("SoundLockRust");
            std::fs::create_dir_all(&path).ok();
            path.push("config.toml");
            return path;
        }

        let mut path = std::env::current_dir().unwrap_or_default();
        path.push("config.toml");
        path
    }

    pub fn load() -> Result<Arc<Mutex<Self>>, Box<dyn std::error::Error>> {
        let path = Self::path();

        if !path.exists() {
            log::debug!("Config file not found, using defaults");
            return Ok(Arc::new(Mutex::new(Self::default())));
        }

        let content = std::fs::read_to_string(&path)?;
        let config: Config = toml::from_str(&content)?;

        log::debug!("Config loaded from {:?}", path);
        Ok(Arc::new(Mutex::new(config)))
    }

    pub fn save(&self) -> Result<(), Box<dyn std::error::Error>> {
        let path = Self::path();

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }

        let content = toml::to_string_pretty(self)?;
        std::fs::write(&path, content)?;

        log::info!("Config saved to {:?}", path);
        Ok(())
    }
}