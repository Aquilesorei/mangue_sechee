//! Outbound connection — controller side.
//! Phases 1-7: identity, pairing, edge switching, clipboard sync.

use manguesechee_core::events::InputEvent;
use manguesechee_core::protocol::{Edge, Message};
use manguesechee_input::{find_all_keyboards, find_all_mice, KeyboardCapture, MouseCapture};
use manguesechee_network::{connect, Transport};
use std::path::PathBuf;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::clipboard;
use crate::edge::EdgeDetector;
use crate::session::{generate_code, load_known_peers, save_known_peers, show_outgoing_code};

use std::sync::Arc;

#[derive(Debug, PartialEq)]
enum ControllerState { Local, Forwarding }

#[derive(Debug)]
enum InboundSignal {
    ReturnControl,
    Pong,
    Disconnected(String),
}

struct PeerConnectedGuard {
    ipc_state: crate::ipc_server::SharedState,
    addr: String,
}

impl Drop for PeerConnectedGuard {
    fn drop(&mut self) {
        let mut s = self.ipc_state.lock().unwrap();
        for p in &mut s.peers {
            if p.address == self.addr || self.addr.contains(&p.address) {
                p.connected = false;
            }
        }
        if s.connected_to.as_deref() == Some(&self.addr) {
            s.connected_to = None;
            s.topology_configured = false;
        }
    }
}

