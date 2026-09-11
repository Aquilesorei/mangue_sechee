//! Manguesechee agent
//!
//! Config: ~/.config/manguesechee/config.toml (created on first run)
//! CLI args override config values.
//!
//! Usage:
//!   manguesechee-agent                            # controlled peer
//!   manguesechee-agent --connect <host:port>      # controller
//!
//! Options:
//!   --port      <n>         Listen port     (default from config: 24800)
//!   --connect   <host:port> Peer address
//!   --mouse     <path>      Mouse device    (default: auto-detect)
//!   --keyboard  <path>      Keyboard device (default: auto-detect)
//!   --width     <px>        Screen width    (default from config: 1920)
//!   --height    <px>        Screen height   (default from config: 1080)

mod clipboard;
mod client;
mod discovery;
mod edge;
mod ipc_server;
mod server;
mod session;

use anyhow::Context;
use manguesechee_core::config;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;
use uuid::Uuid;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    clipboard::ensure_display_env();

    let cfg  = config::ensure_default().context("load config")?;
    let args: Vec<String> = std::env::args().collect();
    let opts = parse_args(&args, &cfg);

    let local_name = cfg.device.name.clone();
    let local_id = if cfg.device.id.trim().is_empty() {
        let new_id = Uuid::new_v4().to_string();
        let mut updated = cfg.clone();
        updated.device.id = new_id.clone();
        let _ = config::save(&updated);
        new_id
    } else {
        cfg.device.id.clone()
    };

    info!(
        name   = %local_name,
        id     = %local_id,
        port   = opts.port,
        screen = format!("{}×{}", opts.screen_width, opts.screen_height),
        "manguesechee-agent starting"
    );

    // ── IPC — PID file + Unix socket ─────────────────────────────────────────────
    let _pid_guard = ipc_server::PidGuard::write().context("write PID file")?;

    let known_store = session::load_known_peers().unwrap_or_default();
    let initial_peers: Vec<manguesechee_core::ipc::PeerInfo> = cfg.peers.iter().filter_map(|p| {
        p.address.as_ref().map(|addr| {
            let is_paired = known_store.contains(&p.id) || known_store.peers.iter().any(|kp| kp.name == p.id);
            manguesechee_core::ipc::PeerInfo {
                name: p.id.clone(),
                address: addr.clone(),
                paired: is_paired,
                connected: false,
                position: p.position.clone(),
            }
        })
    }).collect();

    let ipc_state: ipc_server::SharedState = Arc::new(std::sync::Mutex::new(
        ipc_server::AgentState {
            local_name: local_name.clone(),
            discovery:  cfg.network.discovery,
            peers:      initial_peers,
            ..Default::default()
        }
    ));
    let (connect_tx, mut connect_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let (broadcast_tx, _) = tokio::sync::broadcast::channel::<manguesechee_core::protocol::Message>(16);

    {
        let state = Arc::clone(&ipc_state);
        let ctx = connect_tx.clone();
        let b_tx = broadcast_tx.clone();
        tokio::spawn(async move {
            if let Err(e) = ipc_server::run(state, ctx, b_tx).await {
                tracing::error!("IPC server: {e:#}");
            }
        });
    }

    // ── mDNS discovery (background) ───────────────────────────────────────────
    {
        let name = local_name.clone();
        let id   = local_id.clone();
        let port = opts.port;
        let state = Arc::clone(&ipc_state);
        tokio::spawn(async move {
            discovery::run(name, id, port, state).await;
        });
    }

    // ── TCP listener ──────────────────────────────────────────────────────────
    let listener = manguesechee_network::listen(opts.port)
        .await
        .context("failed to start listener")?;

    // ── Outbound controller session worker ───────────────────────────────────
    {
        let name   = local_name.clone();
        let id     = local_id.clone();
        let mouse  = opts.mouse_path.clone();
        let kb     = opts.keyboard_path.clone();
        let w      = opts.screen_width;
        let h      = opts.screen_height;
        let deadzone = cfg.input.corner_deadzone_px;
        let delay    = cfg.input.switch_delay_ms;
        let state  = Arc::clone(&ipc_state);
        let b_tx   = broadcast_tx.clone();
        let connect_tx_retry = connect_tx.clone();

        tokio::spawn(async move {
            while let Some(addr) = connect_rx.recv().await {
                {
                    let s = state.lock().unwrap();
                    if s.connected_to.as_deref() == Some(&addr) {
                        info!("already connected or connecting to {addr} — skipping duplicate connect");
                        continue;
                    }
                }
                let name = name.clone();
                let id = id.clone();
                let mouse = mouse.clone();
                let kb = kb.clone();
                let state = Arc::clone(&state);
                let b_tx = b_tx.clone();
                let retry_tx = connect_tx_retry.clone();
                info!("Starting controller connection to {addr}");

                tokio::spawn(async move {
                    state.lock().unwrap().connected_to = Some(addr.clone());
                    if let Err(e) = client::connect_to(&addr, name, id, mouse, kb, w, h, deadzone, delay, Arc::clone(&state), b_tx).await {
                        tracing::error!("controller session to {addr} failed: {e:#}");
                        state.lock().unwrap().last_error = Some(format!("Connection to {addr} failed: {e}"));
                    }
                    state.lock().unwrap().connected_to = None;

                    // If still configured in peers, retry after 3 seconds
                    let should_retry = {
                        let s = state.lock().unwrap();
                        s.peers.iter().any(|p| p.address == addr || addr.contains(&p.address) || p.address.contains(&addr))
                    };
                    if should_retry {
                        info!("will retry controller connection to {addr} in 3 seconds…");
                        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
                        let _ = retry_tx.send(addr);
                    }
                });
            }
        });
    }

    // Connect if --connect or config peer was specified at launch
    if let Some(addr) = opts.peer_addr {
        let _ = connect_tx.send(addr);
    }

    server::run(
        listener,
        local_name,
        local_id,
        opts.screen_width,
        opts.screen_height,
        Arc::clone(&ipc_state),
        connect_tx.clone(),
        broadcast_tx.clone(),
    ).await;
    Ok(())
}

