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
                        Ok(_) = grab_rx.changed() => {
                            let g = *grab_rx.borrow();
                            if let Err(e) = if g { cap.grab() } else { cap.ungrab() } {
                                warn!("mouse grab {}: {e}", path.display());
                            }
                        }
                        ev = cap.next_event() => match ev {
                            Ok(ev) => { let _ = tx.send(ev).await; }
                            Err(e) => { warn!("mouse read {}: {e}", path.display()); break; }
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
                        Ok(_) = grab_rx.changed() => {
                            let g = *grab_rx.borrow();
                            if let Err(e) = if g { cap.grab() } else { cap.ungrab() } {
                                warn!("keyboard grab {}: {e}", path.display());
                            }
                        }
                        ev = cap.next_event() => match ev {
                            Ok(ev) => { let _ = tx.send(ev).await; }
                            Err(e) => { warn!("keyboard read {}: {e}", path.display()); break; }
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
    let (return_tx, mut return_rx) = mpsc::channel::<()>(4);
    let state_for_recv = Arc::clone(&ipc_state);
    let peer_addr_str = addr.to_string();
    tokio::spawn(async move {
        loop {
            match receiver.receive().await {
                Ok(Message::ReturnControl { edge }) => {
                    info!("← ReturnControl ({edge:?})");
                    let _ = return_tx.send(()).await;
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
                Ok(Message::Goodbye) | Err(_) => break,
                Ok(other) => warn!("unexpected: {other:?}"),
            }
        }
    });

    // ── State machine ─────────────────────────────────────────────────────────
    let mut state = ControllerState::Local;
    let initial_locked = ipc_state.lock().unwrap().cursor_locked;
    let mut edge = EdgeDetector::new(screen_width, screen_height)
        .with_settings(deadzone_px, delay_ms, initial_locked);
    let mut broadcast_rx = broadcast_tx.subscribe();
    info!("ready — move cursor to screen edge to switch to peer (locked={initial_locked}, deadzone={deadzone_px}px, delay={delay_ms}ms)");

    loop {
        // Sync dynamic cursor lock from GUI / IPC
        {
            let is_locked = ipc_state.lock().unwrap().cursor_locked;
            edge.set_locked(is_locked);
        }
        tokio::select! {
            maybe_ev = ev_rx.recv() => {
                let Some(event) = maybe_ev else { break };
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
                                    sender.send(&Message::EdgeCrossed { edge: crossed }).await?;
                                    state = ControllerState::Forwarding;

                                    // Immediate clipboard sync on entering peer screen
                                    if clipboard_enabled {
                                        if let Some(text) = clipboard::get_text() {
                                            if !text.is_empty() && !clipboard::is_already_synced(&text) {
                                                info!("→ syncing clipboard on EdgeCrossed ({} bytes)", text.len());
                                                clipboard::mark_synced(&text);
                                                let _ = sender.send(&Message::ClipboardSync { text }).await;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    ControllerState::Forwarding => {
                        sender.send(&Message::InputEvent(event)).await?;
                    }
                }
            }

            // Outgoing clipboard change
            Some(clip_msg) = clip_msg_rx.recv() => {
                if let Err(e) = sender.send(&clip_msg).await {
                    warn!("failed to send clipboard sync: {e}");
                    break;
                }
            }

            // Broadcast messages (e.g. TopologySync) to peer
            Ok(bmsg) = broadcast_rx.recv() => {
                if let Err(e) = sender.send(&bmsg).await {
                    warn!("failed to send broadcast msg to peer: {e}");
                    break;
                }
            }

            // Peer returning control
            Some(_) = return_rx.recv() => {
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
    }

    sender.send(&Message::Goodbye).await?;
    Ok(())
}
