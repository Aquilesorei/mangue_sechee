//! Unix socket IPC server — lets the GUI control the running agent.

use anyhow::Context;
use manguesechee_core::ipc::{self, AgentEvent, GuiCommand, PeerInfo};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tracing::{info, warn};

#[derive(Debug, Clone, Default)]
pub struct AgentState {
    pub local_name:          String,
    pub connected_to:        Option<String>,
    pub discovery:           bool,
    pub cursor_locked:       bool,
    pub last_error:          Option<String>,
    pub topology_configured: bool,
    pub disconnect_requested: bool,
    pub peers:               Vec<PeerInfo>,
    pub active_transfers:    Vec<manguesechee_core::ipc::FileTransferInfo>,
    pub transfer_history:    Vec<manguesechee_core::ipc::TransferHistoryEntry>,
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
                local_name:          s.local_name.clone(),
                connected_to:        s.connected_to.clone(),
                discovery:           s.discovery,
                cursor_locked:       s.cursor_locked,
                last_error:          s.last_error.clone(),
                topology_configured: s.topology_configured,
                peers:               s.peers.clone(),
                active_transfers:    s.active_transfers.clone(),
                transfer_history:    s.transfer_history.clone(),
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
                    }
                }
                let _ = manguesechee_core::config::save(&cfg);
            }
            let _ = broadcast_tx.send(manguesechee_core::protocol::Message::TopologySync { position: pos_clean });
            AgentEvent::Ok
        }
        GuiCommand::Shutdown => {
            info!("IPC: shutdown requested");
            std::process::exit(0);
        }
    }
}
