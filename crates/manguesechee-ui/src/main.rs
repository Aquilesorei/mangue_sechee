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
        window.set_status("Connecting to agent…".into());
        ensure_agent_running();
        let running = is_service_active();
        window.set_is_service_running(running);
        window.set_status(if running { "Ready".into() } else { "Service Stopped".into() });
        window.set_view(AppView::Main);
    }

    // Populate UI from current config
    if let Ok(cfg) = config::load() {
        populate_settings_from_config(&window, &cfg);
    }

    // ── Service Controls (Start, Stop, Restart) ──────────────────────────────
    {
        let w = window.as_weak();
        window.on_start_service(move || {
            let Some(w) = w.upgrade() else { return };
            info!("starting agent service");
            let _ = Command::new("systemctl")
                .args(["--user", "start", "manguesechee-agent"])
                .status();
            ensure_agent_running();
            w.set_is_service_running(true);
            w.set_status("Running".into());
            w.set_settings_feedback("✓ Agent daemon started".into());
        });
    }
    {
        let w = window.as_weak();
        window.on_stop_service(move || {
            let Some(w) = w.upgrade() else { return };
            info!("stopping agent service");
            let _ = Command::new("systemctl")
                .args(["--user", "stop", "manguesechee-agent"])
                .status();
            send_ipc(ipc::GuiCommand::Shutdown);
            w.set_is_service_running(false);
            w.set_status("Service Stopped".into());
            w.set_settings_feedback("✓ Agent daemon stopped".into());
        });
    }
    {
        let w = window.as_weak();
        window.on_restart_service(move || {
            let Some(w) = w.upgrade() else { return };
            info!("restarting agent service");
            w.set_status("Restarting…".into());
            let _ = Command::new("systemctl")
                .args(["--user", "restart", "manguesechee-agent"])
                .status();
            w.set_is_service_running(true);
            w.set_settings_feedback("✓ Agent daemon restarted".into());
        });
    }

    // ── Setup Wizard Finish ──────────────────────────────────────────────────
    {
        let w = window.as_weak();
        window.on_wizard_finish(move |role, peer_addr, side| {
            let Some(w) = w.upgrade() else { return };
            w.set_status("Saving configuration…".into());

            let mut cfg = config::load().unwrap_or_default();
            // Role: 0=Both, 1=Controller only, 2=Peer only
            cfg.input.enabled = role != 2;

            if !peer_addr.trim().is_empty() {
                cfg.peers.clear();
                cfg.peers.push(config::PeerConfig {
                    id:       "primary-peer".to_string(),
                    address:  Some(peer_addr.trim().to_string()),
                    position: side.trim().to_lowercase(),
                });
            }

            if let Err(e) = config::save(&cfg) {
                w.set_status(format!("Config error: {e}").into());
                return;
            }

            w.set_status("Starting agent…".into());
            ensure_agent_running();
            enable_autostart();

            populate_settings_from_config(&w, &cfg);
            w.set_is_service_running(true);
            w.set_status("Ready".into());
            w.set_view(AppView::Main);
        });
    }

    // ── Settings Save ────────────────────────────────────────────────────────
    {
        let w = window.as_weak();
        window.on_save_settings(move || {
            let Some(w) = w.upgrade() else { return };
            let mut cfg = config::load().unwrap_or_default();

            // Device Name
            let dev_name = w.get_setting_name().trim().to_string();
            if !dev_name.is_empty() {
                cfg.device.name = dev_name.clone();
                w.set_local_name(dev_name.into());
            }

            // Role: 0=Both, 1=Controller, 2=Peer
            let role_idx = w.get_setting_role();
            cfg.input.enabled = role_idx != 2;

            // Network Port
            if let Ok(port) = w.get_setting_port().trim().parse::<u16>() {
                cfg.network.port = port;
            }

            // Discovery & Clipboard
            cfg.network.discovery = w.get_setting_discovery();
            cfg.clipboard.enabled = w.get_setting_clipboard();

            // Screen Dimensions
            if let Ok(width) = w.get_setting_width().trim().parse::<u32>() {
                cfg.screen.width = width;
            }
            if let Ok(height) = w.get_setting_height().trim().parse::<u32>() {
                cfg.screen.height = height;
            }

            // Peer Config
            let peer_ip = w.get_setting_peer_addr().trim().to_string();
            let peer_pos = w.get_setting_peer_pos().trim().to_lowercase();
            if !peer_ip.is_empty() {
                cfg.peers.clear();
                cfg.peers.push(config::PeerConfig {
                    id: "primary-peer".to_string(),
                    address: Some(peer_ip),
                    position: peer_pos,
                });
            }

            match config::save(&cfg) {
                Ok(_) => {
                    info!("configuration saved");
                    w.set_settings_feedback("✓ Settings saved! Restarting agent service…".into());
                    restart_agent_service();
                    populate_settings_from_config(&w, &cfg);
                }
                Err(e) => {
                    warn!("failed to save config: {e}");
                    w.set_settings_feedback(format!("Error: {e}").into());
                }
            }
        });
    }

    // ── Topology Save ────────────────────────────────────────────────────────
    {
        let w = window.as_weak();
        window.on_save_topology(move |pos| {
            let Some(w) = w.upgrade() else { return };
            let pos_clean = pos.trim().to_lowercase();
            let mut cfg = config::load().unwrap_or_default();

            if cfg.peers.is_empty() {
                cfg.peers.push(config::PeerConfig {
                    id: "primary-peer".to_string(),
                    address: None,
                    position: pos_clean.clone(),
                });
            } else {
                for p in &mut cfg.peers {
                    p.position = pos_clean.clone();
                }
            }

            if let Ok(_) = config::save(&cfg) {
                w.set_setting_peer_pos(pos_clean.clone().into());
                w.set_settings_feedback(format!("✓ Topology updated: Peer positioned on the {pos_clean}").into());
                restart_agent_service();
            }
        });
    }

    // ── Auto-Detect Screen Geometry ──────────────────────────────────────────
    {
        let w = window.as_weak();
        window.on_auto_detect_screen(move || {
            let Some(w) = w.upgrade() else { return };
            let (width, height) = manguesechee_input::detect_screen_size();
            w.set_setting_width(width.to_string().into());
            w.set_setting_height(height.to_string().into());
            w.set_settings_feedback(format!("✓ Auto-detected screen geometry: {width}×{height}").into());
        });
    }

    // ── Logs Console ─────────────────────────────────────────────────────────
    {
        let w = window.as_weak();
        window.on_copy_logs(move || {
            let Some(w) = w.upgrade() else { return };
            let logs = fetch_journal_logs();
            let all_text = logs.join("\n");
            let success = copy_to_clipboard(&all_text);
            if success {
                w.set_logs_feedback("✓ Copied to clipboard!".into());
            } else {
                w.set_logs_feedback("⚠ Copy failed".into());
            }

            let w_feedback = w.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_secs(3), move || {
                if let Some(w) = w_feedback.upgrade() {
                    w.set_logs_feedback("".into());
                }
            });
        });
    }
    {
        let w = window.as_weak();
        window.on_refresh_logs(move || {
            let Some(w) = w.upgrade() else { return };
            let logs = fetch_journal_logs();
            let slint_logs: Vec<slint::SharedString> = logs.into_iter().map(Into::into).collect();
            w.set_log_lines(slint_logs.as_slice().into());
        });
    }
    {
        let w = window.as_weak();
        window.on_clear_logs(move || {
            let Some(w) = w.upgrade() else { return };
            w.set_log_lines((&[] as &[slint::SharedString]).into());
        });
    }

    // ── Connect & Disconnect IPC ─────────────────────────────────────────────
    {
        let w = window.as_weak();
        window.on_connect_requested(move |addr| {
            send_ipc(ipc::GuiCommand::Connect { address: addr.to_string() });
            if let Some(w) = w.upgrade() {
                w.set_status(format!("Connecting to {addr}…").into());
            }
        });
    }
    {
        let w = window.as_weak();
        window.on_disconnect_requested(move || {
            send_ipc(ipc::GuiCommand::Disconnect);
            if let Some(w) = w.upgrade() {
                w.set_status("Disconnected".into());
            }
        });
    }
    window.on_toggle_discovery(move |enabled| {
        send_ipc(ipc::GuiCommand::SetDiscovery { enabled });
    });

    // ── Background Polling Timer (Every 2s) ──────────────────────────────────
    let _poll_timer = {
        let w = window.as_weak();
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_secs(2),
            move || {
                let Some(w) = w.upgrade() else { return };
                if w.get_view() != AppView::Main { return; }

                let service_running = is_service_active();
                w.set_is_service_running(service_running);

                if !service_running {
                    w.set_status("Service Stopped".into());
                    return;
                }

                // Periodic status polling via IPC
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

                    let default_pos = w.get_setting_peer_pos().to_string();
                    let entries: Vec<PeerEntry> = peers.iter().map(|p| PeerEntry {
                        name:      p.name.clone().into(),
                        address:   p.address.clone().into(),
                        paired:    p.paired,
                        connected: p.connected,
                        position:  default_pos.clone().into(),
                    }).collect();
                    w.set_peers(entries.as_slice().into());
                }

                // If currently on the logs tab, auto-refresh logs
                if w.get_active_tab() == 3 {
                    let logs = fetch_journal_logs();
                    let slint_logs: Vec<slint::SharedString> = logs.into_iter().map(Into::into).collect();
                    w.set_log_lines(slint_logs.as_slice().into());
                }
            },
        );
        timer
    };

    window.run()?;
    Ok(())
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn populate_settings_from_config(w: &MainWindow, cfg: &config::Config) {
    w.set_local_ip(detect_local_ip().into());
    w.set_local_name(cfg.device.name.clone().into());
    w.set_setting_name(cfg.device.name.clone().into());
    w.set_setting_port(cfg.network.port.to_string().into());
    w.set_setting_discovery(cfg.network.discovery);
    w.set_setting_clipboard(cfg.clipboard.enabled);
    w.set_setting_width(cfg.screen.width.to_string().into());
    w.set_setting_height(cfg.screen.height.to_string().into());

    // Role: 0=Both, 1=Controller, 2=Peer
    let role = if !cfg.input.enabled {
        w.set_role_label("Peer only (Server - receives input)".into());
        2
    } else {
        w.set_role_label("Server + Client (Bidirectional)".into());
        0
    };
    w.set_setting_role(role);

    if let Some(p) = cfg.peers.first() {
        w.set_setting_peer_addr(p.address.clone().unwrap_or_default().into());
        w.set_setting_peer_pos(p.position.clone().into());
    } else {
        w.set_setting_peer_pos("right".into());
    }
}

