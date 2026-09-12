slint::include_modules!();

mod tray;

use manguesechee_core::{config, ipc};
use slint::Model;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use tracing::{info, warn};

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let window = MainWindow::new()?;

    // ── Window Lifecycle: Close-to-tray & Minimize-to-tray ───────────────────
    window.window().on_close_requested(move || {
        info!("Window close requested — minimizing to system tray");
        slint::CloseRequestResponse::HideWindow
    });

    {
        let w = window.as_weak();
        window.on_hide_to_tray(move || {
            if let Some(w) = w.upgrade() {
                info!("Hide to tray clicked");
                let _ = w.hide();
            }
        });
    }

    // ── System Tray (StatusNotifierItem via ksni) ────────────────────────────
    let tray = tray::ManguesecheeTray::new(window.as_weak());
    let tray_handle = tray.spawn_tray();

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

    // ── Toast Dismiss ────────────────────────────────────────────────────────
    {
        let w = window.as_weak();
        window.on_dismiss_toast(move || {
            if let Some(w) = w.upgrade() {
                w.set_toast_visible(false);
            }
        });
    }

    // ── Theme Switcher ────────────────────────────────────────────────────────
    {
        let w = window.as_weak();
        window.on_toggle_theme(move || {
            let Some(w) = w.upgrade() else { return };
            let new_is_dark = !w.get_is_dark();
            w.set_is_dark(new_is_dark);
            if let Ok(mut cfg) = config::load() {
                cfg.theme = if new_is_dark { "dark".into() } else { "light".into() };
                let _ = config::save(&cfg);
            }
            show_toast(&w, if new_is_dark { "Switched to Dark theme" } else { "Switched to Light theme" }, false);
        });
    }
    {
        let w = window.as_weak();
        window.on_theme_changed(move |idx| {
            let Some(w) = w.upgrade() else { return };
            let new_is_dark = idx == 0;
            w.set_is_dark(new_is_dark);
            if let Ok(mut cfg) = config::load() {
                cfg.theme = if new_is_dark { "dark".into() } else { "light".into() };
                let _ = config::save(&cfg);
            }
            show_toast(&w, if new_is_dark { "Switched to Dark theme" } else { "Switched to Light theme" }, false);
        });
    }

    // ── Modal Dialog Controls ────────────────────────────────────────────────
    {
        let w = window.as_weak();
        window.on_open_connect_modal(move || {
            if let Some(w) = w.upgrade() {
                w.set_active_modal(1);
            }
        });
    }
    {
        let w = window.as_weak();
        window.on_close_modal(move || {
            if let Some(w) = w.upgrade() {
                w.set_active_modal(0);
                w.set_modal_target_addr("".into());
            }
        });
    }
    {
        let w = window.as_weak();
        window.on_open_unpair_modal(move |addr| {
            let Some(w) = w.upgrade() else { return };
            let addr_str = addr.to_string();
            w.set_modal_target_addr(addr_str.clone().into());
            w.set_modal_title("Unpair Machine".into());
            w.set_modal_message(format!("Are you sure you want to unpair {addr_str}? You will need to pair again to share controls.").into());
            w.set_active_modal(2);
        });
    }
    {
        let w = window.as_weak();
        window.on_modal_connect_submitted(move |addr, side| {
            let Some(w) = w.upgrade() else { return };
            let addr_str = addr.trim().to_string();
            let side_str = side.trim().to_lowercase();
            w.set_active_modal(0);
            if addr_str.is_empty() { return; }

            if let Ok(mut cfg) = config::load() {
                let mut found = false;
                for p in &mut cfg.peers {
                    if p.address.as_deref() == Some(&addr_str) {
                        p.position = side_str.clone();
                        found = true;
                    }
                }
                if !found {
                    cfg.peers.push(config::PeerConfig::new(
                        format!("peer-{}", addr_str.replace(':', "_")),
                        Some(addr_str.clone()),
                        side_str.clone(),
                    ));
                }
                let _ = config::save(&cfg);
            }

            send_ipc(ipc::GuiCommand::Connect { address: addr_str.clone() });
            show_toast(&w, &format!("Connecting to {addr_str}…"), false);
        });
    }
    {
        let w = window.as_weak();
        window.on_modal_confirm_submitted(move || {
            let Some(w) = w.upgrade() else { return };
            let addr_str = w.get_modal_target_addr().to_string();
            w.set_active_modal(0);
            w.set_modal_target_addr("".into());

            if !addr_str.is_empty() {
                send_ipc(ipc::GuiCommand::ForgetPeer { address: addr_str.clone() });
                if let Ok(mut cfg) = config::load() {
                    cfg.peers.retain(|p| p.address.as_deref() != Some(&addr_str) && !p.address.as_deref().is_some_and(|a| !a.is_empty() && addr_str.contains(a)));
                    let _ = config::save(&cfg);
                }
                let mut peers: Vec<PeerEntry> = w.get_peers().iter().collect();
                peers.retain(|p| p.address != addr_str.as_str() && !addr_str.contains(p.address.as_str()));
                w.set_peers(peers.as_slice().into());
                show_toast(&w, &format!("Unpaired {addr_str}"), false);
            }
        });
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
            show_toast(&w, "Agent daemon started", false);
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
            show_toast(&w, "Agent daemon stopped", false);
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
            show_toast(&w, "Agent daemon restarted", false);
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
                cfg.peers.push(config::PeerConfig::new(
                    "primary-peer",
                    Some(peer_addr.trim().to_string()),
                    side.trim().to_lowercase(),
                ));
            }

            if cfg.screen.width == 1920 && cfg.screen.height == 1080 {
                if let Some((w_det, h_det)) = manguesechee_input::try_detect_screen_size() {
                    cfg.screen.width = w_det;
                    cfg.screen.height = h_det;
                }
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

            // Discovery, Clipboard & TLS
            cfg.network.discovery = w.get_setting_discovery();
            cfg.clipboard.enabled = w.get_setting_clipboard();
            cfg.clipboard.files_enabled = w.get_setting_file_transfer();
            let tls_enabled = w.get_setting_tls();
            cfg.network.tls = tls_enabled;
            send_ipc(ipc::GuiCommand::SetTls { enabled: tls_enabled });

            // Screen Dimensions
            if let Ok(width) = w.get_setting_width().trim().parse::<u32>() {
                cfg.screen.width = width;
            }
            if let Ok(height) = w.get_setting_height().trim().parse::<u32>() {
                cfg.screen.height = height;
            }

            // Sensitivity & Edge tuning
            if let Ok(delay) = w.get_setting_switch_delay().trim().parse::<u32>() {
                cfg.input.switch_delay_ms = delay;
            }
            if let Ok(deadzone) = w.get_setting_deadzone().trim().parse::<u32>() {
                cfg.input.corner_deadzone_px = deadzone;
            }
            if let Ok(velocity) = w.get_setting_velocity().trim().parse::<u32>() {
                cfg.input.edge_velocity_threshold = velocity;
            }
            let cursor_lock = w.get_cursor_locked();
            cfg.input.cursor_locked = cursor_lock;
            send_ipc(ipc::GuiCommand::SetCursorLock { locked: cursor_lock });

            // Global Hotkeys
            cfg.hotkeys.toggle_cursor_lock = w.get_setting_hotkey_lock().trim().to_string();
            cfg.hotkeys.switch_screen = w.get_setting_hotkey_switch().trim().to_string();
            cfg.hotkeys.emergency_escape = w.get_setting_hotkey_escape().trim().to_string();
            cfg.hotkeys.enabled = !cfg.hotkeys.toggle_cursor_lock.eq_ignore_ascii_case("disabled")
                || !cfg.hotkeys.switch_screen.eq_ignore_ascii_case("disabled");

            // Autostart on boot (systemd user unit)
            let autostart_enabled = w.get_setting_autostart();
            if autostart_enabled {
                let _ = Command::new("systemctl").args(["--user", "enable", "manguesechee-agent"]).status();
            } else {
                let _ = Command::new("systemctl").args(["--user", "disable", "manguesechee-agent"]).status();
            }

            // Peer Config
            let peer_ip = w.get_setting_peer_addr().trim().to_string();
            let peer_pos = w.get_setting_peer_pos().trim().to_lowercase();
            if !peer_ip.is_empty() {
                cfg.peers.clear();
                cfg.peers.push(config::PeerConfig::new(
                    "primary-peer",
                    Some(peer_ip),
                    peer_pos,
                ));
            }

            cfg.theme = if w.get_is_dark() { "dark".into() } else { "light".into() };

            match config::save(&cfg) {
                Ok(_) => {
                    info!("configuration saved");
                    show_toast(&w, "Settings saved & service restarted", false);
                    restart_agent_service();
                    populate_settings_from_config(&w, &cfg);
                }
                Err(e) => {
                    warn!("failed to save config: {e}");
                    show_toast(&w, &format!("Config error: {e}"), true);
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

            // Prune phantom peers without address if any valid peer exists
            if cfg.peers.len() > 1 && cfg.peers.iter().any(|p| p.address.as_ref().map(|a| !a.trim().is_empty()).unwrap_or(false)) {
                cfg.peers.retain(|p| p.address.as_ref().map(|a| !a.trim().is_empty()).unwrap_or(false));
            }

            if cfg.peers.is_empty() {
                cfg.peers.push(config::PeerConfig::new(
                    "primary-peer",
                    None,
                    pos_clean.clone(),
                ));
            } else {
                for p in &mut cfg.peers {
                    p.position = pos_clean.clone();
                }
            }

            let _ = config::save(&cfg);
            w.set_setting_peer_pos(pos_clean.clone().into());
            show_toast(&w, &format!("Topology synced: Peer positioned on the {pos_clean}"), false);
            w.set_topology_configured(true);
            let pos_disp = match pos_clean.as_str() {
                "left" => "Left",
                "above" => "Above (Top)",
                "below" => "Below (Bottom)",
                _ => "Right",
            };
            w.set_topology_notice(format!("Peer screen arranged on your {pos_disp} (synchronized with peer).").into());

            // Sync live over IPC to agent & network broadcast to peer
            let target_addr = cfg.peers.iter()
                .find_map(|p| p.address.clone().filter(|a| !a.trim().is_empty()))
                .unwrap_or_default();
            send_ipc(ipc::GuiCommand::SyncTopology {
                address: target_addr,
                position: pos_clean.clone(),
            });
        });
    }

    // ── Topology Grid Save ───────────────────────────────────────────────────
    {
        let w = window.as_weak();
        window.on_save_topology_grid(move |addr, gx, gy| {
            let Some(w) = w.upgrade() else { return };
            let addr_str = addr.to_string();
            let mut cfg = config::load().unwrap_or_default();

            let pos_str = match (gx, gy) {
                (-1, 0) => "left".to_string(),
                (1, 0) => "right".to_string(),
                (0, 1) => "above".to_string(),
                (0, -1) => "below".to_string(),
                (x, _) if x < 0 => "left".to_string(),
                (x, _) if x > 0 => "right".to_string(),
                (_, y) if y > 0 => "above".to_string(),
                _ => "below".to_string(),
            };

            let mut found = false;
            for p in &mut cfg.peers {
                if addr_str.is_empty()
                    || p.address.as_deref() == Some(&addr_str)
                    || (p.address.is_some() && addr_str.contains(p.address.as_ref().unwrap()))
                {
                    p.grid_x = Some(gx);
                    p.grid_y = Some(gy);
                    p.position = pos_str.clone();
                    found = true;
                }
            }
            if !found {
                if !addr_str.is_empty() {
                    cfg.peers.push(config::PeerConfig::with_coords(
                        format!("peer-{}", addr_str.replace(':', "_")),
                        Some(addr_str.clone()),
                        gx,
                        gy,
                    ));
                } else {
                    cfg.peers.push(config::PeerConfig::with_coords(
                        "primary-peer",
                        None,
                        gx,
                        gy,
                    ));
                }
            }
            let _ = config::save(&cfg);

            let target_addr = if !addr_str.is_empty() {
                addr_str.clone()
            } else {
                cfg.peers.iter()
                    .find_map(|p| p.address.clone().filter(|a| !a.trim().is_empty()))
                    .unwrap_or_default()
            };

            send_ipc(ipc::GuiCommand::SyncTopologyGrid {
                address: target_addr,
                grid_x: gx,
                grid_y: gy,
            });

            let mut peers: Vec<PeerEntry> = w.get_peers().iter().collect();
            for p in &mut peers {
                if addr_str.is_empty() || p.address == addr_str.as_str() || addr_str.contains(p.address.as_str()) {
                    p.grid_x = gx;
                    p.grid_y = gy;
                    p.position = pos_str.clone().into();
                }
            }
            w.set_peers(peers.as_slice().into());
            w.set_setting_peer_pos(pos_str.clone().into());
            show_toast(&w, &format!("2D Grid updated: Screen placed at ({gx}, {gy})"), false);
            w.set_topology_configured(true);
            w.set_topology_notice(format!("Screen grid coordinates set to ({gx}, {gy}). Traversable via mouse and Ctrl+Alt+Arrows.").into());
        });
    }

    // ── Auto-Detect Screen Geometry ──────────────────────────────────────────
    {
        let w = window.as_weak();
        window.on_auto_detect_screen(move || {
            let Some(w) = w.upgrade() else { return };
            if let Some((width, height)) = manguesechee_input::try_detect_screen_size() {
                w.set_setting_width(width.to_string().into());
                w.set_setting_height(height.to_string().into());
                show_toast(&w, &format!("Auto-detected screen geometry: {width}×{height}"), false);
            } else {
                show_toast(&w, "Hardware detection unavailable; keeping current geometry", true);
            }
        });
    }

    // ── Logs Console ─────────────────────────────────────────────────────────
    {
        let w = window.as_weak();
        window.on_copy_logs(move || {
            let Some(w) = w.upgrade() else { return };
            let lines: Vec<String> = w.get_log_lines().iter().map(|s| s.to_string()).collect();
            let all_text = if !lines.is_empty() {
                lines.join("\n")
            } else {
                fetch_journal_logs().join("\n")
            };
            let _ = std::fs::write("/tmp/manguesechee-latest-logs.txt", &all_text);
            let success = copy_to_clipboard(&all_text);
            if success {
                show_toast(&w, "Logs copied to clipboard", false);
            } else {
                show_toast(&w, "Logs saved to /tmp/manguesechee-latest-logs.txt", false);
            }
        });
    }
    {
        let w = window.as_weak();
        window.on_refresh_logs(move || {
            let Some(w) = w.upgrade() else { return };
            let logs = fetch_journal_logs();
            let lvl = w.get_log_level_filter();
            let q = w.get_log_filter().to_string();
            let filtered = apply_log_filters(&logs, lvl, &q);
            let slint_logs: Vec<slint::SharedString> = filtered.into_iter().map(Into::into).collect();
            w.set_log_lines(slint_logs.as_slice().into());
        });
    }
    {
        let w = window.as_weak();
        window.on_filter_logs_level(move |lvl| {
            let Some(w) = w.upgrade() else { return };
            let logs = fetch_journal_logs();
            let q = w.get_log_filter().to_string();
            let filtered = apply_log_filters(&logs, lvl, &q);
            let slint_logs: Vec<slint::SharedString> = filtered.into_iter().map(Into::into).collect();
            w.set_log_lines(slint_logs.as_slice().into());
        });
    }
    {
        let w = window.as_weak();
        window.on_filter_logs_text(move |q| {
            let Some(w) = w.upgrade() else { return };
            let logs = fetch_journal_logs();
            let lvl = w.get_log_level_filter();
            let filtered = apply_log_filters(&logs, lvl, &q.to_string());
            let slint_logs: Vec<slint::SharedString> = filtered.into_iter().map(Into::into).collect();
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

    // ── Cursor Lock & Edge Switching IPC ──────────────────────────────────────
    {
        let w = window.as_weak();
        let th = tray_handle.clone();
        window.on_toggle_cursor_lock(move || {
            let Some(w) = w.upgrade() else { return };
            let new_lock = !w.get_cursor_locked();
            w.set_cursor_locked(new_lock);
            send_ipc(ipc::GuiCommand::SetCursorLock { locked: new_lock });
            if let Ok(mut cfg) = config::load() {
                cfg.input.cursor_locked = new_lock;
                let _ = config::save(&cfg);
            }
            if let Some(ref handle) = th {
                handle.update(move |t| {
                    t.cursor_locked = new_lock;
                });
            }
            show_toast(&w, if new_lock {
                "Cursor locked to local screen"
            } else {
                "Cursor unlocked — edge switching enabled"
            }, false);
        });
    }

    // ── Peer Management (Ping, Unpair, Side/Position) ─────────────────────────
    {
        let w = window.as_weak();
        window.on_ping_peer(move |addr| {
            let Some(w) = w.upgrade() else { return };
            let addr_str = addr.to_string();

            // Instant UI feedback: set status to Pinging…
            let mut peers: Vec<PeerEntry> = w.get_peers().iter().collect();
            for p in &mut peers {
                if p.address == addr_str.as_str() || addr_str.contains(p.address.as_str()) {
                    p.ping_status = "Pinging…".into();
                }
            }
            w.set_peers(peers.as_slice().into());

            let w_async = w.as_weak();
            let target_addr = addr_str.clone();
            std::thread::spawn(move || {
                use std::net::ToSocketAddrs;
                let host_port = if target_addr.contains(':') {
                    target_addr.clone()
                } else {
                    format!("{target_addr}:24800")
                };

                let start = std::time::Instant::now();
                let ping_res = match host_port.to_socket_addrs() {
                    Ok(mut addrs) => {
                        if let Some(sock_addr) = addrs.next() {
                            match std::net::TcpStream::connect_timeout(&sock_addr, std::time::Duration::from_millis(1500)) {
                                Ok(_) => {
                                    let ms = start.elapsed().as_millis();
                                    format!("{ms}ms")
                                }
                                Err(_) => "Timeout".to_string(),
                            }
                        } else {
                            "Invalid".to_string()
                        }
                    }
                    Err(_) => "Unresolved".to_string(),
                };

                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = w_async.upgrade() {
                        let mut peers: Vec<PeerEntry> = w.get_peers().iter().collect();
                        for p in &mut peers {
                            if p.address == target_addr.as_str() || target_addr.contains(p.address.as_str()) {
                                p.ping_status = ping_res.clone().into();
                            }
                        }
                        w.set_peers(peers.as_slice().into());
                    }
                });
            });
        });
    }
    {
        let w = window.as_weak();
        window.on_forget_peer(move |addr| {
            let Some(w) = w.upgrade() else { return };
            let addr_str = addr.to_string();
            send_ipc(ipc::GuiCommand::ForgetPeer { address: addr_str.clone() });
            if let Ok(mut cfg) = config::load() {
                cfg.peers.retain(|p| p.address.as_deref() != Some(&addr_str) && !p.address.as_deref().is_some_and(|a| !a.is_empty() && addr_str.contains(a)));
                let _ = config::save(&cfg);
            }
            let mut peers: Vec<PeerEntry> = w.get_peers().iter().collect();
            peers.retain(|p| p.address != addr_str.as_str() && !addr_str.contains(p.address.as_str()));
            w.set_peers(peers.as_slice().into());
            show_toast(&w, &format!("Unpaired peer {addr_str}"), false);
        });
    }
    {
        let w = window.as_weak();
        window.on_update_peer_position(move |addr, pos| {
            let Some(w) = w.upgrade() else { return };
            let addr_str = addr.to_string();
            let pos_str = pos.to_string().to_lowercase();
            if let Ok(mut cfg) = config::load() {
                let mut found = false;
                for p in &mut cfg.peers {
                    if p.address.as_deref() == Some(&addr_str) || (p.address.is_some() && addr_str.contains(p.address.as_ref().unwrap())) {
                        p.position = pos_str.clone();
                        found = true;
                    }
                }
                if !found {
                    cfg.peers.push(config::PeerConfig::new(
                        format!("peer-{}", addr_str.replace(':', "_")),
                        Some(addr_str.clone()),
                        pos_str.clone(),
                    ));
                }
                let _ = config::save(&cfg);
            }
            send_ipc(ipc::GuiCommand::SyncTopology {
                address: addr_str.clone(),
                position: pos_str.clone(),
            });
            let mut peers: Vec<PeerEntry> = w.get_peers().iter().collect();
            for p in &mut peers {
                if p.address == addr_str.as_str() || addr_str.contains(p.address.as_str()) {
                    p.position = pos_str.clone().into();
                }
            }
            w.set_peers(peers.as_slice().into());
            w.set_setting_peer_pos(pos_str.clone().into());
            show_toast(&w, &format!("Position updated: peer placed on the {pos_str}"), false);
            w.set_topology_configured(true);
            let pos_disp = match pos_str.as_str() {
                "left" => "Left",
                "above" => "Above (Top)",
                "below" => "Below (Bottom)",
                _ => "Right",
            };
            w.set_topology_notice(format!("Peer screen arranged on your {pos_disp}.").into());
        });
    }

    // ── Connect & Disconnect IPC ─────────────────────────────────────────────
    {
        let w = window.as_weak();
        let th = tray_handle.clone();
        window.on_connect_requested(move |addr| {
            send_ipc(ipc::GuiCommand::Connect { address: addr.to_string() });
            if let Some(ref handle) = th {
                let addr_s = addr.to_string();
                handle.update(move |t| {
                    t.status_text = format!("Connecting to {addr_s}…");
                    t.connected_to = Some(addr_s);
                });
            }
            if let Some(w) = w.upgrade() {
                w.set_connection_error("".into());
                w.set_topology_configured(false);
                w.set_topology_notice("".into());
                w.set_status(format!("Connecting to {addr}…").into());
            }
        });
    }
    {
        let w = window.as_weak();
        let th = tray_handle.clone();
        window.on_disconnect_requested(move || {
            send_ipc(ipc::GuiCommand::Disconnect);
            if let Some(ref handle) = th {
                handle.update(|t| {
                    t.status_text = "Disconnected".into();
                    t.connected_to = None;
                    t.is_forwarding = false;
                });
            }
            if let Some(w) = w.upgrade() {
                w.set_connected_peer("".into());
                w.set_topology_configured(false);
                w.set_topology_notice("".into());
                w.set_status("Disconnected".into());
            }
        });
    }
    {
        let w = window.as_weak();
        window.on_dismiss_error(move || {
            if let Some(w) = w.upgrade() {
                w.set_connection_error("".into());
            }
        });
    }
    window.on_toggle_discovery(move |enabled| {
        send_ipc(ipc::GuiCommand::SetDiscovery { enabled });
    });
    {
        let w = window.as_weak();
        window.on_toggle_file_transfer(move |enabled| {
            send_ipc(ipc::GuiCommand::SetFileTransfer { enabled });
            if let Ok(mut cfg) = config::load() {
                cfg.clipboard.files_enabled = enabled;
                let _ = config::save(&cfg);
            }
            if let Some(w) = w.upgrade() {
                w.set_setting_file_transfer(enabled);
            }
        });
    }
    {
        let w = window.as_weak();
        window.on_toggle_tls(move |enabled| {
            send_ipc(ipc::GuiCommand::SetTls { enabled });
            if let Ok(mut cfg) = config::load() {
                cfg.network.tls = enabled;
                let _ = config::save(&cfg);
            }
            if let Some(w) = w.upgrade() {
                w.set_setting_tls(enabled);
            }
        });
    }

    // ── Background Polling Timer (Every 2s) ──────────────────────────────────
    let _poll_timer = {
        let w = window.as_weak();
        let th = tray_handle.clone();
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
                    if let Some(ref handle) = th {
                        handle.update(|t| {
                            t.status_text = "Service Stopped".into();
                            t.is_service_running = false;
                            t.is_forwarding = false;
                            t.connected_to = None;
                        });
                    }
                    return;
                }

                // Periodic status polling via IPC
                if let Some(ipc::AgentEvent::Status {
                    local_name, connected_to, discovery, file_transfer_enabled, tls_enabled, tls_active, peers, cursor_locked, last_error, topology_configured, active_transfers, transfer_history, ..
                }) = poll_status()
                {
                    if let Some(ref handle) = th {
                        let status_str = match &connected_to {
                            Some(p) => format!("Forwarding → {p}"),
                            None    => "Ready".into(),
                        };
                        let is_fwd = connected_to.is_some();
                        let locked = cursor_locked;
                        let conn = connected_to.clone();
                        let p_list = peers.clone();
                        handle.update(move |t| {
                            t.status_text = status_str;
                            t.is_service_running = true;
                            t.is_forwarding = is_fwd;
                            t.cursor_locked = locked;
                            t.connected_to = conn;
                            t.peers = p_list;
                        });
                    }

                    w.set_local_name(local_name.into());
                    w.set_discovery(discovery);
                    w.set_setting_file_transfer(file_transfer_enabled);
                    w.set_setting_tls(tls_enabled);
                    w.set_tls_active(tls_active);
                    w.set_cursor_locked(cursor_locked);
                    w.set_status(match &connected_to {
                        Some(p) => format!("Forwarding → {p}").into(),
                        None    => "Ready".into(),
                    });

                    // File transfer updates
                    let transfers: Vec<TransferItem> = active_transfers.iter().map(|t| {
                        let pct = if t.total_bytes > 0 {
                            (t.bytes_transferred as f32 / t.total_bytes as f32).clamp(0.0, 1.0)
                        } else {
                            0.0
                        };
                        TransferItem {
                            name: t.filename.clone().into(),
                            size_text: format!("{}/{}", format_bytes_ui(t.bytes_transferred), format_bytes_ui(t.total_bytes)).into(),
                            progress: pct,
                            status_text: format!("{:.0}%", pct * 100.0).into(),
                            is_receiving: t.is_receiving,
                        }
                    }).collect();
                    w.set_active_transfers(transfers.as_slice().into());

                    let history: Vec<HistoryItem> = transfer_history.iter().map(|h| {
                        HistoryItem {
                            name: h.filename.clone().into(),
                            size_text: format_bytes_ui(h.total_bytes).into(),
                            time_text: h.completed_at.clone().into(),
                            is_receiving: h.is_receiving,
                        }
                    }).collect();
                    w.set_transfer_history(history.as_slice().into());

                    // Connection error update
                    if let Some(err) = last_error {
                        w.set_connection_error(err.into());
                    }


                    // Connected peer detection
                    let active_peer_info = peers.iter().find(|p| p.connected);
                    let is_connected = connected_to.is_some() || active_peer_info.is_some();

                    if let Some(ref p) = connected_to {
                        let peer_name = peers.iter()
                            .find(|item| item.address == *p || p.contains(&item.address))
                            .map(|item| item.name.clone())
                            .unwrap_or_else(|| p.clone());
                        w.set_connected_peer(format!("{peer_name} ({p})").into());
                        w.set_connection_error("".into());
                    } else if let Some(p) = active_peer_info {
                        w.set_connected_peer(format!("{} ({})", p.name, p.address).into());
                        w.set_connection_error("".into());
                    } else {
                        w.set_connected_peer("".into());
                    }

                    // Topology synchronization state
                    if !is_connected {
                        w.set_topology_configured(false);
                        w.set_topology_notice("".into());
                    } else {
                        let active_pos = peers.iter()
                            .find(|p| p.connected || connected_to.as_deref() == Some(&p.address))
                            .map(|p| p.position.clone())
                            .unwrap_or_else(|| w.get_setting_peer_pos().to_string());
                        let clean_pos = active_pos.to_lowercase();

                        if topology_configured {
                            let prev_configured = w.get_topology_configured();
                            w.set_topology_configured(true);
                            if !clean_pos.is_empty() && w.get_setting_peer_pos().to_string() != clean_pos {
                                w.set_setting_peer_pos(clean_pos.clone().into());
                            }
                            if !prev_configured || w.get_topology_notice().is_empty() {
                                let pos_display = match clean_pos.as_str() {
                                    "left" => "Left",
                                    "above" => "Above (Top)",
                                    "below" => "Below (Bottom)",
                                    _ => "Right",
                                };
                                let peer_name = peers.iter()
                                    .find(|p| p.connected || connected_to.as_deref() == Some(&p.address))
                                    .map(|p| p.name.clone())
                                    .unwrap_or_else(|| "peer".to_string());
                                w.set_topology_notice(format!("Screen arranged on your {pos_display} (synchronized with {peer_name}).").into());
                            }
                        } else if !clean_pos.is_empty() && w.get_setting_peer_pos().to_string() != clean_pos {
                            w.set_setting_peer_pos(clean_pos.into());
                        }
                    }

                    let current_peers: Vec<PeerEntry> = w.get_peers().iter().collect();
                    let default_pos = w.get_setting_peer_pos().to_string();
                    let entries: Vec<PeerEntry> = peers.iter().map(|p| {
                        let existing_ping = current_peers.iter()
                            .find(|cp| cp.address == p.address.as_str())
                            .map(|cp| cp.ping_status.clone())
                            .unwrap_or_default();
                        PeerEntry {
                            name:        p.name.clone().into(),
                            address:     p.address.clone().into(),
                            paired:      p.paired,
                            connected:   p.connected,
                            position:    if !p.position.is_empty() { p.position.clone().into() } else { default_pos.clone().into() },
                            ping_status: existing_ping,
                            grid_x:      p.grid_x,
                            grid_y:      p.grid_y,
                        }
                    }).collect();
                    w.set_peers(entries.as_slice().into());
                }

                // If currently on the logs tab, auto-refresh logs with active filters
                if w.get_active_tab() == 3 {
                    let logs = fetch_journal_logs();
                    let lvl = w.get_log_level_filter();
                    let q = w.get_log_filter().to_string();
                    let filtered = apply_log_filters(&logs, lvl, &q);
                    let slint_logs: Vec<slint::SharedString> = filtered.into_iter().map(Into::into).collect();
                    w.set_log_lines(slint_logs.as_slice().into());
                }
            },
        );
        timer
    };

    window.run()?;
    if let Some(th) = tray_handle {
        th.shutdown().wait();
    }
    Ok(())
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn show_toast(w: &MainWindow, msg: &str, is_error: bool) {
    w.set_toast_text(msg.into());
    w.set_toast_is_error(is_error);
    w.set_toast_visible(true);
    let w_weak = w.as_weak();
    slint::Timer::single_shot(std::time::Duration::from_millis(3500), move || {
        if let Some(w) = w_weak.upgrade() {
            w.set_toast_visible(false);
        }
    });
}

