use manguesechee_core::events::InputEvent;
use manguesechee_core::protocol::Message;
use manguesechee_input::{KeyboardInjector, MouseInjector};
use manguesechee_network::transport::{TcpReceiver, TcpSender};
use manguesechee_network::{wrap, TcpTransport, Transport};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tracing::{error, info, warn};

use crate::clipboard;
use crate::edge::EdgeDetector;
use crate::session::{load_known_peers, prompt_accept, save_known_peers};

#[derive(Debug, Clone)]
struct PeerIdentity {
    name: String,
    id: String,
    display_name: String,
}

struct VirtualDevices {
    mouse: MouseInjector,
    keyboard: KeyboardInjector,
}

impl VirtualDevices {
    fn new() -> anyhow::Result<Self> {
        let mouse = MouseInjector::new().map_err(|e| {
            error!("failed to create virtual mouse: {e:#}");
            anyhow::anyhow!("Virtual mouse creation failed: {e}. Check /dev/uinput permissions.")
        })?;
        let keyboard = KeyboardInjector::new().map_err(|e| {
            error!("failed to create virtual keyboard: {e:#}");
            anyhow::anyhow!("Virtual keyboard creation failed: {e}. Check /dev/uinput permissions.")
        })?;
        Ok(Self { mouse, keyboard })
    }

    fn release_all(&mut self) {
        let _ = self.mouse.release_all();
        let _ = self.keyboard.release_all();
    }
}

pub async fn run(
    listener: TcpListener,
    local_name: String,
    local_id: String,
    local_display_name: String,
    screen_width: u32,
    screen_height: u32,
    port: u16,
    ipc_state: crate::ipc_server::SharedState,
    connect_tx: tokio::sync::mpsc::UnboundedSender<String>,
    broadcast_tx: tokio::sync::broadcast::Sender<Message>,
) {
    if let Ok(addr) = listener.local_addr() {
        info!("listening on {addr}");
    }
    loop {
        match listener.accept().await {
            Ok((stream, peer_addr)) => {
                info!("incoming from {peer_addr}");
                let name = local_name.clone();
                let id = local_id.clone();
                let disp = local_display_name.clone();
                let state = Arc::clone(&ipc_state);
                let tx = connect_tx.clone();
                let b_tx = broadcast_tx.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle(
                        stream,
                        peer_addr,
                        name,
                        id,
                        disp,
                        screen_width,
                        screen_height,
                        port,
                        state,
                        tx,
                        b_tx,
                    )
                    .await
                    {
                        error!("session error from {peer_addr}: {e:#}");
                    }
                });
            }
            Err(e) => warn!("accept error: {e}"),
        }
    }
}


