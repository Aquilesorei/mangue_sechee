use manguesechee_core::events::{InputEvent, KeyCode};
use manguesechee_core::protocol::{Edge, Message};
use manguesechee_core::topology::GridTopology;
use manguesechee_input::{
    find_all_keyboards, find_all_mice, HotkeyAction, HotkeyMatcher, KeyboardCapture, MouseCapture,
};
use manguesechee_network::transport::{TcpReceiver, TcpSender};
use manguesechee_network::{connect, TcpTransport, Transport};
use std::path::PathBuf;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::clipboard;
use crate::edge::EdgeDetector;
use crate::session::{generate_code, load_known_peers, save_known_peers, show_outgoing_code};

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, PartialEq)]
enum ControllerState {
    Local,
    Forwarding,
}

#[derive(Debug)]
enum InboundSignal {
    ReturnControl(Edge, Option<f32>),
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
            s.tls_active = false;
        }
    }
}

struct GrabGuard {
    grab_mouse_tx: tokio::sync::watch::Sender<bool>,
    grab_keyboard_tx: tokio::sync::watch::Sender<bool>,
    ipc_state: Arc<std::sync::Mutex<crate::ipc_server::AgentState>>,
}

impl Drop for GrabGuard {
    fn drop(&mut self) {
        let _ = self.grab_mouse_tx.send(false);
        let _ = self.grab_keyboard_tx.send(false);
        self.ipc_state.lock().unwrap().is_forwarding = false;
    }
}

#[derive(Debug, Clone)]
struct PeerIdentity {
    name: String,
    id: String,
    display_name: String,
}


async fn negotiate_tls(
    transport: &mut TcpTransport,
    addr: &str,
    ipc_state: &crate::ipc_server::SharedState,
) -> anyhow::Result<()> {
    let local_tls_enabled = ipc_state.lock().unwrap().tls_enabled;
    let _ = transport
        .send(&Message::StartTls {
            requested: local_tls_enabled,
        })
        .await;

    let tls_active = match transport.receive().await {
        Ok(Message::StartTlsAck { accept: true }) => {
            info!("TLS accepted by peer; upgrading connection to TLS");
            let client_config = manguesechee_network::tls::create_client_config()?;
            let host = addr.split(':').next().unwrap_or("manguesechee.local");
            transport
                .upgrade_to_tls_client(client_config, host)
                .await?;
            info!("TLS encryption established with {addr}");
            true
        }
        Ok(Message::StartTlsAck { accept: false }) => {
            if local_tls_enabled {
                warn!("peer at {addr} does not have TLS enabled; connection is unencrypted (enable TLS on peer to secure traffic)");
            } else {
                info!("TLS is disabled locally; connection is unencrypted");
            }
            false
        }
        Ok(other) => {
            warn!("peer sent unexpected response to StartTls ({other:?}); continuing unencrypted");
            false
        }
        Err(e) => anyhow::bail!("connection error during TLS negotiation: {e}"),
    };

    ipc_state.lock().unwrap().tls_active = tls_active;
    Ok(())
}


async fn exchange_identity(
    transport: &mut TcpTransport,
    local_name: &str,
    local_id: &str,
    local_display_name: &str,
) -> anyhow::Result<PeerIdentity> {
    transport
        .send(&Message::Identity {
            name: local_name.to_string(),
            id: local_id.to_string(),
            display_name: Some(local_display_name.to_string()),
        })
        .await?;

    match transport.receive().await? {
        Message::Identity {
            name,
            id,
            display_name,
        } => {
            let disp = manguesechee_core::names::clean_display_name(
                display_name.as_deref().unwrap_or(""),
                &id,
            );
            info!("peer: name={name} id={id} display={disp}");
            Ok(PeerIdentity {
                name,
                id,
                display_name: disp,
            })
        }
        other => anyhow::bail!("expected Identity, got {other:?}"),
    }
}


async fn perform_pairing(
    transport: &mut TcpTransport,
    peer: &PeerIdentity,
    local_name: &str,
    local_id: &str,
    local_display_name: &str,
) -> anyhow::Result<Option<Message>> {
    let mut known = load_known_peers().unwrap_or_default();
    if known.contains(&peer.id) {
        info!("peer already known — skipping pairing");
        return Ok(None);
    }

    let code = generate_code();
    show_outgoing_code(&peer.name, &code);
    transport
        .send(&Message::PairRequest {
            name: local_name.to_string(),
            id: local_id.to_string(),
            code: code.clone(),
            display_name: Some(local_display_name.to_string()),
        })
        .await?;

    match transport.receive().await? {
        Message::PairAccepted {
            code: c, name, id, ..
        } => {
            anyhow::ensure!(c == code, "pairing code mismatch");
            info!("paired with '{name}'");
            known.add(id, name);
            save_known_peers(&known)?;
            Ok(None)
        }
        Message::PairRejected { reason } => anyhow::bail!("pairing rejected: {reason}"),
        session_msg @ (Message::FileTransferStatus { .. }
        | Message::ClipboardSync { .. }
        | Message::InputEvent(_)
        | Message::EdgeCrossed { .. }
        | Message::TopologySync { .. }
        | Message::ReturnControl { .. }
        | Message::Ping) => {
            info!("peer '{}' ({}) already has us paired; auto-trusting peer", peer.name, peer.id);
            known.add(peer.id.clone(), peer.name.clone());
            let _ = save_known_peers(&known);
            Ok(Some(session_msg))
        }
        other => anyhow::bail!("expected PairAccepted/Rejected, got {other:?}"),
    }
}


