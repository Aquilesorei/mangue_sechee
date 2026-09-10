//! Unix socket IPC server — lets the GUI control the running agent.

use anyhow::Context;
use manguesechee_core::ipc::{self, AgentEvent, GuiCommand, PeerInfo};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tracing::{info, warn};

#[derive(Debug, Clone, Default)]
pub struct AgentState {
    pub local_name:   String,
    pub connected_to: Option<String>,
    pub discovery:    bool,
    pub peers:        Vec<PeerInfo>,
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
    state: SharedState,
    connect_tx: tokio::sync::mpsc::UnboundedSender<String>,
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
        tokio::spawn(async move {
            if let Err(e) = handle_client(stream, state, connect_tx).await {
                warn!("IPC client: {e}");
            }
        });
    }
}

async fn handle_client(
    stream: tokio::net::UnixStream,
    state:  SharedState,
    connect_tx: tokio::sync::mpsc::UnboundedSender<String>,
) -> anyhow::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await? {
        let event = match serde_json::from_str::<GuiCommand>(&line) {
            Ok(cmd) => handle_command(cmd, &state, &connect_tx),
            Err(e)  => AgentEvent::Error { message: e.to_string() },
        };
        let mut resp = serde_json::to_string(&event)?;
        resp.push('\n');
        writer.write_all(resp.as_bytes()).await?;
    }
    Ok(())
}

fn handle_command(
    cmd: GuiCommand,
    state: &SharedState,
    connect_tx: &tokio::sync::mpsc::UnboundedSender<String>,
) -> AgentEvent {
    match cmd {
        GuiCommand::GetStatus => {
            let s = state.lock().unwrap();
            AgentEvent::Status {
                local_name:   s.local_name.clone(),
                connected_to: s.connected_to.clone(),
                discovery:    s.discovery,
                peers:        s.peers.clone(),
            }
        }
        GuiCommand::Connect { address } => {
            info!("IPC: connect requested to {address}");
            let _ = connect_tx.send(address);
            AgentEvent::Ok
        }
        GuiCommand::Disconnect => {
            state.lock().unwrap().connected_to = None;
            AgentEvent::Ok
        }
        GuiCommand::SetDiscovery { enabled } => {
            state.lock().unwrap().discovery = enabled;
            AgentEvent::Ok
        }
        GuiCommand::Shutdown => {
            info!("IPC: shutdown requested");
            std::process::exit(0);
        }
    }
}
