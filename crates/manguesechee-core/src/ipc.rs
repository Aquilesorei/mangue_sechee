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

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "cmd")]
pub enum GuiCommand {
    GetStatus,
    Connect   { address: String },
    Disconnect,
    SetDiscovery { enabled: bool },
    SetFileTransfer { enabled: bool },
    SetCursorLock { locked: bool },
    ForgetPeer { address: String },
    SyncTopology { address: String, position: String },
    Shutdown,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum AgentEvent {
    Status {
        local_name:            String,
        connected_to:          Option<String>,
        discovery:             bool,
        #[serde(default = "default_true")]
        file_transfer_enabled: bool,
        cursor_locked:         bool,
        last_error:            Option<String>,
        topology_configured:   bool,
        peers:                 Vec<PeerInfo>,
        #[serde(default)]
        active_transfers:      Vec<FileTransferInfo>,
        #[serde(default)]
        transfer_history:      Vec<TransferHistoryEntry>,
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

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct FileTransferInfo {
    pub transfer_id:       String,
    pub filename:          String,
    pub bytes_transferred: u64,
    pub total_bytes:       u64,
    pub is_receiving:      bool,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TransferHistoryEntry {
    pub filename:     String,
    pub total_bytes:  u64,
    pub completed_at: String,
    pub is_receiving: bool,
}