async fn negotiate_tls_and_identity(
    mut transport: TcpTransport,
    peer_addr: SocketAddr,
    local_name: &str,
    local_id: &str,
    local_display_name: &str,
    ipc_state: &crate::ipc_server::SharedState,
) -> anyhow::Result<Option<(TcpTransport, PeerIdentity)>> {
    let first_msg = transport.receive().await?;
    let (peer_name, peer_id, peer_display_name) = match first_msg {
        Message::StartTls { requested } => {
            let local_tls_enabled = ipc_state.lock().unwrap().tls_enabled;
            let should_accept = requested && local_tls_enabled;
            transport
                .send(&Message::StartTlsAck {
                    accept: should_accept,
                })
                .await?;
            if should_accept {
                info!("upgrading incoming connection from {peer_addr} to TLS");
                let (certs, key) =
                    manguesechee_network::tls::load_or_generate_identity(local_name)?;
                let server_config =
                    manguesechee_network::tls::create_server_config(certs, key)?;
                transport.upgrade_to_tls_server(server_config).await?;
                info!("🔒 TLS encryption established with incoming peer {peer_addr}");
                ipc_state.lock().unwrap().tls_active = true;
            } else {
                if requested && !local_tls_enabled {
                    info!("peer requested TLS, but local TLS is disabled; continuing unencrypted");
                }
                ipc_state.lock().unwrap().tls_active = false;
            }

            match transport.receive().await? {
                Message::FileChannelInit { transfer_id } => {
                    info!("incoming dedicated file channel from {peer_addr} (transfer {transfer_id})");
                    crate::file_transfer::handle_incoming_file_channel(
                        transport,
                        transfer_id,
                        Arc::clone(ipc_state),
                    )
                    .await?;
                    return Ok(None);
                }
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
                    (name, id, disp)
                }
                other => anyhow::bail!(
                    "expected Identity or FileChannelInit after TLS negotiation, got {other:?}"
                ),
            }
        }
        Message::FileChannelInit { transfer_id } => {
            ipc_state.lock().unwrap().tls_active = false;
            info!("incoming dedicated file channel from {peer_addr} (transfer {transfer_id})");
            crate::file_transfer::handle_incoming_file_channel(
                transport,
                transfer_id,
                Arc::clone(ipc_state),
            )
            .await?;
            return Ok(None);
        }
        Message::Identity {
            name,
            id,
            display_name,
        } => {
            ipc_state.lock().unwrap().tls_active = false;
            let disp = manguesechee_core::names::clean_display_name(
                display_name.as_deref().unwrap_or(""),
                &id,
            );
            info!("peer: name={name} id={id} display={disp}");
            (name, id, disp)
        }
        other => anyhow::bail!("expected StartTls or Identity or FileChannelInit, got {other:?}"),
    };

    transport
        .send(&Message::Identity {
            name: local_name.to_string(),
            id: local_id.to_string(),
            display_name: Some(local_display_name.to_string()),
        })
        .await?;

    Ok(Some((
        transport,
        PeerIdentity {
            name: peer_name,
            id: peer_id,
            display_name: peer_display_name,
        },
    )))
}


async fn perform_pairing_server(
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

    let first_msg = transport.receive().await?;
    match first_msg {
        Message::PairRequest { name, id, code, .. } => {
            let accepted = tokio::task::spawn_blocking({
                let name = name.clone();
                let code = code.clone();
                move || prompt_accept(&name, &code)
            })
            .await
            .unwrap_or(false);

            if accepted {
                transport
                    .send(&Message::PairAccepted {
                        name: local_name.to_string(),
                        id: local_id.to_string(),
                        code,
                        display_name: Some(local_display_name.to_string()),
                    })
                    .await?;
                known.add(id, name.clone());
                let _ = save_known_peers(&known);
                info!("paired with '{name}'");
                Ok(None)
            } else {
                transport
                    .send(&Message::PairRejected {
                        reason: "user rejected".into(),
                    })
                    .await?;
                anyhow::bail!("pairing rejected");
            }
        }
        active_msg @ (Message::InputEvent(_)
        | Message::EdgeCrossed { .. }
        | Message::Ping
        | Message::ClipboardSync { .. }
        | Message::FileTransferStatus { .. }
        | Message::TopologySync { .. }
        | Message::ReturnControl { .. }) => {
            info!(
                "peer '{}' ({}) already paired from remote; auto-trusting peer",
                peer.name, peer.id
            );
            known.add(peer.id.clone(), peer.name.clone());
            let _ = save_known_peers(&known);
            Ok(Some(active_msg))
        }
        other => anyhow::bail!("expected PairRequest or session message, got {other:?}"),
    }
}