fn register_peer_in_ipc_and_config(
    ipc_state: &crate::ipc_server::SharedState,
    addr: &str,
    peer: &PeerIdentity,
) {
    let existing_pos = {
        let mut s = ipc_state.lock().unwrap();
        s.connected_to = Some(addr.to_string());
        s.last_error = None;
        if let Some(p) = s.peers.iter_mut().find(|p| {
            p.address == addr
                || addr.contains(&p.address)
                || p.address.contains(addr)
                || p.name == peer.name
        }) {
            p.connected = true;
            p.paired = true;
            if !manguesechee_core::names::is_raw_uuid(&peer.display_name) {
                p.display_name = peer.display_name.clone();
            }
            if !p.address.contains(':') {
                p.address = addr.to_string();
            }
            p.position.clone()
        } else {
            let (pos, gx, gy) = manguesechee_core::config::load()
                .ok()
                .and_then(|cfg| {
                    cfg.peers
                        .iter()
                        .find(|p| {
                            p.address.as_deref().unwrap_or("").contains(addr)
                                || p.address
                                    .as_deref()
                                    .is_some_and(|a| !a.is_empty() && addr.contains(a))
                                || p.id == peer.id
                        })
                        .map(|p| {
                            let (x, y) = p.coordinates();
                            (p.position.clone(), x, y)
                        })
                })
                .unwrap_or_else(|| ("right".to_string(), 1, 0));

            s.peers.push(manguesechee_core::ipc::PeerInfo {
                name: peer.name.clone(),
                display_name: peer.display_name.clone(),
                address: addr.to_string(),
                paired: true,
                connected: true,
                position: pos.clone(),
                grid_x: gx,
                grid_y: gy,
            });
            pos
        }
    };

    if let Ok(mut cfg) = manguesechee_core::config::load() {
        if !cfg.peers.iter().any(|p| {
            p.address.as_deref().unwrap_or("").contains(addr)
                || p.address
                    .as_deref()
                    .is_some_and(|a| !a.is_empty() && addr.contains(a))
        }) {
            cfg.peers.push(
                manguesechee_core::config::PeerConfig::new(
                    peer.id.clone(),
                    Some(addr.to_string()),
                    existing_pos,
                )
                .with_display_name(Some(peer.display_name.clone())),
            );
            let _ = manguesechee_core::config::save(&cfg);
        }
    }
}


