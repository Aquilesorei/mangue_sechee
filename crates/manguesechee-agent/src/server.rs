//! Inbound connection handler — controlled peer side.
//! Phases 1-7: identity, pairing, input injection, clipboard sync.

use manguesechee_core::events::InputEvent;
use manguesechee_core::protocol::Message;
use manguesechee_input::{KeyboardInjector, MouseInjector};
use manguesechee_network::{transport::Transport, wrap};
use tokio::net::TcpListener;
use tracing::{error, info, warn};

use crate::clipboard;
use crate::edge::EdgeDetector;
use crate::session::{load_known_peers, prompt_accept, save_known_peers};

pub async fn run(
    listener:      TcpListener,
    local_name:    String,
    local_id:      String,
    screen_width:  u32,
    screen_height: u32,
    ipc_state:     crate::ipc_server::SharedState,
    connect_tx:    tokio::sync::mpsc::UnboundedSender<String>,
    broadcast_tx:  tokio::sync::broadcast::Sender<Message>,
) {
    info!("listening on {}", listener.local_addr().unwrap());
    loop {
        match listener.accept().await {
            Ok((stream, peer_addr)) => {
                info!("incoming from {peer_addr}");
                let name = local_name.clone();
                let id   = local_id.clone();
                let state = std::sync::Arc::clone(&ipc_state);
                let tx = connect_tx.clone();
                let b_tx = broadcast_tx.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle(stream, peer_addr, name, id, screen_width, screen_height, state, tx, b_tx).await {
                        error!("session error from {peer_addr}: {e:#}");
                    }
                });
            }
            Err(e) => warn!("accept error: {e}"),
        }
    }
}

