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
    pub hotkeys:   HotkeyConfig,
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
            hotkeys:   HotkeyConfig::default(),
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
    pub tls:       bool,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self { discovery: true, port: 24800, tls: false }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct InputConfig {
    pub enabled:                 bool,
    pub mouse_device:            Option<String>,
    pub keyboard_device:         Option<String>,
    pub switch_delay_ms:         u32,
    pub corner_deadzone_px:      u32,
    pub cursor_locked:           bool,
    pub edge_velocity_threshold: u32,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            enabled:                 true,
            mouse_device:            None,
            keyboard_device:         None,
            switch_delay_ms:         0,
            corner_deadzone_px:      50,
            cursor_locked:           false,
            edge_velocity_threshold: 20,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct HotkeyConfig {
    pub enabled:            bool,
    pub toggle_cursor_lock: String,
    pub switch_screen:      String,
    pub emergency_escape:   String,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            enabled:            true,
            toggle_cursor_lock: "ScrollLock".to_string(),
            switch_screen:      "Ctrl+Alt+Tab".to_string(),
            emergency_escape:   "Ctrl+Alt+Escape".to_string(),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EdgeAlignment {
    Center,
    Top,
    Bottom,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PeerConfig {
    pub id:        String,
    pub address:   Option<String>,  // host:port — optional if using discovery
    pub position:  String,          // "left" | "right" | "above" | "below"
    #[serde(default)]
    pub grid_x:    Option<i32>,
    #[serde(default)]
    pub grid_y:    Option<i32>,
    #[serde(default)]
    pub alignment: Option<String>, // "center" | "top" | "bottom"
}

impl PeerConfig {
    pub fn new(id: impl Into<String>, address: Option<String>, position: impl Into<String>) -> Self {
        let pos = position.into();
        let (gx, gy) = match pos.trim().to_lowercase().as_str() {
            "left" => (-1, 0),
            "above" | "top" => (0, 1),
            "below" | "bottom" => (0, -1),
            _ => (1, 0),
        };
        Self {
            id: id.into(),
            address,
            position: pos,
            grid_x: Some(gx),
            grid_y: Some(gy),
            alignment: None,
        }
    }

    pub fn with_coords(id: impl Into<String>, address: Option<String>, grid_x: i32, grid_y: i32) -> Self {
        let position = match (grid_x, grid_y) {
            (-1, 0) => "left".to_string(),
            (1, 0) => "right".to_string(),
            (0, 1) => "above".to_string(),
            (0, -1) => "below".to_string(),
            (x, _) if x > 0 => "right".to_string(),
            (x, _) if x < 0 => "left".to_string(),
            (_, y) if y > 0 => "above".to_string(),
            _ => "below".to_string(),
        };
        Self {
            id: id.into(),
            address,
            position,
            grid_x: Some(grid_x),
            grid_y: Some(grid_y),
            alignment: None,
        }
    }

    pub fn with_alignment(mut self, alignment: Option<String>) -> Self {
        self.alignment = alignment;
        self
    }

    pub fn coordinates(&self) -> (i32, i32) {
        if let (Some(x), Some(y)) = (self.grid_x, self.grid_y) {
            return (x, y);
        }
        match self.position.trim().to_lowercase().as_str() {
            "left" => (-1, 0),
            "above" | "top" => (0, 1),
            "below" | "bottom" => (0, -1),
            _ => (1, 0),
        }
    }

    pub fn edge_alignment(&self) -> EdgeAlignment {
        match self.alignment.as_deref().unwrap_or("center").trim().to_lowercase().as_str() {
            "top" | "start" => EdgeAlignment::Top,
            "bottom" | "end" => EdgeAlignment::Bottom,
            _ => EdgeAlignment::Center,
        }
    }
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
