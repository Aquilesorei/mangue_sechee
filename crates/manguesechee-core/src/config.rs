//! Persistent configuration loaded from `~/.config/manguesechee/config.toml`.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use anyhow::Context;

// ── Top-level config ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub device:    DeviceConfig,
    pub network:   NetworkConfig,
    pub input:     InputConfig,
    pub clipboard: ClipboardConfig,
    pub screen:    ScreenConfig,
    pub peers:     Vec<PeerConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            device:    DeviceConfig::default(),
            network:   NetworkConfig::default(),
            input:     InputConfig::default(),
            clipboard: ClipboardConfig::default(),
            screen:    ScreenConfig::default(),
            peers:     Vec::new(),
        }
    }
}

// ── Sub-sections ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceConfig {
    pub name: String,
    pub id:   String,
}

impl Default for DeviceConfig {
    fn default() -> Self {
        Self {
            name: hostname(),
            id:   uuid::Uuid::new_v4().to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkConfig {
    pub discovery: bool,
    pub port:      u16,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self { discovery: true, port: 24800 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct InputConfig {
    pub enabled:            bool,
    pub mouse_device:       Option<String>,
    pub keyboard_device:    Option<String>,
    pub switch_delay_ms:    u32,
    pub corner_deadzone_px: u32,
    pub cursor_locked:      bool,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            enabled:            true,
            mouse_device:       None,
            keyboard_device:    None,
            switch_delay_ms:    0,
            corner_deadzone_px: 50,
            cursor_locked:      false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipboardConfig {
    pub enabled: bool,
    pub files_enabled: bool,
    pub fast_limit_mb: u32,
    pub background_limit_mb: u32,
}

impl Default for ClipboardConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            files_enabled: true,
            fast_limit_mb: 15,
            background_limit_mb: 500,
        }
    }
}


#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ScreenConfig {
    pub width:  u32,
    pub height: u32,
}

impl Default for ScreenConfig {
    fn default() -> Self {
        Self { width: 1920, height: 1080 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerConfig {
    pub id:       String,
    pub address:  Option<String>,  // host:port — optional if using discovery
    pub position: String,          // "left" | "right" | "above" | "below"
}

// ── Load / save ───────────────────────────────────────────────────────────────

pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("manguesechee")
        .join("config.toml")
}

pub fn load() -> anyhow::Result<Config> {
    let path = config_path();
    if !path.exists() {
        return Ok(Config::default());
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("read {}", path.display()))?;
    let mut cfg: Config = toml::from_str(&text)
        .with_context(|| format!("parse {}", path.display()))?;
    if cfg.device.id.trim().is_empty() {
        cfg.device.id = uuid::Uuid::new_v4().to_string();
        let _ = save(&cfg);
    }
    Ok(cfg)
}

pub fn save(config: &Config) -> anyhow::Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("create config dir")?;
    }
    let text = toml::to_string_pretty(config).context("serialize config")?;
    std::fs::write(&path, text)
        .with_context(|| format!("write {}", path.display()))
}

/// Write a default config file if none exists yet.
pub fn ensure_default() -> anyhow::Result<Config> {
    let path = config_path();
    if !path.exists() {
        let cfg = Config::default();
        save(&cfg)?;
        tracing::info!("created default config at {}", path.display());
        Ok(cfg)
    } else {
        load()
    }
}

fn hostname() -> String {
    if let Ok(s) = std::fs::read_to_string("/etc/hostname") {
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    if let Ok(name) = std::env::var("HOSTNAME") {
        let trimmed = name.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    let mut buf = [0u8; 256];
    let res = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
    if res == 0 {
        if let Ok(cstr) = std::ffi::CStr::from_bytes_until_nul(&buf) {
            if let Ok(s) = cstr.to_str() {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
        }
    }
    "manguesechee-device".to_string()
}
