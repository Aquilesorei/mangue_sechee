//! Unix socket IPC server — lets the GUI control the running agent.

use anyhow::Context;
use manguesechee_core::ipc::{self, AgentEvent, GuiCommand, PeerInfo};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tracing::{info, warn};

#[derive(Debug, Clone)]
pub struct AgentState {
    pub local_name:            String,
    pub connected_to:          Option<String>,
    pub discovery:             bool,
    pub file_transfer_enabled: bool,
    pub tls_enabled:           bool,
    pub tls_active:            bool,
    pub cursor_locked:         bool,
    pub is_forwarding:         bool,
    pub last_error:            Option<String>,
    pub topology_configured:   bool,
    pub disconnect_requested:  bool,
    pub edge_delay_ms:         u32,
    pub peers:                 Vec<PeerInfo>,
    pub active_transfers:      Vec<manguesechee_core::ipc::FileTransferInfo>,
    pub transfer_history:      Vec<manguesechee_core::ipc::TransferHistoryEntry>,
}

impl Default for AgentState {
    fn default() -> Self {
        Self {
            local_name:            String::new(),
            connected_to:          None,
            discovery:             true,
            file_transfer_enabled: true,
            tls_enabled:           false,
            tls_active:            false,
            cursor_locked:         false,
            is_forwarding:         false,
            last_error:            None,
            topology_configured:   false,
            disconnect_requested:  false,
            edge_delay_ms:         0,
            peers:                 Vec::new(),
            active_transfers:      Vec::new(),
            transfer_history:      Vec::new(),
        }
    }
}

pub type SharedState = Arc<Mutex<AgentState>>;

pub struct PidGuard;
impl PidGuard {
    pub fn write() -> anyhow::Result<Self> {
        let path = ipc::pid_file();
        if let Some(p) = path.parent() { std::fs::create_dir_all(p)?; }
        std::fs::write(&path, std::process::id().to_string())
            .with_context(|| format!("write PID to {}", path.display()))?;
        Ok(Self)
    }
}
impl Drop for PidGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(ipc::pid_file());
        let _ = std::fs::remove_file(ipc::socket_path());
    }
}

pub async fn run(
    state:        SharedState,
    connect_tx:   tokio::sync::mpsc::UnboundedSender<String>,
    broadcast_tx: tokio::sync::broadcast::Sender<manguesechee_core::protocol::Message>,
) -> anyhow::Result<()> {
    let path = ipc::socket_path();
    if let Some(p) = path.parent() { std::fs::create_dir_all(p)?; }
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)
        .with_context(|| format!("bind Unix socket {}", path.display()))?;
    info!("IPC socket: {}", path.display());
    loop {
        let (stream, _) = listener.accept().await?;
        let state = Arc::clone(&state);
        let connect_tx = connect_tx.clone();
        let broadcast_tx = broadcast_tx.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_client(stream, state, connect_tx, broadcast_tx).await {
                warn!("IPC client: {e}");
            }
        });
    }
}

async fn handle_client(
    stream:       tokio::net::UnixStream,
    state:        SharedState,
    connect_tx:   tokio::sync::mpsc::UnboundedSender<String>,
    broadcast_tx: tokio::sync::broadcast::Sender<manguesechee_core::protocol::Message>,
) -> anyhow::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await? {
        let event = match serde_json::from_str::<GuiCommand>(&line) {
            Ok(cmd) => handle_command(cmd, &state, &connect_tx, &broadcast_tx),
            Err(e)  => AgentEvent::Error { message: e.to_string() },
        };
        let mut resp = serde_json::to_string(&event)?;
        resp.push('\n');
        writer.write_all(resp.as_bytes()).await?;
    }
    Ok(())
}