pub async fn connect_to(
    addr:          &str,
    local_name:    String,
    local_id:      String,
    mouse_path:    Option<PathBuf>,
    keyboard_path: Option<PathBuf>,
    screen_width:  u32,
    screen_height: u32,
    deadzone_px:   u32,
    delay_ms:      u32,
    ipc_state:     crate::ipc_server::SharedState,
    broadcast_tx:  tokio::sync::broadcast::Sender<Message>,
) -> anyhow::Result<()> {
    info!("connecting to {addr}  (screen {screen_width}×{screen_height})");
    let mut transport = connect(addr).await?;

    // ── Identity ──────────────────────────────────────────────────────────────
    transport.send(&Message::Identity { name: local_name.clone(), id: local_id.clone() }).await?;
    let (peer_name, peer_id) = match transport.receive().await? {
        Message::Identity { name, id } => { info!("peer: name={name} id={id}"); (name, id) }
        other => anyhow::bail!("expected Identity, got {other:?}"),
    };

    // ── Pairing ───────────────────────────────────────────────────────────────
    let mut known = load_known_peers().unwrap_or_default();
    if !known.contains(&peer_id) {
        let code = generate_code();
        show_outgoing_code(&peer_name, &code);
        transport.send(&Message::PairRequest {
            name: local_name.clone(), id: local_id.clone(), code: code.clone(),
        }).await?;
        match transport.receive().await? {
            Message::PairAccepted { code: c, name, id } => {
                anyhow::ensure!(c == code, "pairing code mismatch");
                info!("paired with '{name}'");
                known.add(id, name);
                save_known_peers(&known)?;
            }
            Message::PairRejected { reason } => anyhow::bail!("pairing rejected: {reason}"),
            other => anyhow::bail!("expected PairAccepted/Rejected, got {other:?}"),
        }
    } else {
        info!("peer already known — skipping pairing");
    }

    // Mark peer connected in IPC state
    let existing_pos = {
        let mut s = ipc_state.lock().unwrap();
        s.connected_to = Some(addr.to_string());
        s.last_error = None;
        if let Some(p) = s.peers.iter_mut().find(|p| p.address == addr || addr.contains(&p.address) || p.address.contains(addr) || p.name == peer_name) {
            p.connected = true;
            p.paired = true;
            if !p.address.contains(':') {
                p.address = addr.to_string();
            }
            p.position.clone()
        } else {
            let pos = manguesechee_core::config::load().ok().and_then(|cfg| {
                cfg.peers.iter().find(|p| p.address.as_deref().unwrap_or("").contains(addr) || addr.contains(p.address.as_deref().unwrap_or("!@#$")) || p.id == peer_id)
                    .map(|p| p.position.clone())
            }).unwrap_or_else(|| "right".to_string());

            s.peers.push(manguesechee_core::ipc::PeerInfo {
                name: peer_name.clone(),
                address: addr.to_string(),
                paired: true,
                connected: true,
                position: pos.clone(),
            });
            pos
        }
    };
    let _conn_guard = PeerConnectedGuard {
        ipc_state: Arc::clone(&ipc_state),
        addr: addr.to_string(),
    };

    if let Ok(mut cfg) = manguesechee_core::config::load() {
        if !cfg.peers.iter().any(|p| p.address.as_deref().unwrap_or("").contains(addr) || addr.contains(p.address.as_deref().unwrap_or("!@#$"))) {
            cfg.peers.push(manguesechee_core::config::PeerConfig {
                id: peer_id.clone(),
                address: Some(addr.to_string()),
                position: existing_pos,
            });
            let _ = manguesechee_core::config::save(&cfg);
        }
    }

    let (mut sender, mut receiver) = transport.into_split();

    // ── Devices ───────────────────────────────────────────────────────────────
    let mouse_paths: Vec<PathBuf> = if let Some(p) = mouse_path {
        vec![p]
    } else {
        find_all_mice()
    };

    let keyboard_paths: Vec<PathBuf> = if let Some(p) = keyboard_path {
        vec![p]
    } else {
        find_all_keyboards()
    };

    if mouse_paths.is_empty() {
        warn!("no mouse devices detected in /dev/input");
    }
    if keyboard_paths.is_empty() {
        warn!("no keyboard devices detected in /dev/input");
    }

    let mut pressed = Vec::new();
    for p in &keyboard_paths {
        if let Ok(keys) = KeyboardCapture::snapshot_pressed(p) {
            pressed.extend(keys);
        }
    }
    pressed.sort();
    pressed.dedup();
    sender.send(&Message::InputEvent(InputEvent::KeySync { pressed_keys: pressed })).await?;

    // ── Capture tasks ─────────────────────────────────────────────────────────
    let (ev_tx, mut ev_rx) = mpsc::channel::<InputEvent>(512);
    let (grab_mouse_tx, grab_mouse_rx) = tokio::sync::watch::channel(false);
    let (grab_keyboard_tx, grab_keyboard_rx) = tokio::sync::watch::channel(false);

    for path in mouse_paths {
        let tx = ev_tx.clone();
        let mut grab_rx = grab_mouse_rx.clone();
        tokio::spawn(async move {
            match MouseCapture::open(&path) {
                Err(e) => warn!("mouse {}: {e:#}", path.display()),
                Ok(mut cap) => loop {
                    tokio::select! {
                        res = grab_rx.changed() => match res {
                            Ok(_) => {
                                let g = *grab_rx.borrow();
                                if let Err(e) = if g { cap.grab() } else { cap.ungrab() } {
                                    warn!("mouse grab {}: {e}", path.display());
                                }
                            }
                            Err(_) => {
                                let _ = cap.force_ungrab();
                                break;
                            }
                        },
                        ev = cap.next_event() => match ev {
                            Ok(ev) => {
                                if tx.send(ev).await.is_err() {
                                    let _ = cap.force_ungrab();
                                    break;
                                }
                            }
                            Err(e) => {
                                let _ = cap.force_ungrab();
                                warn!("mouse read {}: {e}", path.display());
                                break;
                            }
                        }
                    }
                },
            }
        });
    }

    for path in keyboard_paths {
        let tx = ev_tx.clone();
        let mut grab_rx = grab_keyboard_rx.clone();
        tokio::spawn(async move {
            match KeyboardCapture::open(&path) {
                Err(e) => warn!("keyboard {}: {e:#}", path.display()),
                Ok(mut cap) => loop {
                    tokio::select! {
                        res = grab_rx.changed() => match res {
                            Ok(_) => {
                                let g = *grab_rx.borrow();
                                if let Err(e) = if g { cap.grab() } else { cap.ungrab() } {
                                    warn!("keyboard grab {}: {e}", path.display());
                                }
                            }
                            Err(_) => {
                                let _ = cap.ungrab();
                                break;
                            }
                        },
                        ev = cap.next_event() => match ev {
                            Ok(ev) => {
                                if tx.send(ev).await.is_err() {
                                    let _ = cap.ungrab();
                                    break;
                                }
                            }
                            Err(e) => {
                                let _ = cap.ungrab();
                                warn!("keyboard read {}: {e}", path.display());
                                break;
                            }
                        }
                    }
                },
            }
        });
    }
    drop(ev_tx);

    // ── Clipboard watcher ─────────────────────────────────────────────────────
    // Outgoing clipboard messages share the main sender via a dedicated channel.
    let (clip_msg_tx, mut clip_msg_rx) = mpsc::channel::<Message>(16);
    let clipboard_enabled = manguesechee_core::config::load()
        .map(|c| c.clipboard.enabled)
        .unwrap_or(true);
    if clipboard_enabled {
        clipboard::spawn_watcher(clip_msg_tx);
    }

    // ── ReturnControl & Message listener ─────────────────────────────────────
    let (inbound_tx, mut inbound_rx) = mpsc::channel::<InboundSignal>(32);
    let state_for_recv = Arc::clone(&ipc_state);
    let peer_addr_str = addr.to_string();
    let grab_mouse_cleanup = grab_mouse_tx.clone();
    let grab_keyboard_cleanup = grab_keyboard_tx.clone();
    let inbound_tx_task = inbound_tx.clone();
    tokio::spawn(async move {
        let mut file_receiver = crate::file_transfer::FileReceiver::default();
        loop {
            match receiver.receive().await {
                Ok(Message::ReturnControl { edge }) => {
                    info!("← ReturnControl ({edge:?})");
                    let _ = inbound_tx_task.send(InboundSignal::ReturnControl).await;
                }
                Ok(Message::FileTransferOffer { transfer_id, files, total_size, is_background }) => {
                    file_receiver.handle_offer(transfer_id, files, total_size, is_background, &state_for_recv);
                }
                Ok(Message::FileTransferChunk { transfer_id, file_index, offset, data, is_last_chunk: _ }) => {
                    file_receiver.handle_chunk(&transfer_id, file_index, offset, &data, &state_for_recv);
                }
                Ok(Message::FileTransferDone { transfer_id }) => {
                    file_receiver.handle_done(&transfer_id, &state_for_recv);
                }
                Ok(Message::ClipboardSync { text }) => {
                    info!("← ClipboardSync from server ({} bytes)", text.len());
                    if clipboard_enabled {
                        if let Err(e) = clipboard::set_text(&text) {
                            warn!("failed to set local clipboard: {e}");
                        }
                    }
                }
                Ok(Message::TopologySync { position }) => {
                    let opp = manguesechee_core::protocol::opposite_position(&position).to_string();
                    info!("client: received TopologySync: remote set position to {position} -> setting local peer to {opp}");
                    {
                        let mut s = state_for_recv.lock().unwrap();
                        s.topology_configured = true;
                        let single_peer = s.peers.len() == 1;
                        for p in &mut s.peers {
                            if single_peer
                                || p.address == peer_addr_str
                                || peer_addr_str.contains(&p.address)
                                || p.address.contains(&peer_addr_str)
                            {
                                p.position = opp.clone();
                            }
                        }
                    }
                    if let Ok(mut cfg) = manguesechee_core::config::load() {
                        let single_peer = cfg.peers.len() == 1;
                        for p in &mut cfg.peers {
                            let p_addr = p.address.as_deref().unwrap_or("");
                            if single_peer
                                || p_addr == peer_addr_str
                                || peer_addr_str.contains(p_addr)
                                || (!p_addr.is_empty() && p_addr.contains(&peer_addr_str))
                            {
                                p.position = opp.clone();
                            }
                        }
                        let _ = manguesechee_core::config::save(&cfg);
                    }
                }
                Ok(Message::Pong) => {
                    let _ = inbound_tx_task.send(InboundSignal::Pong).await;
                }
                Ok(Message::Goodbye) => {
                    info!("← Goodbye received from peer {peer_addr_str}");
                    let _ = inbound_tx_task.send(InboundSignal::Disconnected("peer closed connection".into())).await;
                    break;
                }
                Err(e) => {
                    warn!("← receiver error from peer {peer_addr_str}: {e:#}");
                    let _ = inbound_tx_task.send(InboundSignal::Disconnected(format!("receiver error: {e}"))).await;
                    break;
                }
                Ok(other) => warn!("unexpected: {other:?}"),
            }
        }
        // Safety: ensure any grabs are released when receiver task finishes
        let _ = grab_mouse_cleanup.send(false);
        let _ = grab_keyboard_cleanup.send(false);
        let _ = inbound_tx_task.send(InboundSignal::Disconnected("receiver task finished".into())).await;
    });

    // ── State machine ─────────────────────────────────────────────────────────
    struct GrabGuard {
        grab_mouse_tx: tokio::sync::watch::Sender<bool>,
        grab_keyboard_tx: tokio::sync::watch::Sender<bool>,
    }
    impl Drop for GrabGuard {
        fn drop(&mut self) {
            let _ = self.grab_mouse_tx.send(false);
            let _ = self.grab_keyboard_tx.send(false);
        }
    }
    let _grab_guard = GrabGuard {
        grab_mouse_tx: grab_mouse_tx.clone(),
        grab_keyboard_tx: grab_keyboard_tx.clone(),
    };

    let mut state = ControllerState::Local;
    let initial_locked = ipc_state.lock().unwrap().cursor_locked;
    let mut edge = EdgeDetector::new(screen_width, screen_height)
        .with_settings(deadzone_px, delay_ms, initial_locked);
    let mut broadcast_rx = broadcast_tx.subscribe();
    let mut missed_pings: u32 = 0;
    let mut ping_interval = tokio::time::interval(std::time::Duration::from_secs(2));
    ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let mut lctrl_held = false;
    let mut rctrl_held = false;
    let mut lalt_held = false;
    let mut ralt_held = false;

    info!("ready — move cursor to screen edge to switch to peer (locked={initial_locked}, deadzone={deadzone_px}px, delay={delay_ms}ms)");

    loop {
        // Sync dynamic cursor lock from GUI / IPC
        {
            let is_locked = ipc_state.lock().unwrap().cursor_locked;
            edge.set_locked(is_locked);
        }
        tokio::select! {
            maybe_ev = ev_rx.recv() => {
                let Some(event) = maybe_ev else {
                    warn!("local input event stream ended — ungrabbing");
                    let _ = grab_mouse_tx.send(false);
                    let _ = grab_keyboard_tx.send(false);
                    break;
                };

                // Track modifier keys and emergency escape hotkey
                if let InputEvent::Key { key, pressed } = &event {
                    match key.0 {
                        29 => lctrl_held = *pressed,
                        97 => rctrl_held = *pressed,
                        56 => lalt_held = *pressed,
                        100 => ralt_held = *pressed,
                        _ => {}
                    }
                    if *pressed && state == ControllerState::Forwarding {
                        let ctrl_alt_esc = (lctrl_held || rctrl_held) && (lalt_held || ralt_held) && key.0 == 1;
                        let pause_or_scroll = key.0 == 119 || key.0 == 70;
                        if ctrl_alt_esc || pause_or_scroll {
                            info!("🚨 Emergency local escape hotkey triggered (key code {}) — ungrabbing immediately", key.0);
                            let _ = grab_mouse_tx.send(false);
                            let _ = grab_keyboard_tx.send(false);
                            state = ControllerState::Local;
                            let target_edge = {
                                let s = ipc_state.lock().unwrap();
                                let single_peer = s.peers.len() == 1;
                                s.peers.iter()
                                    .find(|p| single_peer || p.address == addr || addr.contains(&p.address) || p.address.contains(addr))
                                    .map(|p| match p.position.to_lowercase().as_str() {
                                        "left" => Edge::Left,
                                        "above" | "top" => Edge::Top,
                                        "below" | "bottom" => Edge::Bottom,
                                        _ => Edge::Right,
                                    })
                                    .unwrap_or(Edge::Right)
                            };
                            edge.set_allowed_edge(Some(target_edge));
                            edge.place_at_entry(&target_edge);
                            continue;
                        }
                    }
                }

                match state {
                    ControllerState::Local => {
                        if let InputEvent::MouseMove { dx, dy } = &event {
                            // Dynamic peer target edge resolution
                            let target_edge = {
                                let s = ipc_state.lock().unwrap();
                                let single_peer = s.peers.len() == 1;
                                s.peers.iter()
                                    .find(|p| single_peer || p.address == addr || addr.contains(&p.address) || p.address.contains(addr))
                                    .map(|p| match p.position.to_lowercase().as_str() {
                                        "left" => Edge::Left,
                                        "above" | "top" => Edge::Top,
                                        "below" | "bottom" => Edge::Bottom,
                                        _ => Edge::Right,
                                    })
                                    .unwrap_or(Edge::Right)
                            };
                            edge.set_allowed_edge(Some(target_edge));

                            if let Some(crossed) = edge.update(*dx, *dy) {
                                if crossed == target_edge {
                                    info!("→ EdgeCrossed ({crossed:?}) matching target {target_edge:?} — grabbing");
                                    let _ = grab_mouse_tx.send(true);
                                    let _ = grab_keyboard_tx.send(true);
                                    if let Err(e) = sender.send(&Message::EdgeCrossed { edge: crossed }).await {
                                        warn!("failed to send EdgeCrossed to {addr}: {e} — ungrabbing");
                                        let _ = grab_mouse_tx.send(false);
                                        let _ = grab_keyboard_tx.send(false);
                                        break;
                                    }
                                    state = ControllerState::Forwarding;

                                    // Immediate clipboard sync on entering peer screen
                                    if clipboard_enabled {
                                        if let Some(text) = clipboard::get_text() {
                                            if !text.is_empty() && !clipboard::is_already_synced(&text) {
                                                clipboard::mark_synced(&text);
                                                if let Some(paths) = crate::file_clipboard::parse_clipboard_file_uris(&text) {
                                                    let (files, disk_paths, total_size) = crate::file_clipboard::collect_file_entries(&paths);
                                                    let cfg_clip = manguesechee_core::config::load().map(|c| c.clipboard).unwrap_or_default();
                                                    let fast_limit = (cfg_clip.fast_limit_mb as u64) * 1024 * 1024;
                                                    let bg_limit = (cfg_clip.background_limit_mb as u64) * 1024 * 1024;

                                                    if total_size <= fast_limit {
                                                        let tid = uuid::Uuid::new_v4().to_string();
                                                        if let Err(e) = crate::file_transfer::send_fast_transfer(tid, files, disk_paths, total_size, &mut sender, &ipc_state).await {
                                                            warn!("failed to send fast file transfer on EdgeCrossed: {e}");
                                                        }
                                                    } else if total_size <= bg_limit {
                                                        crate::file_transfer::spawn_background_sender(addr.to_string(), files, disk_paths, total_size, Arc::clone(&ipc_state));
                                                    }
                                                } else {
                                                    info!("→ syncing clipboard on EdgeCrossed ({} bytes)", text.len());
                                                    let _ = sender.send(&Message::ClipboardSync { text }).await;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    ControllerState::Forwarding => {
                        let res = tokio::time::timeout(
                            std::time::Duration::from_millis(500),
                            sender.send(&Message::InputEvent(event)),
                        ).await;
                        match res {
                            Ok(Ok(())) => {}
                            Ok(Err(e)) => {
                                warn!("failed to send InputEvent to {addr}: {e} — ungrabbing");
                                let _ = grab_mouse_tx.send(false);
                                let _ = grab_keyboard_tx.send(false);
                                break;
                            }
                            Err(_) => {
                                warn!("timeout sending InputEvent to {addr} (peer likely offline) — ungrabbing");
                                let _ = grab_mouse_tx.send(false);
                                let _ = grab_keyboard_tx.send(false);
                                break;
                            }
                        }
                    }
                }
            }

            // Periodic heartbeat ping to keep connection alive and detect dropped peers early
            _ = ping_interval.tick() => {
                if missed_pings >= 2 {
                    warn!("peer {addr} missed 2 consecutive heartbeats — connection dead, ungrabbing and disconnecting");
                    let _ = grab_mouse_tx.send(false);
                    let _ = grab_keyboard_tx.send(false);
                    break;
                }
                missed_pings += 1;
                let ping_res = tokio::time::timeout(
                    std::time::Duration::from_millis(800),
                    sender.send(&Message::Ping),
                ).await;
                match ping_res {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        warn!("heartbeat ping to {addr} failed: {e} — ungrabbing and disconnecting");
                        let _ = grab_mouse_tx.send(false);
                        let _ = grab_keyboard_tx.send(false);
                        break;
                    }
                    Err(_) => {
                        warn!("heartbeat ping to {addr} timed out — ungrabbing and disconnecting");
                        let _ = grab_mouse_tx.send(false);
                        let _ = grab_keyboard_tx.send(false);
                        break;
                    }
                }
            }

            // Outgoing clipboard change
            Some(clip_msg) = clip_msg_rx.recv() => {
                match clip_msg {
                    Message::ClipboardSync { text } => {
                        if let Some(paths) = crate::file_clipboard::parse_clipboard_file_uris(&text) {
                            let (files, disk_paths, total_size) = crate::file_clipboard::collect_file_entries(&paths);
                            let cfg_clip = manguesechee_core::config::load().map(|c| c.clipboard).unwrap_or_default();
                            let fast_limit = (cfg_clip.fast_limit_mb as u64) * 1024 * 1024;
                            let bg_limit = (cfg_clip.background_limit_mb as u64) * 1024 * 1024;

                            if total_size <= fast_limit {
                                let tid = uuid::Uuid::new_v4().to_string();
                                if let Err(e) = crate::file_transfer::send_fast_transfer(tid, files, disk_paths, total_size, &mut sender, &ipc_state).await {
                                    warn!("failed to send fast file transfer: {e}");
                                }
                            } else if total_size <= bg_limit {
                                crate::file_transfer::spawn_background_sender(addr.to_string(), files, disk_paths, total_size, Arc::clone(&ipc_state));
                            }
                        } else {
                            if let Err(e) = sender.send(&Message::ClipboardSync { text }).await {
                                warn!("failed to send clipboard sync: {e}");
                                let _ = grab_mouse_tx.send(false);
                                let _ = grab_keyboard_tx.send(false);
                                break;
                            }
                        }
                    }
                    other => {
                        if let Err(e) = sender.send(&other).await {
                            warn!("failed to send clipboard msg: {e}");
                            let _ = grab_mouse_tx.send(false);
                            let _ = grab_keyboard_tx.send(false);
                            break;
                        }
                    }
                }
            }

            // Broadcast messages (e.g. TopologySync, Goodbye) to peer
            Ok(bmsg) = broadcast_rx.recv() => {
                if matches!(bmsg, Message::Goodbye) {
                    info!("broadcast Goodbye received — disconnecting from peer {addr}");
                    let _ = grab_mouse_tx.send(false);
                    let _ = grab_keyboard_tx.send(false);
                    let _ = sender.send(&Message::Goodbye).await;
                    break;
                }
                if let Err(e) = sender.send(&bmsg).await {
                    warn!("failed to send broadcast msg to peer: {e}");
                    let _ = grab_mouse_tx.send(false);
                    let _ = grab_keyboard_tx.send(false);
                    break;
                }
            }

            // Inbound signals from receiver task
            inbound = inbound_rx.recv() => {
                match inbound {
                    Some(InboundSignal::ReturnControl) => {
                        if state == ControllerState::Forwarding {
                            info!("← ReturnControl — ungrabbing, back to local");
                            let _ = grab_mouse_tx.send(false);
                            let _ = grab_keyboard_tx.send(false);
                            state = ControllerState::Local;
                            let target_edge = {
                                let s = ipc_state.lock().unwrap();
                                let single_peer = s.peers.len() == 1;
                                s.peers.iter()
                                    .find(|p| single_peer || p.address == addr || addr.contains(&p.address) || p.address.contains(addr))
                                    .map(|p| match p.position.to_lowercase().as_str() {
                                        "left" => Edge::Left,
                                        "above" | "top" => Edge::Top,
                                        "below" | "bottom" => Edge::Bottom,
                                        _ => Edge::Right,
                                    })
                                    .unwrap_or(Edge::Right)
                            };
                            edge.set_allowed_edge(Some(target_edge));
                            edge.place_at_entry(&target_edge);
                        }
                    }
                    Some(InboundSignal::Pong) => {
                        missed_pings = 0;
                    }
                    Some(InboundSignal::Disconnected(reason)) => {
                        warn!("peer {addr} disconnected ({reason}) — ungrabbing and exiting session");
                        let _ = grab_mouse_tx.send(false);
                        let _ = grab_keyboard_tx.send(false);
                        break;
                    }
                    None => {
                        warn!("inbound channel closed for {addr} — ungrabbing and exiting session");
                        let _ = grab_mouse_tx.send(false);
                        let _ = grab_keyboard_tx.send(false);
                        break;
                    }
                }
            }
        }
    }

    let _ = grab_mouse_tx.send(false);
    let _ = grab_keyboard_tx.send(false);
    let _ = sender.send(&Message::Goodbye).await;
    Ok(())
}
