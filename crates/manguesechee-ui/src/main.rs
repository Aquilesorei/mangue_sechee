slint::include_modules!();

use manguesechee_core::{config, ipc};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use tracing::{info, warn};

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let window = MainWindow::new()?;
    let first_run = !config::config_path().exists();

    if first_run {
        info!("first run — showing wizard");
        window.set_view(AppView::Wizard);
    } else {
        window.set_view(AppView::Connecting);
        window.set_status("Starting agent…".into());
        if let Ok(cfg) = config::load() {
            window.set_local_name(cfg.device.name.clone().into());
            window.set_discovery(cfg.network.discovery);
        }
        ensure_agent_running();
        window.set_status("Ready".into());
        window.set_view(AppView::Main);
    }

    // Wizard finish
    {
        let w = window.as_weak();
        window.on_wizard_finish(move |role, peer_addr, side| {
            let w = w.upgrade().unwrap();
            w.set_status("Saving configuration…".into());
            let mut cfg = config::Config::default();
            if !peer_addr.is_empty() {
                cfg.peers.push(config::PeerConfig {
                    id:       "peer".to_string(),
                    address:  Some(peer_addr.to_string()),
                    position: side.to_string(),
                });
            }
            if let Err(e) = config::save(&cfg) {
                w.set_status(format!("Config error: {e}").into());
                return;
            }
            w.set_status("Starting agent…".into());
            ensure_agent_running();
            enable_autostart();
            w.set_local_name(cfg.device.name.clone().into());
            w.set_discovery(cfg.network.discovery);
            w.set_status("Ready".into());
            w.set_view(AppView::Main);
            let _ = role; // role stored in config via input.enabled — expand later
        });
    }

    // Connect
    {
        let w = window.as_weak();
        window.on_connect_requested(move |addr| {
            send_ipc(ipc::GuiCommand::Connect { address: addr.to_string() });
            if let Some(w) = w.upgrade() {
                w.set_status(format!("Connecting to {addr}…").into());
            }
        });
    }

    // Disconnect
    {
        let w = window.as_weak();
        window.on_disconnect_requested(move || {
            send_ipc(ipc::GuiCommand::Disconnect);
            if let Some(w) = w.upgrade() { w.set_status("Disconnected".into()); }
        });
    }

    // Discovery toggle
    window.on_toggle_discovery(move |enabled| {
        send_ipc(ipc::GuiCommand::SetDiscovery { enabled });
    });

    // Poll status every 2 s
    let _poll_timer = {
        let w = window.as_weak();
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_secs(2),
            move || {
                let Some(w) = w.upgrade() else { return };
                if w.get_view() != AppView::Main { return; }
                if let Some(ipc::AgentEvent::Status {
                    local_name, connected_to, discovery, peers,
                }) = poll_status()
                {
                    w.set_local_name(local_name.into());
                    w.set_discovery(discovery);
                    w.set_status(match &connected_to {
                        Some(p) => format!("Forwarding → {p}").into(),
                        None    => "Ready".into(),
                    });
                    let entries: Vec<PeerEntry> = peers.iter().map(|p| PeerEntry {
                        name:      p.name.clone().into(),
                        address:   p.address.clone().into(),
                        paired:    p.paired,
                        connected: p.connected,
                    }).collect();
                    w.set_peers(entries.as_slice().into());
                }
            },
        );
        timer
    };

    window.run()?;
    Ok(())
}

fn ensure_agent_running() {
    if agent_alive() { info!("agent already running"); return; }
    let bin = std::env::current_exe().ok()
        .and_then(|p| p.parent().map(|d| d.join("manguesechee-agent")))
        .filter(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from("manguesechee-agent"));
    match Command::new(&bin).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
        Ok(_)  => { info!("agent spawned"); std::thread::sleep(std::time::Duration::from_millis(800)); }
        Err(e) => warn!("spawn failed: {e}"),
    }
}

fn agent_alive() -> bool {
    let Ok(s) = std::fs::read_to_string(ipc::pid_file()) else { return false; };
    let Ok(pid) = s.trim().parse::<i32>() else { return false; };
    unsafe { libc::kill(pid, 0) == 0 }
}

fn send_ipc(cmd: ipc::GuiCommand) {
    use std::io::Write;
    if let Ok(mut s) = std::os::unix::net::UnixStream::connect(ipc::socket_path()) {
        if let Ok(j) = serde_json::to_string(&cmd) { let _ = s.write_all(format!("{j}\n").as_bytes()); }
    }
}

fn poll_status() -> Option<ipc::AgentEvent> {
    use std::io::{BufRead, BufReader, Write};
    let s = std::os::unix::net::UnixStream::connect(ipc::socket_path()).ok()?;
    s.set_read_timeout(Some(std::time::Duration::from_millis(400))).ok()?;
    let mut w = s.try_clone().ok()?;
    w.write_all(format!("{j}\n", j = serde_json::to_string(&ipc::GuiCommand::GetStatus).ok()?).as_bytes()).ok()?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).ok()?;
    serde_json::from_str(&line).ok()
}

fn enable_autostart() {
    let _ = Command::new("systemctl").args(["--user","enable","--now","manguesechee-agent"]).status();
}