fn register_peer_and_auto_connect(
    peer_addr: SocketAddr,
    port: u16,
    peer: &PeerIdentity,
    ipc_state: &crate::ipc_server::SharedState,
    connect_tx: &tokio::sync::mpsc::UnboundedSender<String>,
) -> String {
    let peer_ip = peer_addr.ip().to_string();
    let peer_target_addr = format!("{peer_ip}:{port}");

    let existing_pos = {
        let mut s = ipc_state.lock().unwrap();
        if let Some(existing) = s
            .peers
            .iter_mut()
            .find(|p| p.address.contains(&peer_ip) || p.name == peer.name)
        {
            existing.paired = true;
            if !manguesechee_core::names::is_raw_uuid(&peer.display_name) {
                existing.display_name = peer.display_name.clone();
            }
            if !existing.address.contains(':') {
                existing.address = peer_target_addr.clone();
            }
            existing.position.clone()
        } else {
            let pos = manguesechee_core::config::load()
                .ok()
                .and_then(|cfg| {
                    cfg.peers
                        .iter()
                        .find(|p| {
                            p.address.as_deref().unwrap_or("").contains(&peer_ip)
                                || p.id == peer.id
                        })
                        .map(|p| p.position.clone())
                })
                .unwrap_or_else(|| "right".to_string());

            s.peers.push(
                manguesechee_core::ipc::PeerInfo::new(
                    peer.name.clone(),
                    peer_target_addr.clone(),
                    true,
                    false,
                    pos.clone(),
                )
                .with_display_name(peer.display_name.clone()),
            );
            pos
        }
    };

    if let Ok(mut cfg) = manguesechee_core::config::load() {
        if !cfg
            .peers
            .iter()
            .any(|p| p.address.as_deref().unwrap_or("").contains(&peer_ip))
        {
            cfg.peers.push(
                manguesechee_core::config::PeerConfig::new(
                    peer.id.clone(),
                    Some(peer_target_addr.clone()),
                    existing_pos,
                )
                .with_display_name(Some(peer.display_name.clone())),
            );
            let _ = manguesechee_core::config::save(&cfg);
        }

        if cfg.input.enabled {
            let already_connected = ipc_state.lock().unwrap().connected_to.is_some();
            if !already_connected {
                info!("Bidirectional auto-connect: initiating reverse controller connection to {peer_target_addr}");
                let _ = connect_tx.send(peer_target_addr.clone());
            }
        }
    }

    peer_target_addr
}


fn spawn_outbound_sender(
    mut sender: TcpSender,
    mut out_rx: tokio::sync::mpsc::Receiver<Message>,
    mut broadcast_rx: tokio::sync::broadcast::Receiver<Message>,
    peer_target_addr: String,
    ipc_state: crate::ipc_server::SharedState,
    peer_files_enabled: Arc<AtomicBool>,
) {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                Some(msg) = out_rx.recv() => {
                    match msg {
                        Message::ClipboardSync { text } => {
                            let local_allowed = ipc_state.lock().unwrap().file_transfer_enabled;
                            let peer_allowed = peer_files_enabled.load(Ordering::SeqCst);
                            let files_allowed = local_allowed && peer_allowed;
                            if !peer_allowed && crate::file_clipboard::parse_clipboard_file_uris(&text).is_some() {
                                info!("server: skipping file transfer: peer has file transfer disabled");
                            }
                            if files_allowed {
                                if let Some(paths) = crate::file_clipboard::parse_clipboard_file_uris(&text) {
                                    let (files, disk_paths, total_size) = crate::file_clipboard::collect_file_entries(&paths);
                                    let cfg_clip = manguesechee_core::config::load().map(|c| c.clipboard).unwrap_or_default();
                                    let fast_limit = (cfg_clip.fast_limit_mb as u64) * 1024 * 1024;
                                    let bg_limit = (cfg_clip.background_limit_mb as u64) * 1024 * 1024;

                                    if total_size <= fast_limit {
                                        let tid = uuid::Uuid::new_v4().to_string();
                                        if let Err(e) = crate::file_transfer::send_fast_transfer(tid, files, disk_paths, total_size, &mut sender, &ipc_state).await {
                                            warn!("send fast file transfer failed: {e}");
                                        }
                                    } else if total_size <= bg_limit {
                                        crate::file_transfer::spawn_background_sender(peer_target_addr.clone(), files, disk_paths, total_size, Arc::clone(&ipc_state));
                                    }
                                } else if let Err(e) = sender.send(&Message::ClipboardSync { text }).await {
                                    warn!("send: {e}");
                                    break;
                                }
                            } else if crate::file_clipboard::parse_clipboard_file_uris(&text).is_none() {
                                if let Err(e) = sender.send(&Message::ClipboardSync { text }).await {
                                    warn!("send: {e}");
                                    break;
                                }
                            }
                        }
                        other => {
                            if let Err(e) = sender.send(&other).await {
                                warn!("send: {e}");
                                break;
                            }
                        }
                    }
                }
                Ok(msg) = broadcast_rx.recv() => {
                    if let Err(e) = sender.send(&msg).await {
                        warn!("broadcast send: {e}");
                        break;
                    }
                }
            }
        }
    });
}