fn handle_command(
    cmd:          GuiCommand,
    state:        &SharedState,
    connect_tx:   &tokio::sync::mpsc::UnboundedSender<String>,
    broadcast_tx: &tokio::sync::broadcast::Sender<manguesechee_core::protocol::Message>,
) -> AgentEvent {
    match cmd {
        GuiCommand::GetStatus => {
            let s = state.lock().unwrap();
            AgentEvent::Status {
                local_name:            s.local_name.clone(),
                connected_to:          s.connected_to.clone(),
                discovery:             s.discovery,
                file_transfer_enabled: s.file_transfer_enabled,
                tls_enabled:           s.tls_enabled,
                tls_active:            s.tls_active,
                cursor_locked:         s.cursor_locked,
                edge_delay_ms:         s.edge_delay_ms,
                last_error:            s.last_error.clone(),
                topology_configured:   s.topology_configured,
                peers:                 s.peers.clone(),
                active_transfers:      s.active_transfers.clone(),
                transfer_history:      s.transfer_history.clone(),
            }
        }
        GuiCommand::Connect { address } => {
            info!("IPC: connect requested to {address}");
            let mut s = state.lock().unwrap();
            s.last_error = None;
            s.disconnect_requested = false;
            let _ = connect_tx.send(address);
            AgentEvent::Ok
        }
        GuiCommand::Disconnect => {
            info!("IPC: disconnect requested");
            let mut s = state.lock().unwrap();
            s.connected_to = None;
            s.topology_configured = false;
            s.disconnect_requested = true;
            let _ = broadcast_tx.send(manguesechee_core::protocol::Message::Goodbye);
            AgentEvent::Ok
        }
        GuiCommand::SetDiscovery { enabled } => {
            state.lock().unwrap().discovery = enabled;
            AgentEvent::Ok
        }
        GuiCommand::SetFileTransfer { enabled } => {
            info!("IPC: set file transfer = {enabled}");
            let mut s = state.lock().unwrap();
            s.file_transfer_enabled = enabled;
            if let Ok(mut cfg) = manguesechee_core::config::load() {
                cfg.clipboard.files_enabled = enabled;
                let _ = manguesechee_core::config::save(&cfg);
            }
            let _ = broadcast_tx.send(manguesechee_core::protocol::Message::FileTransferStatus { enabled });
            AgentEvent::Ok
        }
        GuiCommand::SetTls { enabled } => {
            info!("IPC: set TLS = {enabled}");
            let mut s = state.lock().unwrap();
            s.tls_enabled = enabled;
            if let Ok(mut cfg) = manguesechee_core::config::load() {
                cfg.network.tls = enabled;
                let _ = manguesechee_core::config::save(&cfg);
            }
            AgentEvent::Ok
        }
        GuiCommand::SetCursorLock { locked } => {
            info!("IPC: set cursor lock = {locked}");
            let mut s = state.lock().unwrap();
            s.cursor_locked = locked;
            if let Ok(mut cfg) = manguesechee_core::config::load() {
                cfg.input.cursor_locked = locked;
                let _ = manguesechee_core::config::save(&cfg);
            }
            AgentEvent::Ok
        }
        GuiCommand::ToggleCursorLock => {
            let mut s = state.lock().unwrap();
            let new_lock = !s.cursor_locked;
            info!("IPC: toggle cursor lock -> {new_lock}");
            s.cursor_locked = new_lock;
            if let Ok(mut cfg) = manguesechee_core::config::load() {
                cfg.input.cursor_locked = new_lock;
                let _ = manguesechee_core::config::save(&cfg);
            }
            AgentEvent::Ok
        }
        GuiCommand::SwitchScreen => {
            let is_fwd = state.lock().unwrap().is_forwarding;
            info!("IPC: switch screen requested (currently forwarding={is_fwd})");
            if is_fwd {
                let _ = broadcast_tx.send(manguesechee_core::protocol::Message::ReturnControl {
                    edge: manguesechee_core::protocol::Edge::Right,
                    ratio: None,
                });
            } else {
                let _ = broadcast_tx.send(manguesechee_core::protocol::Message::EdgeCrossed {
                    edge: manguesechee_core::protocol::Edge::Right,
                    ratio: None,
                });
            }
            AgentEvent::Ok
        }
        GuiCommand::SetEdgeResistance { delay_ms } => {
            info!("IPC: set edge resistance delay to {delay_ms} ms");
            state.lock().unwrap().edge_delay_ms = delay_ms;
            if let Ok(mut cfg) = manguesechee_core::config::load() {
                cfg.input.switch_delay_ms = delay_ms;
                let _ = manguesechee_core::config::save(&cfg);
            }
            AgentEvent::Ok
        }
        GuiCommand::ForgetPeer { address } => {
            info!("IPC: forget peer at {address}");
            let mut s = state.lock().unwrap();
            s.peers.retain(|p| p.address != address);
            if let Ok(mut known) = crate::session::load_known_peers() {
                known.peers.retain(|p| p.name != address && p.id != address);
                let _ = crate::session::save_known_peers(&known);
            }
            if let Ok(mut cfg) = manguesechee_core::config::load() {
                cfg.peers.retain(|p| p.address.as_deref() != Some(&address));
                let _ = manguesechee_core::config::save(&cfg);
            }
            AgentEvent::Ok
        }
        GuiCommand::SyncTopology { address, position } => {
            info!("IPC: sync topology for {address}: {position}");
            let pos_clean = position.trim().to_lowercase();
            let (gx, gy) = match pos_clean.as_str() {
                "left" => (-1, 0),
                "right" => (1, 0),
                "above" | "top" => (0, 1),
                "below" | "bottom" => (0, -1),
                _ => (1, 0),
            };
            {
                let mut s = state.lock().unwrap();
                s.topology_configured = true;
                let single_peer = s.peers.len() == 1;
                for p in &mut s.peers {
                    if address.is_empty()
                        || single_peer
                        || p.address == address
                        || address.contains(&p.address)
                        || p.address.contains(&address)
                    {
                        p.position = pos_clean.clone();
                        p.grid_x = gx;
                        p.grid_y = gy;
                    }
                }
            }
            if let Ok(mut cfg) = manguesechee_core::config::load() {
                // If there are real peer entries with addresses, prune phantom peers without address
                if cfg.peers.len() > 1 && cfg.peers.iter().any(|p| p.address.as_ref().map(|a| !a.trim().is_empty()).unwrap_or(false)) {
                    cfg.peers.retain(|p| p.address.as_ref().map(|a| !a.trim().is_empty()).unwrap_or(false));
                }
                let single_peer = cfg.peers.len() == 1;
                for p in &mut cfg.peers {
                    if address.is_empty()
                        || single_peer
                        || p.address.as_deref().unwrap_or("") == address
                        || p.address.as_deref().is_some_and(|a| !a.is_empty() && address.contains(a))
                        || p.address.as_deref().unwrap_or("").contains(&address)
                    {
                        p.position = pos_clean.clone();
                        p.grid_x = Some(gx);
                        p.grid_y = Some(gy);
                    }
                }
                let _ = manguesechee_core::config::save(&cfg);
            }
            let _ = broadcast_tx.send(manguesechee_core::protocol::Message::TopologySync { position: pos_clean });
            AgentEvent::Ok
        }
        GuiCommand::SyncTopologyGrid { address, grid_x, grid_y } => {
            info!("IPC: sync topology grid for {address}: ({grid_x}, {grid_y})");
            let pos_str = match (grid_x, grid_y) {
                (-1, 0) => "left".to_string(),
                (1, 0) => "right".to_string(),
                (0, 1) => "above".to_string(),
                (0, -1) => "below".to_string(),
                (x, _) if x < 0 => "left".to_string(),
                (x, _) if x > 0 => "right".to_string(),
                (_, y) if y > 0 => "above".to_string(),
                _ => "below".to_string(),
            };
            {
                let mut s = state.lock().unwrap();
                s.topology_configured = true;
                let single_peer = s.peers.len() == 1;
                for p in &mut s.peers {
                    if address.is_empty()
                        || single_peer
                        || p.address == address
                        || address.contains(&p.address)
                        || p.address.contains(&address)
                    {
                        p.position = pos_str.clone();
                        p.grid_x = grid_x;
                        p.grid_y = grid_y;
                    }
                }
            }
            if let Ok(mut cfg) = manguesechee_core::config::load() {
                let single_peer = cfg.peers.len() == 1;
                for p in &mut cfg.peers {
                    if address.is_empty()
                        || single_peer
                        || p.address.as_deref().unwrap_or("") == address
                        || p.address.as_deref().is_some_and(|a| !a.is_empty() && address.contains(a))
                        || p.address.as_deref().unwrap_or("").contains(&address)
                    {
                        p.position = pos_str.clone();
                        p.grid_x = Some(grid_x);
                        p.grid_y = Some(grid_y);
                    }
                }
                let _ = manguesechee_core::config::save(&cfg);
            }
            let _ = broadcast_tx.send(manguesechee_core::protocol::Message::TopologySync { position: pos_str });
            AgentEvent::Ok
        }
        GuiCommand::Shutdown => {
            info!("IPC: shutdown requested");
            std::process::exit(0);
        }
    }
}