fn detect_local_ip() -> String {
    if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") {
        if socket.connect("8.8.8.8:80").is_ok() {
            if let Ok(addr) = socket.local_addr() {
                let ip = addr.ip();
                if !ip.is_loopback() {
                    return ip.to_string();
                }
            }
        }
    }
    let out = Command::new("hostname").arg("-I").output();
    if let Ok(o) = out {
        let s = String::from_utf8_lossy(&o.stdout);
        if let Some(first_ip) = s.split_whitespace().next() {
            return first_ip.to_string();
        }
    }
    "127.0.0.1".to_string()
}

fn is_service_active() -> bool {
    let out = Command::new("systemctl")
        .args(["--user", "is-active", "manguesechee-agent"])
        .output();
    if let Ok(o) = out {
        if String::from_utf8_lossy(&o.stdout).trim() == "active" {
            return true;
        }
    }
    agent_alive()
}

fn fetch_journal_logs() -> Vec<String> {
    let output = Command::new("journalctl")
        .args(["--user", "-u", "manguesechee-agent", "-n", "100", "--no-pager", "-o", "short-iso"])
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            let lines: Vec<String> = text.lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| l.to_string())
                .collect();
            if lines.is_empty() {
                vec!["[Notice] No journalctl entries available for manguesechee-agent.".into()]
            } else {
                lines
            }
        }
        _ => vec!["[Notice] No journalctl entries available for manguesechee-agent.".into()],
    }
}

