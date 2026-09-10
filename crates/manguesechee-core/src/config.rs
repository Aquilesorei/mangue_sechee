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
}

impl Default for DeviceConfig {
    fn default() -> Self {
        Self { name: hostname() }
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
    pub enabled:       bool,
    pub mouse_device:  Option<String>,
    pub keyboard_device: Option<String>,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self { enabled: true, mouse_device: None, keyboard_device: None }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipboardConfig {
    pub enabled: bool,
}

impl Default for ClipboardConfig {
    fn default() -> Self {
        Self { enabled: true }
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
    toml::from_str(&text)
        .with_context(|| format!("parse {}", path.display()))
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
    std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}