fn populate_settings_from_config(w: &MainWindow, cfg: &config::Config) {
    let is_dark = cfg.theme.as_str() != "light";
    w.set_is_dark(is_dark);
    w.set_local_ip(detect_local_ip().into());
    w.set_local_name(cfg.device.name.clone().into());
    w.set_setting_name(cfg.device.name.clone().into());
    w.set_setting_port(cfg.network.port.to_string().into());
    w.set_setting_discovery(cfg.network.discovery);
    w.set_setting_clipboard(cfg.clipboard.enabled);
    w.set_setting_file_transfer(cfg.clipboard.files_enabled);
    w.set_setting_tls(cfg.network.tls);
    w.set_setting_autostart(is_autostart_enabled());
    w.set_setting_width(cfg.screen.width.to_string().into());
    w.set_setting_height(cfg.screen.height.to_string().into());
    w.set_setting_switch_delay(cfg.input.switch_delay_ms.to_string().into());
    w.set_setting_deadzone(cfg.input.corner_deadzone_px.to_string().into());
    w.set_setting_velocity(cfg.input.edge_velocity_threshold.to_string().into());
    w.set_cursor_locked(cfg.input.cursor_locked);
    w.set_setting_hotkey_lock(cfg.hotkeys.toggle_cursor_lock.clone().into());
    w.set_setting_hotkey_switch(cfg.hotkeys.switch_screen.clone().into());
    w.set_setting_hotkey_escape(cfg.hotkeys.emergency_escape.clone().into());

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

fn is_autostart_enabled() -> bool {
    let out = Command::new("systemctl")
        .args(["--user", "is-enabled", "manguesechee-agent"])
        .output();
    if let Ok(o) = out {
        String::from_utf8_lossy(&o.stdout).trim() == "enabled"
    } else {
        false
    }
}

fn apply_log_filters(raw_logs: &[String], level: i32, query: &str) -> Vec<String> {
    let q_lower = query.trim().to_lowercase();
    raw_logs
        .iter()
        .filter(|line| {
            let l_upper = line.to_uppercase();
            let matches_level = match level {
                1 => l_upper.contains("ERROR") || l_upper.contains("ERR") || l_upper.contains("FAILED") || l_upper.contains("PANICKED"),
                2 => l_upper.contains("WARN"),
                3 => l_upper.contains("INFO"),
                _ => true,
            };
            if !matches_level {
                return false;
            }
            if q_lower.is_empty() {
                return true;
            }
            line.to_lowercase().contains(&q_lower)
        })
        .cloned()
        .collect()
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

static CLIPBOARD: std::sync::Mutex<Option<arboard::Clipboard>> = std::sync::Mutex::new(None);

fn copy_to_clipboard(text: &str) -> bool {
    let mut copied = false;

    // 1. Native cross-platform via arboard with Wayland data-control support
    // Keeping CLIPBOARD alive in static Mutex ensures Wayland data source thread
    // stays alive to serve paste requests when user switches windows.
    if let Ok(mut guard) = CLIPBOARD.lock() {
        if guard.is_none() {
            *guard = arboard::Clipboard::new().ok();
        }
        if let Some(board) = guard.as_mut() {
            if board.set_text(text).is_ok() {
                copied = true;
            } else {
                // Reconnect clipboard if compositor connection dropped
                *guard = arboard::Clipboard::new().ok();
                if let Some(board) = guard.as_mut() {
                    if board.set_text(text).is_ok() {
                        copied = true;
                    }
                }
            }
        }
    }

    // 2. Wayland native fallback via wl-copy (standard on COSMIC & wlroots)
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
            let _ = child.wait();
            copied = true;
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
            let _ = child.wait();
            copied = true;
        }
    }

    // 4. Fallback via xsel
    if let Ok(mut child) = Command::new("xsel")
        .args(["--clipboard", "--input"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            let _ = stdin.write_all(text.as_bytes());
            drop(stdin);
            let _ = child.wait();
            copied = true;
        }
    }

    copied
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

fn format_bytes_ui(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