async fn handle(
    stream:        tokio::net::TcpStream,
    peer_addr:     std::net::SocketAddr,
    local_name:    String,
    local_id:      String,
    screen_width:  u32,
    screen_height: u32,
    ipc_state:     crate::ipc_server::SharedState,
    connect_tx:    tokio::sync::mpsc::UnboundedSender<String>,
    broadcast_tx:  tokio::sync::broadcast::Sender<Message>,
) -> anyhow::Result<()> {
    let mut transport = wrap(stream);

    // ── Identity ──────────────────────────────────────────────────────────────
    let (_peer_name, peer_id) = match transport.receive().await? {
        Message::Identity { name, id } => { info!("peer: name={name} id={id}"); (name, id) }
        other => anyhow::bail!("expected Identity, got {other:?}"),
    };
    transport.send(&Message::Identity { name: local_name.clone(), id: local_id.clone() }).await?;

    // ── Pairing ───────────────────────────────────────────────────────────────
    let mut known = load_known_peers().unwrap_or_default();
    let mut initial_msg: Option<Message> = None;

    if !known.contains(&peer_id) {
        let first_msg = transport.receive().await?;
        match first_msg {
            Message::PairRequest { name, id, code } => {
                let accepted = tokio::task::spawn_blocking({
                    let name = name.clone(); let code = code.clone();
                    move || prompt_accept(&name, &code)
                }).await.unwrap_or(false);

                if accepted {
                    transport.send(&Message::PairAccepted {
                        name: local_name.clone(), id: local_id.clone(), code,
                    }).await?;
                    known.add(id, name.clone());
                    let _ = save_known_peers(&known);
                    info!("paired with '{name}'");
                } else {
                    transport.send(&Message::PairRejected { reason: "user rejected".into() }).await?;
                    anyhow::bail!("pairing rejected");
                }
            }
            // Peer already considers us paired (e.g. client reconnected or configured on peer side)
            active_msg @ (Message::InputEvent(_) | Message::EdgeCrossed { .. } | Message::Ping | Message::ClipboardSync { .. }) => {
                info!("peer '{_peer_name}' ({peer_id}) already paired from remote; auto-trusting peer");
                known.add(peer_id.clone(), _peer_name.clone());
                let _ = save_known_peers(&known);
                initial_msg = Some(active_msg);
            }
            other => anyhow::bail!("expected PairRequest or session message, got {other:?}"),
        }
    } else {
        info!("peer already known — skipping pairing");
    }

    // ── Auto-register peer & auto-connect bidirectional controller ───────────
    let peer_ip = peer_addr.ip().to_string();
    let peer_target_addr = format!("{peer_ip}:24800");

    let existing_pos = {
        let mut s = ipc_state.lock().unwrap();
        if let Some(existing) = s.peers.iter_mut().find(|p| p.address.contains(&peer_ip) || p.name == _peer_name) {
            existing.paired = true;
            if !existing.address.contains(':') {
                existing.address = peer_target_addr.clone();
            }
            existing.position.clone()
        } else {
            let pos = manguesechee_core::config::load().ok().and_then(|cfg| {
                cfg.peers.iter().find(|p| p.address.as_deref().unwrap_or("").contains(&peer_ip) || p.id == peer_id)
                    .map(|p| p.position.clone())
            }).unwrap_or_else(|| "right".to_string());

            s.peers.push(manguesechee_core::ipc::PeerInfo {
                name: _peer_name.clone(),
                address: peer_target_addr.clone(),
                paired: true,
                connected: false,
                position: pos.clone(),
            });
            pos
        }
    };

    if let Ok(mut cfg) = manguesechee_core::config::load() {
        if !cfg.peers.iter().any(|p| p.address.as_deref().unwrap_or("").contains(&peer_ip)) {
            cfg.peers.push(manguesechee_core::config::PeerConfig {
                id: peer_id.clone(),
                address: Some(peer_target_addr.clone()),
                position: existing_pos,
            });
            let _ = manguesechee_core::config::save(&cfg);
        }

        // If local machine is configured for bidirectional operation (input enabled)
        // and not already controlling a peer, auto-connect back to this peer!
        if cfg.input.enabled {
            let already_connected = ipc_state.lock().unwrap().connected_to.is_some();
            if !already_connected {
                info!("Bidirectional auto-connect: initiating reverse controller connection to {peer_target_addr}");
                let _ = connect_tx.send(peer_target_addr);
            }
        }
    }

    // ── Split + virtual devices ───────────────────────────────────────────────
    let (mut sender, mut receiver) = transport.into_split();
    let mut mouse = match MouseInjector::new() {
        Ok(m) => m,
        Err(e) => {
            error!("failed to create virtual mouse: {e:#}");
            anyhow::bail!("Virtual mouse creation failed: {e}. Check /dev/uinput permissions.");
        }
    };
    let mut keyboard = match KeyboardInjector::new() {
        Ok(k) => k,
        Err(e) => {
            error!("failed to create virtual keyboard: {e:#}");
            anyhow::bail!("Virtual keyboard creation failed: {e}. Check /dev/uinput permissions.");
        }
    };
    info!("virtual mouse + keyboard ready");

    let mut edge = EdgeDetector::new(screen_width, screen_height);
    let mut has_control = false;

    // Outbound channel — ReturnControl, Pong, and broadcast messages go here
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<Message>(16);
    let clipboard_enabled = manguesechee_core::config::load()
        .map(|c| c.clipboard.enabled)
        .unwrap_or(true);
    if clipboard_enabled {
        clipboard::spawn_watcher(out_tx.clone());
    }

    let mut broadcast_rx = broadcast_tx.subscribe();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                Some(msg) = out_rx.recv() => {
                    if let Err(e) = sender.send(&msg).await { warn!("send: {e}"); break; }
                }
                Ok(msg) = broadcast_rx.recv() => {
                    if let Err(e) = sender.send(&msg).await { warn!("broadcast send: {e}"); break; }
                }
            }
        }
    });

    // Closure to process any message
    let handle_msg = |msg: Message,
                      edge: &mut EdgeDetector,
                      mouse: &mut MouseInjector,
                      keyboard: &mut KeyboardInjector,
                      has_control: &mut bool,
                      out_tx: &tokio::sync::mpsc::Sender<Message>| -> anyhow::Result<bool> {
        match msg {
            Message::EdgeCrossed { edge: entry } => {
                let return_edge = entry.opposite();
                edge.set_allowed_edge(Some(return_edge));
                edge.place_at_entry(&return_edge);
                *has_control = true;
                info!("cursor entered from {return_edge:?} (controller exited {entry:?}) — return edge restricted to {return_edge:?}");
            }

            Message::InputEvent(event) => {
                if !*has_control {
                    // Residual in-flight events after returning control — ignore
                    return Ok(true);
                }

                if let InputEvent::MouseMove { dx, dy } = &event {
                    if let Some(exit) = edge.update(*dx, *dy) {
                        *has_control = false;
                        info!("cursor left via {exit:?} — ReturnControl");
                        // Immediate clipboard sync on returning control to controller
                        if clipboard_enabled {
                            if let Some(text) = clipboard::get_text() {
                                if !text.is_empty() && !clipboard::is_already_synced(&text) {
                                    info!("→ syncing clipboard on ReturnControl ({} bytes)", text.len());
                                    clipboard::mark_synced(&text);
                                    let _ = out_tx.try_send(Message::ClipboardSync { text });
                                }
                            }
                        }
                        let _ = out_tx.try_send(Message::ReturnControl { edge: exit });
                        return Ok(true);
                    }
                }
                match &event {
                    InputEvent::MouseMove { .. }
                    | InputEvent::MouseButton { .. }
                    | InputEvent::MouseScroll { .. } => mouse.inject(&event)?,
                    InputEvent::Key { .. }
                    | InputEvent::KeySync { .. }     => keyboard.inject(&event)?,
                }
            }

            Message::ClipboardSync { text } => {
                info!("← ClipboardSync ({} bytes)", text.len());
                if clipboard_enabled {
                    if let Err(e) = clipboard::set_text(&text) {
                        warn!("clipboard set failed: {e}");
                    }
                }
            }

            Message::TopologySync { position } => {
                let opp = manguesechee_core::protocol::opposite_position(&position).to_string();
                info!("received TopologySync: peer set our position to {position} -> setting peer to {opp}");
                {
                    let mut s = ipc_state.lock().unwrap();
                    s.topology_configured = true;
                    let single_peer = s.peers.len() == 1;
                    for p in &mut s.peers {
                        if single_peer || p.address.contains(&peer_ip) || p.name == _peer_name {
                            p.position = opp.clone();
                        }
                    }
                }
                if let Ok(mut cfg) = manguesechee_core::config::load() {
                    let single_peer = cfg.peers.len() == 1;
                    for p in &mut cfg.peers {
                        if single_peer || p.address.as_deref().unwrap_or("").contains(&peer_ip) || p.id == peer_id {
                            p.position = opp.clone();
                        }
                    }
                    let _ = manguesechee_core::config::save(&cfg);
                }
            }

            Message::PairRequest { name, id, code } => {
                info!("received PairRequest during session from {name} ({id}) — re-accepting");
                let _ = out_tx.try_send(Message::PairAccepted {
                    name: local_name.clone(), id: local_id.clone(), code,
                });
            }

            Message::Ping    => { let _ = out_tx.try_send(Message::Pong); }
            Message::Goodbye => { info!("peer disconnected"); return Ok(false); }
            other            => warn!("unexpected: {other:?}"),
        }
        Ok(true)
    };

    // Process initial message if captured during pairing resolution
    if let Some(msg) = initial_msg {
        if !handle_msg(msg, &mut edge, &mut mouse, &mut keyboard, &mut has_control, &out_tx)? {
            return Ok(());
        }
    }

    // ── Event loop ────────────────────────────────────────────────────────────
    loop {
        let msg = receiver.receive().await?;
        if !handle_msg(msg, &mut edge, &mut mouse, &mut keyboard, &mut has_control, &out_tx)? {
            break;
        }
    }
    Ok(())
}