struct ServerLoopContext<'a> {
    edge: &'a mut EdgeDetector,
    devices: &'a mut VirtualDevices,
    has_control: &'a mut bool,
    ctrl_held: &'a mut bool,
    file_receiver: &'a mut crate::file_transfer::FileReceiver,
    out_tx: &'a tokio::sync::mpsc::Sender<Message>,
    ipc_state: &'a crate::ipc_server::SharedState,
    peer_files_enabled: &'a Arc<AtomicBool>,
    peer_ip: &'a str,
    peer_name: &'a str,
    peer_id: &'a str,
    local_name: &'a str,
    local_id: &'a str,
    local_display_name: &'a str,
    clipboard_enabled: bool,
}

fn handle_server_message(
    msg: Message,
    ctx: &mut ServerLoopContext<'_>,
) -> anyhow::Result<bool> {
    match msg {
        Message::EdgeCrossed { edge: entry, ratio } => {
            let return_edge = entry.opposite();
            ctx.edge.set_allowed_edge(None);
            ctx.edge.place_at_entry_ratio(&return_edge, ratio);
            *ctx.has_control = true;
            *ctx.ctrl_held = false;
            info!("cursor entered from {return_edge:?} (controller exited {entry:?}, ratio={ratio:?}) — multi-directional navigation enabled");
        }

        Message::ReturnControl { edge, ratio: _ } => {
            *ctx.has_control = false;
            *ctx.ctrl_held = false;
            ctx.devices.release_all();
            info!("controller reclaimed control via ReturnControl ({edge:?}) — released all virtual keys & mouse buttons");
        }

        Message::InputEvent(event) => {
            if !*ctx.has_control {
                return Ok(true);
            }

            if let InputEvent::MouseMove { dx, dy } = &event {
                if let Some(exit) = ctx.edge.update(*dx, *dy) {
                    *ctx.has_control = false;
                    *ctx.ctrl_held = false;
                    let ratio = ctx.edge.current_ratio(exit);
                    info!("cursor left via {exit:?} (ratio={ratio:.2}) — ReturnControl");
                    ctx.devices.release_all();
                    if ctx.clipboard_enabled {
                        if let Some(text) = clipboard::get_text() {
                            if !text.is_empty() && !clipboard::is_already_synced(&text) {
                                clipboard::mark_synced(&text);
                                let _ = ctx.out_tx.try_send(Message::ClipboardSync { text });
                            }
                        }
                    }
                    let _ = ctx.out_tx.try_send(Message::ReturnControl {
                        edge: exit,
                        ratio: Some(ratio),
                    });
                    return Ok(true);
                }
            }

            if let InputEvent::Key { key, pressed } = &event {
                if key.is_ctrl() {
                    *ctx.ctrl_held = *pressed;
                } else if *pressed && *key == manguesechee_core::events::KeyCode::KEY_V && *ctx.ctrl_held {
                    crate::file_transfer::notify_premature_paste_if_transferring(ctx.ipc_state);
                }
            }

            match &event {
                InputEvent::MouseMove { .. }
                | InputEvent::MouseButton { .. }
                | InputEvent::MouseScroll { .. } => ctx.devices.mouse.inject(&event)?,
                InputEvent::Key { .. } | InputEvent::KeySync { .. } => {
                    ctx.devices.keyboard.inject(&event)?
                }
            }
        }

        Message::FileTransferStatus { enabled } => {
            info!("server: received FileTransferStatus from peer: enabled={enabled}");
            ctx.peer_files_enabled.store(enabled, Ordering::SeqCst);
        }

        Message::FileTransferOffer {
            transfer_id,
            files,
            total_size,
            is_background,
        } => {
            if ctx.ipc_state.lock().unwrap().file_transfer_enabled {
                ctx.file_receiver.handle_offer(
                    transfer_id,
                    files,
                    total_size,
                    is_background,
                    ctx.ipc_state,
                );
            } else {
                warn!("incoming file transfer offer {transfer_id} ignored — file transfer disabled");
            }
        }

        Message::FileTransferChunk {
            transfer_id,
            file_index,
            offset,
            data,
            is_last_chunk: _,
        } => {
            ctx.file_receiver
                .handle_chunk(&transfer_id, file_index, offset, &data, ctx.ipc_state);
        }

        Message::FileTransferDone { transfer_id } => {
            ctx.file_receiver.handle_done(&transfer_id, ctx.ipc_state);
        }

        Message::ClipboardSync { text } => {
            info!("← ClipboardSync ({} bytes)", text.len());
            if ctx.clipboard_enabled {
                if let Err(e) = clipboard::set_text(&text) {
                    warn!("clipboard set failed: {e}");
                }
            }
        }

        Message::TopologySync { position } => {
            let opp = manguesechee_core::protocol::opposite_position(&position).to_string();
            info!("received TopologySync: peer set our position to {position} -> setting peer to {opp}");
            {
                let mut s = ctx.ipc_state.lock().unwrap();
                s.topology_configured = true;
                let single_peer = s.peers.len() == 1;
                for p in &mut s.peers {
                    if single_peer || p.address.contains(ctx.peer_ip) || p.name == ctx.peer_name {
                        p.position = opp.clone();
                    }
                }
            }
            if let Ok(mut cfg) = manguesechee_core::config::load() {
                let single_peer = cfg.peers.len() == 1;
                for p in &mut cfg.peers {
                    if single_peer
                        || p.address.as_deref().unwrap_or("").contains(ctx.peer_ip)
                        || p.id == ctx.peer_id
                    {
                        p.position = opp.clone();
                    }
                }
                let _ = manguesechee_core::config::save(&cfg);
            }
        }

        Message::PairRequest { name, id, code, .. } => {
            info!("received PairRequest during session from {name} ({id}) — re-accepting");
            let _ = ctx.out_tx.try_send(Message::PairAccepted {
                name: ctx.local_name.to_string(),
                id: ctx.local_id.to_string(),
                code,
                display_name: Some(ctx.local_display_name.to_string()),
            });
        }

        Message::IdentityUpdate { name, display_name } => {
            info!("received IdentityUpdate from {name}: display_name={display_name}");
            {
                let mut s = ctx.ipc_state.lock().unwrap();
                for p in &mut s.peers {
                    if p.address.contains(ctx.peer_ip)
                        || p.name == name
                        || p.name == ctx.peer_name
                    {
                        p.display_name = display_name.clone();
                    }
                }
            }
            if let Ok(mut cfg) = manguesechee_core::config::load() {
                for p in &mut cfg.peers {
                    if p.address.as_deref().unwrap_or("").contains(ctx.peer_ip)
                        || p.id == ctx.peer_id
                        || p.name.as_deref() == Some(&name)
                    {
                        p.display_name = Some(display_name.clone());
                    }
                }
                let _ = manguesechee_core::config::save(&cfg);
            }
        }

        Message::Ping => {
            let _ = ctx.out_tx.try_send(Message::Pong);
        }
        Message::Goodbye => {
            info!("peer disconnected");
            return Ok(false);
        }
        other => warn!("unexpected: {other:?}"),
    }
    Ok(true)
}