fn copy_to_clipboard(text: &str) -> bool {
    // 1. Native / cross-platform via arboard
    if let Ok(mut board) = arboard::Clipboard::new() {
        if board.set_text(text).is_ok() {
            return true;
        }
    }

    // 2. Wayland native fallback via wl-copy
    if let Ok(mut child) = Command::new("wl-copy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            let _ = stdin.write_all(text.as_bytes());
            drop(stdin);
            if let Ok(status) = child.wait() {
                if status.success() {
                    return true;
                }
            }
        }
    }

    // 3. X11 / Xwayland fallback via xclip
    if let Ok(mut child) = Command::new("xclip")
        .args(["-selection", "clipboard"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            let _ = stdin.write_all(text.as_bytes());
            drop(stdin);
            if let Ok(status) = child.wait() {
                if status.success() {
                    return true;
                }
            }
        }
    }

    false
}

fn restart_agent_service() {
    let _ = Command::new("systemctl")
        .args(["--user", "restart", "manguesechee-agent"])
        .spawn();
}

fn ensure_agent_running() {
    if is_service_active() { return; }
    let _ = Command::new("systemctl")
        .args(["--user", "start", "manguesechee-agent"])
        .status();
    if agent_alive() { return; }

    let bin = std::env::current_exe().ok()
        .and_then(|p| p.parent().map(|d| d.join("manguesechee-agent")))
        .filter(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from("manguesechee-agent"));

    match Command::new(&bin).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
        Ok(_)  => { std::thread::sleep(std::time::Duration::from_millis(600)); }
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
        if let Ok(j) = serde_json::to_string(&cmd) {
            let _ = s.write_all(format!("{j}\n").as_bytes());
        }
    }
}

fn poll_status() -> Option<ipc::AgentEvent> {
    use std::io::{BufRead, BufReader, Write};
    let s = std::os::unix::net::UnixStream::connect(ipc::socket_path()).ok()?;
    s.set_read_timeout(Some(std::time::Duration::from_millis(400))).ok()?;
    let mut w = s.try_clone().ok()?;
    let cmd_json = serde_json::to_string(&ipc::GuiCommand::GetStatus).ok()?;
    w.write_all(format!("{cmd_json}\n").as_bytes()).ok()?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).ok()?;
    serde_json::from_str(&line).ok()
}

fn enable_autostart() {
    let _ = Command::new("systemctl").args(["--user", "enable", "--now", "manguesechee-agent"]).status();
}
