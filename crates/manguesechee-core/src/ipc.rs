//! Shared IPC path conventions and message types.
//! Agent writes PID file + listens on Unix socket.
//! GUI reads PID file to check liveness, connects to socket to send commands.

use std::path::PathBuf;

pub fn runtime_dir() -> PathBuf {
    std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(format!("/tmp/manguesechee-{}", unsafe { libc::getuid() })))
        .join("manguesechee")
}

pub fn pid_file()    -> PathBuf { runtime_dir().join("agent.pid")  }
pub fn socket_path() -> PathBuf { runtime_dir().join("agent.sock") }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "cmd")]
pub enum GuiCommand {
    GetStatus,
    Connect   { address: String },
    Disconnect,
    SetDiscovery { enabled: bool },
    SetCursorLock { locked: bool },
    ForgetPeer { address: String },
    SyncTopology { address: String, position: String },
    Shutdown,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum AgentEvent {
    Status {
        local_name:    String,
        connected_to:  Option<String>,
        discovery:     bool,
        cursor_locked: bool,
        last_error:    Option<String>,
        peers:         Vec<PeerInfo>,
    },
    Ok,
    Error { message: String },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PeerInfo {
    pub name:      String,
    pub address:   String,
    pub paired:    bool,
    pub connected: bool,
    pub position:  String,
}