async fn run_server_event_loop(
    mut receiver: TcpReceiver,
    initial_msg: Option<Message>,
    mut ctx: ServerLoopContext<'_>,
) -> anyhow::Result<()> {
    if let Some(msg) = initial_msg {
        if !handle_server_message(msg, &mut ctx)? {
            return Ok(());
        }
    }

    loop {
        let msg = if *ctx.has_control {
            match tokio::time::timeout(std::time::Duration::from_secs(5), receiver.receive()).await {
                Ok(Ok(m)) => m,
                Ok(Err(e)) => {
                    info!("peer connection closed: {e:#}");
                    break;
                }
                Err(_) => {
                    warn!("timed out waiting for peer with active control — releasing virtual devices");
                    ctx.devices.release_all();
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

        if !handle_server_message(msg, &mut ctx)? {
            break;
        }
    }

    Ok(())
}


async fn handle(
    stream: TcpStream,
    peer_addr: SocketAddr,
    local_name: String,
    local_id: String,
    local_display_name: String,
    screen_width: u32,
    screen_height: u32,
    port: u16,
    ipc_state: crate::ipc_server::SharedState,
    connect_tx: tokio::sync::mpsc::UnboundedSender<String>,
    broadcast_tx: tokio::sync::broadcast::Sender<Message>,
) -> anyhow::Result<()> {
    let transport = wrap(stream);
    let Some((mut transport, peer)) = negotiate_tls_and_identity(
        transport,
        peer_addr,
        &local_name,
        &local_id,
        &local_display_name,
        &ipc_state,
    )
    .await? else {
        return Ok(());
    };

    let initial_msg = perform_pairing_server(
        &mut transport,
        &peer,
        &local_name,
        &local_id,
        &local_display_name,
    )
    .await?;

    let peer_target_addr =
        register_peer_and_auto_connect(peer_addr, port, &peer, &ipc_state, &connect_tx);

    let (sender, receiver) = transport.into_split();
    let mut devices = VirtualDevices::new()?;
    info!("virtual mouse + keyboard ready");

    let cfg_input = manguesechee_core::config::load()
        .map(|c| c.input)
        .unwrap_or_default();
    let mut edge = EdgeDetector::new(screen_width, screen_height).with_settings(
        cfg_input.corner_deadzone_px,
        cfg_input.switch_delay_ms,
        false,
        cfg_input.edge_velocity_threshold,
    );

    let (out_tx, out_rx) = tokio::sync::mpsc::channel::<Message>(16);
    let initial_files_allowed = ipc_state.lock().unwrap().file_transfer_enabled;
    let _ = out_tx.try_send(Message::FileTransferStatus {
        enabled: initial_files_allowed,
    });
    let peer_files_enabled = Arc::new(AtomicBool::new(true));

    let clipboard_enabled = manguesechee_core::config::load()
        .map(|c| c.clipboard.enabled)
        .unwrap_or(true);
    if clipboard_enabled {
        clipboard::spawn_watcher(out_tx.clone());
    }

    let broadcast_rx = broadcast_tx.subscribe();
    spawn_outbound_sender(
        sender,
        out_rx,
        broadcast_rx,
        peer_target_addr,
        Arc::clone(&ipc_state),
        Arc::clone(&peer_files_enabled),
    );

    let mut has_control = false;
    let mut ctrl_held = false;
    let mut file_receiver = crate::file_transfer::FileReceiver::default();
    let peer_ip = peer_addr.ip().to_string();

    let loop_ctx = ServerLoopContext {
        edge: &mut edge,
        devices: &mut devices,
        has_control: &mut has_control,
        ctrl_held: &mut ctrl_held,
        file_receiver: &mut file_receiver,
        out_tx: &out_tx,
        ipc_state: &ipc_state,
        peer_files_enabled: &peer_files_enabled,
        peer_ip: &peer_ip,
        peer_name: &peer.name,
        peer_id: &peer.id,
        local_name: &local_name,
        local_id: &local_id,
        local_display_name: &local_display_name,
        clipboard_enabled,
    };

    run_server_event_loop(receiver, initial_msg, loop_ctx).await?;

    devices.release_all();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    ipc_state.lock().unwrap().tls_active = false;
    Ok(())
}
