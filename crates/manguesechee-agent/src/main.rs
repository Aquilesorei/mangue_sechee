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

    {
        let state = Arc::clone(&ipc_state);
        let ctx = connect_tx.clone();
        tokio::spawn(async move {
            if let Err(e) = ipc_server::run(state, ctx).await {
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

        tokio::spawn(async move {
            while let Some(addr) = connect_rx.recv().await {
                let name = name.clone();
                let id = id.clone();
                let mouse = mouse.clone();
                let kb = kb.clone();
                let state = Arc::clone(&state);
                info!("Starting controller connection to {addr}");

                tokio::spawn(async move {
                    state.lock().unwrap().connected_to = Some(addr.clone());
                    if let Err(e) = client::connect_to(&addr, name, id, mouse, kb, w, h, deadzone, delay, Arc::clone(&state)).await {
                        tracing::error!("controller session to {addr} failed: {e:#}");
                    }
                    state.lock().unwrap().connected_to = None;
                });
            }
        });
    }

    // Connect if --connect or config peer was specified at launch
    if let Some(addr) = opts.peer_addr {
        let _ = connect_tx.send(addr);
    }

    server::run(listener, local_name, local_id, opts.screen_width, opts.screen_height).await;
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
    let mut screen_width  = cfg.screen.width;
    let mut screen_height = cfg.screen.height;
    let mut width_set     = false;
    let mut height_set    = false;
    let mut i = 1;

    while i < args.len() {
        match args[i].as_str() {
            "--port"     => { i += 1; if let Some(v) = args.get(i) { port          = v.parse().unwrap_or(port); } }
            "--connect"  => { i += 1; peer_addr     = args.get(i).cloned(); }
            "--mouse"    => { i += 1; mouse_path    = args.get(i).map(PathBuf::from); }
            "--keyboard" => { i += 1; keyboard_path = args.get(i).map(PathBuf::from); }
            "--width"    => {
                i += 1;
                if let Some(v) = args.get(i) { screen_width  = v.parse().unwrap_or(screen_width); width_set  = true; }
            }
            "--height"   => {
                i += 1;
                if let Some(v) = args.get(i) { screen_height = v.parse().unwrap_or(screen_height); height_set = true; }
            }
            _ => {}
        }
        i += 1;
    }

    // If neither CLI nor config provided explicit dimensions, detect from DRM
    // (works on Wayland/COSMIC without requiring X11).
    let config_is_default = cfg.screen.width == 1920 && cfg.screen.height == 1080;
    if config_is_default && !width_set && !height_set {
        let (w, h) = manguesechee_input::detect_screen_size();
        screen_width  = w;
        screen_height = h;
    }

    let peer_addr = peer_addr.or_else(|| {
        cfg.peers.iter().find_map(|p| p.address.clone())
    });

    Opts { port, peer_addr, mouse_path, keyboard_path, screen_width, screen_height }
}
