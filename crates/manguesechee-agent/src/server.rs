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
) {
    info!("listening on {}", listener.local_addr().unwrap());
    loop {
        match listener.accept().await {
            Ok((stream, peer_addr)) => {
                info!("incoming from {peer_addr}");
                let name = local_name.clone();
                let id   = local_id.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle(stream, name, id, screen_width, screen_height).await {
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
    local_name:    String,
    local_id:      String,
    screen_width:  u32,
    screen_height: u32,
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

    // Outbound channel — ReturnControl and Pong both go here
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<Message>(8);
    tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            if let Err(e) = sender.send(&msg).await { warn!("send: {e}"); break; }
        }
    });

    // Closure to process any message
    let handle_msg = |msg: Message,
                      edge: &mut EdgeDetector,
                      mouse: &mut MouseInjector,
                      keyboard: &mut KeyboardInjector,
                      out_tx: &tokio::sync::mpsc::Sender<Message>| -> anyhow::Result<bool> {
        match msg {
            Message::EdgeCrossed { edge: entry } => {
                edge.place_at_entry(&entry.opposite());
                info!("cursor entered from {entry:?}");
            }

            Message::InputEvent(event) => {
                if let InputEvent::MouseMove { dx, dy } = &event {
                    if let Some(exit) = edge.update(*dx, *dy) {
                        info!("cursor left via {exit:?} — ReturnControl");
                        let _ = out_tx.try_send(Message::ReturnControl { edge: exit });
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
                if let Err(e) = clipboard::set_text(&text) {
                    warn!("clipboard set failed: {e}");
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
        if !handle_msg(msg, &mut edge, &mut mouse, &mut keyboard, &out_tx)? {
            return Ok(());
        }
    }

    // ── Event loop ────────────────────────────────────────────────────────────
    loop {
        let msg = receiver.receive().await?;
        if !handle_msg(msg, &mut edge, &mut mouse, &mut keyboard, &out_tx)? {
            break;
        }
    }
    Ok(())
}
