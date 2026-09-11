//! Inbound connection handler — controlled peer side.
//! Phases 1-7: identity, pairing, input injection, clipboard sync.

use manguesechee_core::events::InputEvent;
use manguesechee_core::protocol::Message;
use manguesechee_input::{KeyboardInjector, MouseInjector};
use manguesechee_network::{transport::Transport, wrap};
use std::sync::Arc;
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
    port:          u16,
    ipc_state:     crate::ipc_server::SharedState,
    connect_tx:    tokio::sync::mpsc::UnboundedSender<String>,
    broadcast_tx:  tokio::sync::broadcast::Sender<Message>,
) {
    if let Ok(addr) = listener.local_addr() {
        info!("listening on {addr}");
    }
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
                    if let Err(e) = handle(stream, peer_addr, name, id, screen_width, screen_height, port, state, tx, b_tx).await {
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
    port:          u16,
    ipc_state:     crate::ipc_server::SharedState,
    connect_tx:    tokio::sync::mpsc::UnboundedSender<String>,
    broadcast_tx:  tokio::sync::broadcast::Sender<Message>,
) -> anyhow::Result<()> {
    let mut transport = wrap(stream);

    // ── Secondary data channel or control identity ────────────────────────────
    let first_msg = transport.receive().await?;
    let (_peer_name, peer_id) = match first_msg {
        Message::FileChannelInit { transfer_id } => {
            info!("incoming dedicated file channel from {peer_addr} (transfer {transfer_id})");
            return crate::file_transfer::handle_incoming_file_channel(transport, transfer_id, ipc_state).await;
        }
        Message::Identity { name, id } => {
            info!("peer: name={name} id={id}");
            (name, id)
        }
        other => anyhow::bail!("expected Identity or FileChannelInit, got {other:?}"),
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
    let peer_target_addr = format!("{peer_ip}:{port}");

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
                let _ = connect_tx.send(peer_target_addr.clone());
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
    // peer_target_addr already declared above; clone it for the send task
    let peer_target_addr_send = peer_target_addr.clone();
    let ipc_state_for_send = Arc::clone(&ipc_state);
    tokio::spawn(async move {
        loop {
            tokio::select! {
                Some(msg) = out_rx.recv() => {
                    match msg {
                        Message::ClipboardSync { text } => {
                            if let Some(paths) = crate::file_clipboard::parse_clipboard_file_uris(&text) {
                                let (files, disk_paths, total_size) = crate::file_clipboard::collect_file_entries(&paths);
                                let cfg_clip = manguesechee_core::config::load().map(|c| c.clipboard).unwrap_or_default();
                                let fast_limit = (cfg_clip.fast_limit_mb as u64) * 1024 * 1024;
                                let bg_limit = (cfg_clip.background_limit_mb as u64) * 1024 * 1024;

                                if total_size <= fast_limit {
                                    let tid = uuid::Uuid::new_v4().to_string();
                                    if let Err(e) = crate::file_transfer::send_fast_transfer(tid, files, disk_paths, total_size, &mut sender, &ipc_state_for_send).await {
                                        warn!("send fast file transfer failed: {e}");
                                    }
                                } else if total_size <= bg_limit {
                                    crate::file_transfer::spawn_background_sender(peer_target_addr_send.clone(), files, disk_paths, total_size, Arc::clone(&ipc_state_for_send));
                                }
                            } else {
                                if let Err(e) = sender.send(&Message::ClipboardSync { text }).await { warn!("send: {e}"); break; }
                            }
                        }
                        other => {
                            if let Err(e) = sender.send(&other).await { warn!("send: {e}"); break; }
                        }
                    }
                }
                Ok(msg) = broadcast_rx.recv() => {
                    if let Err(e) = sender.send(&msg).await { warn!("broadcast send: {e}"); break; }
                }
            }
        }
    });

    let mut file_receiver = crate::file_transfer::FileReceiver::default();

    // Closure to process any message
    let handle_msg = |msg: Message,
                          edge: &mut EdgeDetector,
                          mouse: &mut MouseInjector,
                          keyboard: &mut KeyboardInjector,
                          has_control: &mut bool,
                          file_receiver: &mut crate::file_transfer::FileReceiver,
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
                        let _ = mouse.release_all();
                        let _ = keyboard.release_all();
                        // Immediate clipboard sync on returning control to controller
                        if clipboard_enabled {
                            if let Some(text) = clipboard::get_text() {
                                if !text.is_empty() && !clipboard::is_already_synced(&text) {
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

            Message::FileTransferOffer { transfer_id, files, total_size, is_background } => {
                file_receiver.handle_offer(transfer_id, files, total_size, is_background, &ipc_state);
            }

            Message::FileTransferChunk { transfer_id, file_index, offset, data, is_last_chunk: _ } => {
                file_receiver.handle_chunk(&transfer_id, file_index, offset, &data, &ipc_state);
            }

            Message::FileTransferDone { transfer_id } => {
                file_receiver.handle_done(&transfer_id, &ipc_state);
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
        if !handle_msg(msg, &mut edge, &mut mouse, &mut keyboard, &mut has_control, &mut file_receiver, &out_tx)? {
            return Ok(());
        }
    }

    // ── Event loop ────────────────────────────────────────────────────────────
    loop {
        let msg = if has_control {
            // When master has control of our screen, if master dies or disconnects abruptly,
            // we must not hang waiting indefinitely for input events. Timeout after 5s.
            match tokio::time::timeout(std::time::Duration::from_secs(5), receiver.receive()).await {
                Ok(Ok(m)) => m,
                Ok(Err(e)) => {
                    info!("peer connection closed: {e:#}");
                    break;
                }
                Err(_) => {
                    warn!("timed out waiting for peer with active control — releasing virtual devices");
                    let _ = mouse.release_all();
                    let _ = keyboard.release_all();
                    break;
                }
            }
        } else {
            match receiver.receive().await {
                Ok(m) => m,
                Err(e) => {
                    info!("peer connection closed: {e:#}");
                    break;
                }
            }
        };
        if !handle_msg(msg, &mut edge, &mut mouse, &mut keyboard, &mut has_control, &mut file_receiver, &out_tx)? {
            break;
        }
    }
    let _ = mouse.release_all();
    let _ = keyboard.release_all();
    // Allow compositor and kernel time to flush key-up and button-up events before device destruction
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    Ok(())
}