async fn setup_capture_devices(
    mouse_path: Option<&PathBuf>,
    keyboard_path: Option<&PathBuf>,
    sender: &mut TcpSender,
) -> anyhow::Result<(Vec<PathBuf>, Vec<PathBuf>)> {
    let mouse_paths: Vec<PathBuf> = if let Some(p) = mouse_path {
        vec![p.clone()]
    } else {
        find_all_mice()
    };

    let keyboard_paths: Vec<PathBuf> = if let Some(p) = keyboard_path {
        vec![p.clone()]
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
    sender
        .send(&Message::InputEvent(InputEvent::KeySync {
            pressed_keys: pressed,
        }))
        .await?;

    Ok((mouse_paths, keyboard_paths))
}

fn spawn_capture_workers(
    mouse_paths: Vec<PathBuf>,
    keyboard_paths: Vec<PathBuf>,
) -> (
    mpsc::Receiver<InputEvent>,
    tokio::sync::watch::Sender<bool>,
    tokio::sync::watch::Sender<bool>,
) {
    let (ev_tx, ev_rx) = mpsc::channel::<InputEvent>(512);
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

    (ev_rx, grab_mouse_tx, grab_keyboard_tx)
}


fn spawn_inbound_receiver(
    mut receiver: TcpReceiver,
    initial_msg: Option<Message>,
    addr: String,
    ipc_state: crate::ipc_server::SharedState,
    peer_files_recv: Arc<AtomicBool>,
    grab_mouse_cleanup: tokio::sync::watch::Sender<bool>,
    grab_keyboard_cleanup: tokio::sync::watch::Sender<bool>,
    clipboard_enabled: bool,
) -> mpsc::Receiver<InboundSignal> {
    let (inbound_tx, inbound_rx) = mpsc::channel::<InboundSignal>(32);
    let state_for_recv = Arc::clone(&ipc_state);
    let peer_addr_str = addr.clone();
    let mut pending_first_msg = initial_msg;

    tokio::spawn(async move {
        let mut file_receiver = crate::file_transfer::FileReceiver::default();
        loop {
            let res = if let Some(m) = pending_first_msg.take() {
                Ok(m)
            } else {
                receiver.receive().await
            };
            match res {
                Ok(Message::PairAccepted { name, .. }) => {
                    info!("← PairAccepted received from peer '{name}'");
                }
                Ok(Message::FileTransferStatus { enabled }) => {
                    info!("peer updated FileTransferStatus: enabled={enabled}");
                    peer_files_recv.store(enabled, Ordering::SeqCst);
                }
                Ok(Message::ReturnControl { edge, ratio }) => {
                    info!("← ReturnControl ({edge:?}, ratio={ratio:?})");
                    let _ = inbound_tx.send(InboundSignal::ReturnControl(edge, ratio)).await;
                }
                Ok(Message::FileTransferOffer {
                    transfer_id,
                    files,
                    total_size,
                    is_background,
                }) => {
                    if state_for_recv.lock().unwrap().file_transfer_enabled {
                        file_receiver.handle_offer(
                            transfer_id,
                            files,
                            total_size,
                            is_background,
                            &state_for_recv,
                        );
                    } else {
                        warn!("incoming file transfer offer {transfer_id} ignored — file transfer disabled");
                    }
                }
                Ok(Message::FileTransferChunk {
                    transfer_id,
                    file_index,
                    offset,
                    data,
                    is_last_chunk: _,
                }) => {
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
                Ok(Message::IdentityUpdate { name, display_name }) => {
                    info!("← peer updated identity: name={name} display={display_name}");
                    {
                        let mut s = state_for_recv.lock().unwrap();
                        for p in &mut s.peers {
                            if p.address == peer_addr_str
                                || peer_addr_str.contains(&p.address)
                                || p.address.contains(&peer_addr_str)
                                || p.name == name
                            {
                                p.display_name = display_name.clone();
                            }
                        }
                    }
                    if let Ok(mut cfg) = manguesechee_core::config::load() {
                        for p in &mut cfg.peers {
                            let p_addr = p.address.as_deref().unwrap_or("");
                            if p_addr == peer_addr_str
                                || peer_addr_str.contains(p_addr)
                                || (!p_addr.is_empty() && p_addr.contains(&peer_addr_str))
                                || p.name.as_deref() == Some(&name)
                            {
                                p.display_name = Some(display_name.clone());
                            }
                        }
                        let _ = manguesechee_core::config::save(&cfg);
                    }
                }
                Ok(Message::Pong) => {
                    let _ = inbound_tx.send(InboundSignal::Pong).await;
                }
                Ok(Message::Goodbye) => {
                    info!("← Goodbye received from peer {peer_addr_str}");
                    let _ = inbound_tx
                        .send(InboundSignal::Disconnected("peer closed connection".into()))
                        .await;
                    break;
                }
                Err(e) => {
                    warn!("← receiver error from peer {peer_addr_str}: {e:#}");
                    let _ = inbound_tx
                        .send(InboundSignal::Disconnected(format!("receiver error: {e}")))
                        .await;
                    break;
                }
                Ok(other) => warn!("unexpected: {other:?}"),
            }
        }

        let _ = grab_mouse_cleanup.send(false);
        let _ = grab_keyboard_cleanup.send(false);
        let _ = inbound_tx
            .send(InboundSignal::Disconnected("receiver task finished".into()))
            .await;
    });

    inbound_rx
}


fn resolve_target_edge(
    ipc_state: &Arc<std::sync::Mutex<crate::ipc_server::AgentState>>,
    addr: &str,
) -> Edge {
    let s = ipc_state.lock().unwrap();
    let single_peer = s.peers.len() == 1;
    s.peers
        .iter()
        .find(|p| single_peer || p.address == addr || addr.contains(&p.address) || p.address.contains(addr))
        .map(|p| match p.position.to_lowercase().as_str() {
            "left" => Edge::Left,
            "above" | "top" => Edge::Top,
            "below" | "bottom" => Edge::Bottom,
            _ => Edge::Right,
        })
        .unwrap_or(Edge::Right)
}

async fn sync_clipboard_on_entry(
    sender: &mut TcpSender,
    ipc_state: &Arc<std::sync::Mutex<crate::ipc_server::AgentState>>,
    peer_files_enabled: &Arc<AtomicBool>,
    addr: &str,
) {
    if let Some(text) = clipboard::get_text() {
        if !text.is_empty() && !clipboard::is_already_synced(&text) {
            clipboard::mark_synced(&text);
            let local_allowed = ipc_state.lock().unwrap().file_transfer_enabled;
            let peer_allowed = peer_files_enabled.load(Ordering::SeqCst);
            let files_allowed = local_allowed && peer_allowed;
            if !peer_allowed && crate::file_clipboard::parse_clipboard_file_uris(&text).is_some() {
                info!("→ skipping file transfer on screen entry: peer has file transfer disabled");
            }
            if files_allowed {
                if let Some(paths) = crate::file_clipboard::parse_clipboard_file_uris(&text) {
                    let (files, disk_paths, total_size) =
                        crate::file_clipboard::collect_file_entries(&paths);
                    let cfg_clip = manguesechee_core::config::load()
                        .map(|c| c.clipboard)
                        .unwrap_or_default();
                    let fast_limit = (cfg_clip.fast_limit_mb as u64) * 1024 * 1024;
                    let bg_limit = (cfg_clip.background_limit_mb as u64) * 1024 * 1024;

                    if total_size <= fast_limit {
                        let tid = uuid::Uuid::new_v4().to_string();
                        if let Err(e) = crate::file_transfer::send_fast_transfer(
                            tid, files, disk_paths, total_size, sender, ipc_state,
                        )
                        .await
                        {
                            warn!("failed to send fast file transfer on screen entry: {e}");
                        }
                    } else if total_size <= bg_limit {
                        crate::file_transfer::spawn_background_sender(
                            addr.to_string(),
                            files,
                            disk_paths,
                            total_size,
                            Arc::clone(ipc_state),
                        );
                    } else {
                        let size_str = crate::file_clipboard::format_bytes(total_size);
                        let limit_str = crate::file_clipboard::format_bytes(bg_limit);
                        warn!("transfer size {size_str} exceeds limit {limit_str} — falling back to plain text path");
                        crate::file_clipboard::show_notification(
                            "Manguesechee",
                            &format!("⚠️ Transfert ignoré : {size_str} dépasse la limite autorisée ({limit_str})"),
                        );
                        let _ = sender.send(&Message::ClipboardSync { text }).await;
                    }
                } else {
                    info!("→ syncing clipboard on screen entry ({} bytes)", text.len());
                    let _ = sender.send(&Message::ClipboardSync { text }).await;
                }
            } else if crate::file_clipboard::parse_clipboard_file_uris(&text).is_none() {
                info!("→ syncing clipboard on screen entry ({} bytes)", text.len());
                let _ = sender.send(&Message::ClipboardSync { text }).await;
            }
        }
    }
}


struct ClientLoopContext<'a> {
    addr: &'a str,
    sender: &'a mut TcpSender,
    ev_rx: &'a mut mpsc::Receiver<InputEvent>,
    inbound_rx: &'a mut mpsc::Receiver<InboundSignal>,
    clip_msg_rx: &'a mut mpsc::Receiver<Message>,
    clip_tx_for_keys: &'a mpsc::Sender<Message>,
    broadcast_rx: &'a mut tokio::sync::broadcast::Receiver<Message>,
    grab_mouse_tx: &'a tokio::sync::watch::Sender<bool>,
    grab_keyboard_tx: &'a tokio::sync::watch::Sender<bool>,
    ipc_state: &'a Arc<std::sync::Mutex<crate::ipc_server::AgentState>>,
    peer_files_enabled: &'a Arc<AtomicBool>,
    screen_width: u32,
    screen_height: u32,
    deadzone_px: u32,
    delay_ms: u32,
    velocity_threshold: u32,
    clipboard_enabled: bool,
}

async fn run_client_event_loop(ctx: ClientLoopContext<'_>) -> anyhow::Result<()> {
    let mut state = ControllerState::Local;
    let mut active_coord: (i32, i32) = (0, 0);
    let initial_locked = ctx.ipc_state.lock().unwrap().cursor_locked;
    let mut edge = EdgeDetector::new(ctx.screen_width, ctx.screen_height)
        .with_settings(ctx.deadzone_px, ctx.delay_ms, initial_locked, ctx.velocity_threshold);
    let mut missed_pings: u32 = 0;
    let mut ping_interval = tokio::time::interval(Duration::from_secs(2));
    ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let hotkey_cfg = manguesechee_core::config::load()
        .map(|c| c.hotkeys)
        .unwrap_or_default();
    let mut hotkey_matcher = HotkeyMatcher::from_config(&hotkey_cfg);

    let get_grid = |ipc_state: &Arc<std::sync::Mutex<crate::ipc_server::AgentState>>| -> GridTopology {
        let s = ipc_state.lock().unwrap();
        GridTopology::from_peer_infos(&s.peers)
    };

    info!(
        "ready — move cursor to screen edge or use hotkeys to switch to peer (locked={initial_locked}, deadzone={}px, delay={}ms, velocity_thresh={}px)",
        ctx.deadzone_px, ctx.delay_ms, ctx.velocity_threshold
    );

    loop {
        {
            let is_locked = ctx.ipc_state.lock().unwrap().cursor_locked;
            edge.set_locked(is_locked);
        }

        tokio::select! {
            maybe_ev = ctx.ev_rx.recv() => {
                let Some(event) = maybe_ev else {
                    warn!("local input event stream ended — ungrabbing");
                    let _ = ctx.grab_mouse_tx.send(false);
                    let _ = ctx.grab_keyboard_tx.send(false);
                    ctx.ipc_state.lock().unwrap().is_forwarding = false;
                    break;
                };

                if let InputEvent::Key { key, pressed } = &event {
                    if *pressed && *key == KeyCode::KEY_V && hotkey_matcher.ctrl_held() {
                        crate::file_transfer::notify_premature_paste_if_transferring(ctx.ipc_state);
                    }
                    if *pressed && *key == KeyCode::KEY_C && hotkey_matcher.ctrl_held() {
                        let tx = ctx.clip_tx_for_keys.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(Duration::from_millis(150)).await;
                            if let Some(text) = crate::clipboard::get_text() {
                                if !text.is_empty() && !crate::clipboard::is_already_synced(&text) {
                                    crate::clipboard::mark_synced(&text);
                                    let _ = tx.send(Message::ClipboardSync { text }).await;
                                }
                            }
                        });
                    }
                    if let Some(action) = hotkey_matcher.process_key(*key, *pressed) {
                        match action {
                            HotkeyAction::ToggleCursorLock => {
                                let new_locked = {
                                    let mut s = ctx.ipc_state.lock().unwrap();
                                    s.cursor_locked = !s.cursor_locked;
                                    s.cursor_locked
                                };
                                edge.set_locked(new_locked);
                                if let Ok(mut cfg) = manguesechee_core::config::load() {
                                    cfg.input.cursor_locked = new_locked;
                                    let _ = manguesechee_core::config::save(&cfg);
                                }
                                info!("🔒 Hotkey toggled cursor lock: {}", if new_locked { "LOCKED" } else { "UNLOCKED" });
                                continue;
                            }

                            HotkeyAction::DirectionalJump(jump_edge) => {
                                let grid = get_grid(ctx.ipc_state);
                                let (dx, dy) = jump_edge.delta();
                                let next_coord = (active_coord.0 + dx, active_coord.1 + dy);
                                info!("⌨️ Directional Jump {jump_edge:?} requested from {active_coord:?} -> {next_coord:?}");

                                if next_coord == (0, 0) {
                                    if state == ControllerState::Forwarding {
                                        info!("⌨️ Directional Jump {jump_edge:?}: returning to Local screen (0, 0)");
                                        let _ = ctx.sender.send(&Message::ReturnControl { edge: jump_edge, ratio: None }).await;
                                        let _ = ctx.grab_mouse_tx.send(false);
                                        let _ = ctx.grab_keyboard_tx.send(false);
                                        state = ControllerState::Local;
                                        active_coord = (0, 0);
                                        ctx.ipc_state.lock().unwrap().is_forwarding = false;
                                        hotkey_matcher.reset_modifiers();
                                        edge.place_at_center();
                                        edge.arm_cooldown(Duration::from_millis(300));
                                    }
                                } else if let Some(target_node) = grid.find_at(next_coord.0, next_coord.1) {
                                    info!("⌨️ Directional Jump {jump_edge:?}: target screen '{}' at {next_coord:?}", target_node.name);
                                    match state {
                                        ControllerState::Local => {
                                            let _ = ctx.grab_mouse_tx.send(true);
                                            let _ = ctx.grab_keyboard_tx.send(true);
                                            if let Err(e) = ctx.sender.send(&Message::EdgeCrossed { edge: jump_edge, ratio: None }).await {
                                                warn!("failed to send EdgeCrossed on directional jump: {e}");
                                                let _ = ctx.grab_mouse_tx.send(false);
                                                let _ = ctx.grab_keyboard_tx.send(false);
                                                ctx.ipc_state.lock().unwrap().is_forwarding = false;
                                                break;
                                            }
                                            state = ControllerState::Forwarding;
                                            active_coord = next_coord;
                                            ctx.ipc_state.lock().unwrap().is_forwarding = true;
                                            hotkey_matcher.reset_modifiers();
                                            edge.place_at_center();
                                            edge.arm_cooldown(Duration::from_millis(300));
                                            if ctx.clipboard_enabled {
                                                sync_clipboard_on_entry(ctx.sender, ctx.ipc_state, ctx.peer_files_enabled, ctx.addr).await;
                                            }
                                        }
                                        ControllerState::Forwarding => {
                                            active_coord = next_coord;
                                            hotkey_matcher.reset_modifiers();
                                            let _ = ctx.sender.send(&Message::EdgeCrossed { edge: jump_edge, ratio: None }).await;
                                        }
                                    }
                                } else {
                                    let target_edge = resolve_target_edge(ctx.ipc_state, ctx.addr);
                                    match state {
                                        ControllerState::Local if jump_edge == target_edge => {
                                            info!("⌨️ Directional Jump {jump_edge:?}: jumping to peer ({})", ctx.addr);
                                            let _ = ctx.grab_mouse_tx.send(true);
                                            let _ = ctx.grab_keyboard_tx.send(true);
                                            if let Err(e) = ctx.sender.send(&Message::EdgeCrossed { edge: target_edge, ratio: None }).await {
                                                warn!("failed to send EdgeCrossed: {e}");
                                                let _ = ctx.grab_mouse_tx.send(false);
                                                let _ = ctx.grab_keyboard_tx.send(false);
                                                ctx.ipc_state.lock().unwrap().is_forwarding = false;
                                                break;
                                            }
                                            state = ControllerState::Forwarding;
                                            active_coord = target_edge.delta();
                                            ctx.ipc_state.lock().unwrap().is_forwarding = true;
                                            hotkey_matcher.reset_modifiers();
                                            edge.place_at_center();
                                            edge.arm_cooldown(Duration::from_millis(300));
                                            if ctx.clipboard_enabled {
                                                sync_clipboard_on_entry(ctx.sender, ctx.ipc_state, ctx.peer_files_enabled, ctx.addr).await;
                                            }
                                        }
                                        ControllerState::Forwarding if jump_edge == target_edge.opposite() => {
                                            info!("⌨️ Directional Jump {jump_edge:?}: returning to Local screen (0, 0)");
                                            let _ = ctx.sender.send(&Message::ReturnControl { edge: target_edge, ratio: None }).await;
                                            let _ = ctx.grab_mouse_tx.send(false);
                                            let _ = ctx.grab_keyboard_tx.send(false);
                                            state = ControllerState::Local;
                                            active_coord = (0, 0);
                                            ctx.ipc_state.lock().unwrap().is_forwarding = false;
                                            hotkey_matcher.reset_modifiers();
                                            edge.place_at_center();
                                            edge.arm_cooldown(Duration::from_millis(300));
                                        }
                                        _ => {
                                            info!("⌨️ Directional Jump {jump_edge:?}: hit grid boundary at {active_coord:?} (no adjacent screen)");
                                        }
                                    }
                                }
                                continue;
                            }

                            HotkeyAction::SwitchScreen => {
                                let target_edge = resolve_target_edge(ctx.ipc_state, ctx.addr);
                                match state {
                                    ControllerState::Local => {
                                        info!("⌨️ Hotkey switch: jumping to peer screen ({})", ctx.addr);
                                        let _ = ctx.grab_mouse_tx.send(true);
                                        let _ = ctx.grab_keyboard_tx.send(true);
                                        if let Err(e) = ctx.sender.send(&Message::EdgeCrossed { edge: target_edge, ratio: None }).await {
                                            warn!("failed to send EdgeCrossed on hotkey switch: {e}");
                                            let _ = ctx.grab_mouse_tx.send(false);
                                            let _ = ctx.grab_keyboard_tx.send(false);
                                            ctx.ipc_state.lock().unwrap().is_forwarding = false;
                                            break;
                                        }
                                        state = ControllerState::Forwarding;
                                        active_coord = target_edge.delta();
                                        ctx.ipc_state.lock().unwrap().is_forwarding = true;
                                        hotkey_matcher.reset_modifiers();
                                        edge.place_at_center();
                                        edge.arm_cooldown(Duration::from_millis(300));

                                        if ctx.clipboard_enabled {
                                            sync_clipboard_on_entry(ctx.sender, ctx.ipc_state, ctx.peer_files_enabled, ctx.addr).await;
                                        }
                                    }
                                    ControllerState::Forwarding => {
                                        info!("⌨️ Hotkey switch: returning to local screen");
                                        let _ = ctx.sender.send(&Message::ReturnControl { edge: target_edge, ratio: None }).await;
                                        let _ = ctx.grab_mouse_tx.send(false);
                                        let _ = ctx.grab_keyboard_tx.send(false);
                                        state = ControllerState::Local;
                                        active_coord = (0, 0);
                                        ctx.ipc_state.lock().unwrap().is_forwarding = false;
                                        hotkey_matcher.reset_modifiers();
                                        edge.place_at_center();
                                        edge.arm_cooldown(Duration::from_millis(300));
                                    }
                                }
                                continue;
                            }

                            HotkeyAction::EmergencyEscape => {
                                if state == ControllerState::Forwarding {
                                    let target_edge = resolve_target_edge(ctx.ipc_state, ctx.addr);
                                    info!("🚨 Emergency escape hotkey triggered (key code {}) — ungrabbing immediately", key.0);
                                    let _ = ctx.sender.send(&Message::ReturnControl { edge: target_edge, ratio: None }).await;
                                    let _ = ctx.grab_mouse_tx.send(false);
                                    let _ = ctx.grab_keyboard_tx.send(false);
                                    state = ControllerState::Local;
                                    active_coord = (0, 0);
                                    ctx.ipc_state.lock().unwrap().is_forwarding = false;
                                    hotkey_matcher.reset_modifiers();
                                    edge.place_at_center();
                                    edge.arm_cooldown(Duration::from_millis(300));
                                    continue;
                                }
                            }
                        }
                    }
                }

                match state {
                    ControllerState::Local => {
                        if let InputEvent::MouseMove { dx, dy } = &event {
                            let grid = get_grid(ctx.ipc_state);
                            let target_edge = resolve_target_edge(ctx.ipc_state, ctx.addr);
                            edge.set_allowed_edge(None);

                            if let Some(crossed) = edge.update(*dx, *dy) {
                                let (cdx, cdy) = crossed.delta();
                                let neighbor_coord = (active_coord.0 + cdx, active_coord.1 + cdy);
                                let is_valid = grid.find_at(neighbor_coord.0, neighbor_coord.1).is_some()
                                    || crossed == target_edge;

                                if is_valid {
                                    let ratio = edge.current_ratio(crossed);
                                    info!("→ EdgeCrossed ({crossed:?}, ratio={ratio:.2}) to {neighbor_coord:?} — grabbing");
                                    let _ = ctx.grab_mouse_tx.send(true);
                                    let _ = ctx.grab_keyboard_tx.send(true);
                                    if let Err(e) = ctx.sender.send(&Message::EdgeCrossed { edge: crossed, ratio: Some(ratio) }).await {
                                        warn!("failed to send EdgeCrossed to {}: {e} — ungrabbing", ctx.addr);
                                        let _ = ctx.grab_mouse_tx.send(false);
                                        let _ = ctx.grab_keyboard_tx.send(false);
                                        ctx.ipc_state.lock().unwrap().is_forwarding = false;
                                        break;
                                    }
                                    state = ControllerState::Forwarding;
                                    active_coord = neighbor_coord;
                                    ctx.ipc_state.lock().unwrap().is_forwarding = true;
                                    hotkey_matcher.reset_modifiers();

                                    if ctx.clipboard_enabled {
                                        sync_clipboard_on_entry(ctx.sender, ctx.ipc_state, ctx.peer_files_enabled, ctx.addr).await;
                                    }
                                } else {
                                    edge.place_at_entry(&crossed);
                                }
                            }
                        }
                    }
                    ControllerState::Forwarding => {
                        let res = tokio::time::timeout(
                            Duration::from_millis(500),
                            ctx.sender.send(&Message::InputEvent(event)),
                        ).await;
                        match res {
                            Ok(Ok(())) => {}
                            Ok(Err(e)) => {
                                warn!("failed to send InputEvent to {}: {e} — ungrabbing", ctx.addr);
                                let _ = ctx.grab_mouse_tx.send(false);
                                let _ = ctx.grab_keyboard_tx.send(false);
                                break;
                            }
                            Err(_) => {
                                warn!("timeout sending InputEvent to {} (peer likely offline) — ungrabbing", ctx.addr);
                                let _ = ctx.grab_mouse_tx.send(false);
                                let _ = ctx.grab_keyboard_tx.send(false);
                                break;
                            }
                        }
                    }
                }
            }

            _ = ping_interval.tick() => {
                if missed_pings >= 2 {
                    warn!("peer {} missed 2 consecutive heartbeats — connection dead, ungrabbing and disconnecting", ctx.addr);
                    let _ = ctx.grab_mouse_tx.send(false);
                    let _ = ctx.grab_keyboard_tx.send(false);
                    break;
                }
                missed_pings += 1;
                let ping_res = tokio::time::timeout(
                    Duration::from_millis(800),
                    ctx.sender.send(&Message::Ping),
                ).await;
                match ping_res {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        warn!("heartbeat ping to {} failed: {e} — ungrabbing and disconnecting", ctx.addr);
                        let _ = ctx.grab_mouse_tx.send(false);
                        let _ = ctx.grab_keyboard_tx.send(false);
                        break;
                    }
                    Err(_) => {
                        warn!("heartbeat ping to {} timed out — ungrabbing and disconnecting", ctx.addr);
                        let _ = ctx.grab_mouse_tx.send(false);
                        let _ = ctx.grab_keyboard_tx.send(false);
                        break;
                    }
                }
            }

            Some(clip_msg) = ctx.clip_msg_rx.recv() => {
                match clip_msg {
                    Message::ClipboardSync { text } => {
                        let local_allowed = ctx.ipc_state.lock().unwrap().file_transfer_enabled;
                        let peer_allowed = ctx.peer_files_enabled.load(Ordering::SeqCst);
                        let files_allowed = local_allowed && peer_allowed;
                        if !peer_allowed && crate::file_clipboard::parse_clipboard_file_uris(&text).is_some() {
                            info!("→ skipping file transfer: peer has file transfer disabled");
                        }
                        if files_allowed {
                            if let Some(paths) = crate::file_clipboard::parse_clipboard_file_uris(&text) {
                                let (files, disk_paths, total_size) = crate::file_clipboard::collect_file_entries(&paths);
                                let cfg_clip = manguesechee_core::config::load().map(|c| c.clipboard).unwrap_or_default();
                                let fast_limit = (cfg_clip.fast_limit_mb as u64) * 1024 * 1024;
                                let bg_limit = (cfg_clip.background_limit_mb as u64) * 1024 * 1024;

                                if total_size <= fast_limit {
                                    let tid = uuid::Uuid::new_v4().to_string();
                                    if let Err(e) = crate::file_transfer::send_fast_transfer(tid, files, disk_paths, total_size, ctx.sender, ctx.ipc_state).await {
                                        warn!("failed to send fast file transfer: {e}");
                                    }
                                } else if total_size <= bg_limit {
                                    crate::file_transfer::spawn_background_sender(ctx.addr.to_string(), files, disk_paths, total_size, Arc::clone(ctx.ipc_state));
                                } else {
                                    let size_str = crate::file_clipboard::format_bytes(total_size);
                                    let limit_str = crate::file_clipboard::format_bytes(bg_limit);
                                    warn!("transfer size {size_str} exceeds limit {limit_str} — falling back to plain text path");
                                    crate::file_clipboard::show_notification(
                                        "Manguesechee",
                                        &format!("⚠️ Transfert ignoré : {size_str} dépasse la limite autorisée ({limit_str})"),
                                    );
                                    if let Err(e) = ctx.sender.send(&Message::ClipboardSync { text }).await {
                                        warn!("failed to send clipboard sync: {e}");
                                        let _ = ctx.grab_mouse_tx.send(false);
                                        let _ = ctx.grab_keyboard_tx.send(false);
                                        break;
                                    }
                                }
                            } else {
                                if let Err(e) = ctx.sender.send(&Message::ClipboardSync { text }).await {
                                    warn!("failed to send clipboard sync: {e}");
                                    let _ = ctx.grab_mouse_tx.send(false);
                                    let _ = ctx.grab_keyboard_tx.send(false);
                                    break;
                                }
                            }
                        } else if crate::file_clipboard::parse_clipboard_file_uris(&text).is_none() {
                            if let Err(e) = ctx.sender.send(&Message::ClipboardSync { text }).await {
                                warn!("failed to send clipboard sync: {e}");
                                let _ = ctx.grab_mouse_tx.send(false);
                                let _ = ctx.grab_keyboard_tx.send(false);
                                break;
                            }
                        }
                    }
                    other => {
                        if let Err(e) = ctx.sender.send(&other).await {
                            warn!("failed to send clipboard msg: {e}");
                            let _ = ctx.grab_mouse_tx.send(false);
                            let _ = ctx.grab_keyboard_tx.send(false);
                            break;
                        }
                    }
                }
            }

            Ok(bmsg) = ctx.broadcast_rx.recv() => {
                if matches!(bmsg, Message::Goodbye) {
                    info!("broadcast Goodbye received — disconnecting from peer {}", ctx.addr);
                    let _ = ctx.grab_mouse_tx.send(false);
                    let _ = ctx.grab_keyboard_tx.send(false);
                    ctx.ipc_state.lock().unwrap().is_forwarding = false;
                    let _ = ctx.sender.send(&Message::Goodbye).await;
                    break;
                }
                if matches!(bmsg, Message::ReturnControl { .. }) {
                    if state == ControllerState::Forwarding {
                        let target_edge = resolve_target_edge(ctx.ipc_state, ctx.addr);
                        info!("IPC ReturnControl: ungrabbing and returning to local screen");
                        let _ = ctx.sender.send(&Message::ReturnControl { edge: target_edge, ratio: None }).await;
                        let _ = ctx.grab_mouse_tx.send(false);
                        let _ = ctx.grab_keyboard_tx.send(false);
                        state = ControllerState::Local;
                        active_coord = (0, 0);
                        ctx.ipc_state.lock().unwrap().is_forwarding = false;
                        edge.place_at_center();
                        edge.arm_cooldown(Duration::from_millis(300));
                    }
                    continue;
                }
                if matches!(bmsg, Message::EdgeCrossed { .. }) {
                    if state == ControllerState::Local {
                        let target_edge = resolve_target_edge(ctx.ipc_state, ctx.addr);
                        info!("IPC SwitchScreen: grabbing and switching to peer screen ({})", ctx.addr);
                        let _ = ctx.grab_mouse_tx.send(true);
                        let _ = ctx.grab_keyboard_tx.send(true);
                        if let Err(e) = ctx.sender.send(&Message::EdgeCrossed { edge: target_edge, ratio: None }).await {
                            warn!("failed to send EdgeCrossed on IPC switch: {e}");
                            let _ = ctx.grab_mouse_tx.send(false);
                            let _ = ctx.grab_keyboard_tx.send(false);
                            ctx.ipc_state.lock().unwrap().is_forwarding = false;
                            break;
                        }
                        state = ControllerState::Forwarding;
                        active_coord = target_edge.delta();
                        ctx.ipc_state.lock().unwrap().is_forwarding = true;
                        edge.place_at_center();
                        edge.arm_cooldown(Duration::from_millis(300));
                        if ctx.clipboard_enabled {
                            sync_clipboard_on_entry(ctx.sender, ctx.ipc_state, ctx.peer_files_enabled, ctx.addr).await;
                        }
                    }
                    continue;
                }
                if let Err(e) = ctx.sender.send(&bmsg).await {
                    warn!("failed to send broadcast msg to peer: {e}");
                    let _ = ctx.grab_mouse_tx.send(false);
                    let _ = ctx.grab_keyboard_tx.send(false);
                    ctx.ipc_state.lock().unwrap().is_forwarding = false;
                    break;
                }
            }

            inbound = ctx.inbound_rx.recv() => {
                match inbound {
                    Some(InboundSignal::ReturnControl(exit_edge, ratio)) => {
                        if state == ControllerState::Forwarding {
                            let (edx, edy) = exit_edge.delta();
                            let next_coord = (active_coord.0 + edx, active_coord.1 + edy);
                            let grid = get_grid(ctx.ipc_state);

                            if next_coord == (0, 0) || grid.find_at(next_coord.0, next_coord.1).is_none() {
                                info!("← ReturnControl ({exit_edge:?}, ratio={ratio:?}) from {active_coord:?} -> returning to local (0, 0)");
                                let _ = ctx.grab_mouse_tx.send(false);
                                let _ = ctx.grab_keyboard_tx.send(false);
                                state = ControllerState::Local;
                                active_coord = (0, 0);
                                ctx.ipc_state.lock().unwrap().is_forwarding = false;
                                hotkey_matcher.reset_modifiers();
                                let return_edge = exit_edge.opposite();
                                edge.place_at_entry_ratio(&return_edge, ratio);
                                edge.arm_cooldown(Duration::from_millis(600));
                            } else {
                                info!("← ReturnControl ({exit_edge:?}, ratio={ratio:?}) traversing from {active_coord:?} to adjacent screen at {next_coord:?}");
                                active_coord = next_coord;
                                let _ = ctx.sender.send(&Message::EdgeCrossed { edge: exit_edge, ratio }).await;
                            }
                        }
                    }
                    Some(InboundSignal::Pong) => {
                        missed_pings = 0;
                    }
                    Some(InboundSignal::Disconnected(reason)) => {
                        warn!("peer {} disconnected ({reason}) — ungrabbing and exiting session", ctx.addr);
                        let _ = ctx.grab_mouse_tx.send(false);
                        let _ = ctx.grab_keyboard_tx.send(false);
                        break;
                    }
                    None => {
                        warn!("inbound channel closed for {} — ungrabbing and exiting session", ctx.addr);
                        let _ = ctx.grab_mouse_tx.send(false);
                        let _ = ctx.grab_keyboard_tx.send(false);
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}


pub async fn connect_to(
    addr: &str,
    local_name: String,
    local_id: String,
    local_display_name: String,
    mouse_path: Option<PathBuf>,
    keyboard_path: Option<PathBuf>,
    screen_width: u32,
    screen_height: u32,
    deadzone_px: u32,
    delay_ms: u32,
    velocity_threshold: u32,
    ipc_state: crate::ipc_server::SharedState,
    broadcast_tx: tokio::sync::broadcast::Sender<Message>,
) -> anyhow::Result<()> {
    info!("connecting to {addr}  (screen {screen_width}×{screen_height})");
    let mut transport = connect(addr).await?;

    negotiate_tls(&mut transport, addr, &ipc_state).await?;
    let peer = exchange_identity(&mut transport, &local_name, &local_id, &local_display_name).await?;
    let initial_msg = perform_pairing(&mut transport, &peer, &local_name, &local_id, &local_display_name).await?;
    register_peer_in_ipc_and_config(&ipc_state, addr, &peer);

    let _conn_guard = PeerConnectedGuard {
        ipc_state: Arc::clone(&ipc_state),
        addr: addr.to_string(),
    };

    let (mut sender, receiver) = transport.into_split();

    let initial_files_allowed = ipc_state.lock().unwrap().file_transfer_enabled;
    let _ = sender
        .send(&Message::FileTransferStatus {
            enabled: initial_files_allowed,
        })
        .await;
    let peer_files_enabled = Arc::new(AtomicBool::new(true));

    let (mouse_paths, keyboard_paths) = setup_capture_devices(
        mouse_path.as_ref(),
        keyboard_path.as_ref(),
        &mut sender,
    )
    .await?;
    let (mut ev_rx, grab_mouse_tx, grab_keyboard_tx) =
        spawn_capture_workers(mouse_paths, keyboard_paths);

    let (clip_msg_tx, mut clip_msg_rx) = mpsc::channel::<Message>(16);
    let clip_tx_for_keys = clip_msg_tx.clone();
    let clipboard_enabled = manguesechee_core::config::load()
        .map(|c| c.clipboard.enabled)
        .unwrap_or(true);
    if clipboard_enabled {
        clipboard::spawn_watcher(clip_msg_tx);
    }

    let mut inbound_rx = spawn_inbound_receiver(
        receiver,
        initial_msg,
        addr.to_string(),
        Arc::clone(&ipc_state),
        Arc::clone(&peer_files_enabled),
        grab_mouse_tx.clone(),
        grab_keyboard_tx.clone(),
        clipboard_enabled,
    );

    let _grab_guard = GrabGuard {
        grab_mouse_tx: grab_mouse_tx.clone(),
        grab_keyboard_tx: grab_keyboard_tx.clone(),
        ipc_state: Arc::clone(&ipc_state),
    };

    let mut broadcast_rx = broadcast_tx.subscribe();

    let loop_ctx = ClientLoopContext {
        addr,
        sender: &mut sender,
        ev_rx: &mut ev_rx,
        inbound_rx: &mut inbound_rx,
        clip_msg_rx: &mut clip_msg_rx,
        clip_tx_for_keys: &clip_tx_for_keys,
        broadcast_rx: &mut broadcast_rx,
        grab_mouse_tx: &grab_mouse_tx,
        grab_keyboard_tx: &grab_keyboard_tx,
        ipc_state: &ipc_state,
        peer_files_enabled: &peer_files_enabled,
        screen_width,
        screen_height,
        deadzone_px,
        delay_ms,
        velocity_threshold,
        clipboard_enabled,
    };

    run_client_event_loop(loop_ctx).await?;

    let _ = grab_mouse_tx.send(false);
    let _ = grab_keyboard_tx.send(false);
    let _ = sender.send(&Message::Goodbye).await;
    Ok(())
}