// ── CLI ───────────────────────────────────────────────────────────────────────

struct Opts {
    port:          u16,
    peer_addr:     Option<String>,
    mouse_path:    Option<PathBuf>,
    keyboard_path: Option<PathBuf>,
    screen_width:  u32,
    screen_height: u32,
}

fn parse_args(args: &[String], cfg: &config::Config) -> Opts {
    let mut port          = cfg.network.port;
    let mut peer_addr     = None;
    let mut mouse_path    = cfg.input.mouse_device.as_deref().map(PathBuf::from);
    let mut keyboard_path = cfg.input.keyboard_device.as_deref().map(PathBuf::from);
    let mut cli_width     = None;
    let mut cli_height    = None;
    let mut i = 1;

    while i < args.len() {
        match args[i].as_str() {
            "--port"     => { i += 1; if let Some(v) = args.get(i) { port          = v.parse().unwrap_or(port); } }
            "--connect"  => { i += 1; peer_addr     = args.get(i).cloned(); }
            "--mouse"    => { i += 1; mouse_path    = args.get(i).map(PathBuf::from); }
            "--keyboard" => { i += 1; keyboard_path = args.get(i).map(PathBuf::from); }
            "--width"    => {
                i += 1;
                if let Some(v) = args.get(i) { cli_width  = v.parse().ok(); }
            }
            "--height"   => {
                i += 1;
                if let Some(v) = args.get(i) { cli_height = v.parse().ok(); }
            }
            _ => {}
        }
        i += 1;
    }

    // Screen dimension detection is the default; use config as fallback
    let (detected_width, detected_height) = match manguesechee_input::try_detect_screen_size() {
        Some((w, h)) if w > 0 && h > 0 => {
            info!("using auto-detected display dimensions: {w}×{h}");
            (w, h)
        }
        _ => {
            let fw = if cfg.screen.width > 0 { cfg.screen.width } else { 1920 };
            let fh = if cfg.screen.height > 0 { cfg.screen.height } else { 1080 };
            info!("screen detection unavailable, using fallback dimensions from config: {fw}×{fh}");
            (fw, fh)
        }
    };

    let screen_width  = cli_width.unwrap_or(detected_width);
    let screen_height = cli_height.unwrap_or(detected_height);

    let peer_addr = peer_addr.or_else(|| {
        cfg.peers.iter().find_map(|p| p.address.clone())
    });

    Opts { port, peer_addr, mouse_path, keyboard_path, screen_width, screen_height }
}
