//! Outbound connection — controller side.
//! Phases 1-7: identity, pairing, edge switching, clipboard sync.

use anyhow::Context;
use manguesechee_core::events::InputEvent;
use manguesechee_core::protocol::Message;
use manguesechee_input::{find_keyboard, find_mouse, KeyboardCapture, MouseCapture};
use manguesechee_network::{connect, Transport};
use std::path::PathBuf;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::clipboard;
use crate::edge::EdgeDetector;
use crate::session::{generate_code, load_known_peers, save_known_peers, show_outgoing_code};

#[derive(Debug, PartialEq)]
enum ControllerState { Local, Forwarding }

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

    let (mut sender, mut receiver) = transport.into_split();

    // ── Devices ───────────────────────────────────────────────────────────────
    let mouse_path = mouse_path
        .map(Ok).unwrap_or_else(|| find_mouse().context("auto-detect mouse"))?;
    let keyboard_path = keyboard_path
        .map(Ok).unwrap_or_else(|| find_keyboard().context("auto-detect keyboard"))?;

    let pressed = KeyboardCapture::snapshot_pressed(&keyboard_path).unwrap_or_default();
    sender.send(&Message::InputEvent(InputEvent::KeySync { pressed_keys: pressed })).await?;

    // ── Capture tasks ─────────────────────────────────────────────────────────
    let (ev_tx, mut ev_rx)                       = mpsc::channel::<InputEvent>(512);
    let (grab_mouse_tx, mut grab_mouse_rx)        = mpsc::channel::<bool>(4);
    let (grab_keyboard_tx, mut grab_keyboard_rx)  = mpsc::channel::<bool>(4);

    {
        let tx = ev_tx.clone(); let path = mouse_path.clone();
        tokio::spawn(async move {
            match MouseCapture::open(&path) {
                Err(e) => warn!("mouse: {e:#}"),
                Ok(mut cap) => loop {
                    tokio::select! {
                        Some(g) = grab_mouse_rx.recv() => {
                            if let Err(e) = if g { cap.grab() } else { cap.ungrab() } { warn!("mouse grab: {e}"); }
                        }
                        ev = cap.next_event() => match ev {
                            Ok(ev) => { let _ = tx.send(ev).await; }
                            Err(e) => { warn!("mouse read: {e}"); break; }
                        }
                    }
                },
            }
        });
    }
    {
        let tx = ev_tx.clone(); let path = keyboard_path.clone();
        tokio::spawn(async move {
            match KeyboardCapture::open(&path) {
                Err(e) => warn!("keyboard: {e:#}"),
                Ok(mut cap) => loop {
                    tokio::select! {
                        Some(g) = grab_keyboard_rx.recv() => {
                            if let Err(e) = if g { cap.grab() } else { cap.ungrab() } { warn!("keyboard grab: {e}"); }
                        }
                        ev = cap.next_event() => match ev {
                            Ok(ev) => { let _ = tx.send(ev).await; }
                            Err(e) => { warn!("keyboard read: {e}"); break; }
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
    let (clip_active_tx, clip_active_rx) = mpsc::channel::<bool>(4);
    clipboard::spawn_watcher(clip_msg_tx, clip_active_rx);

    // ── ReturnControl listener ────────────────────────────────────────────────
    let (return_tx, mut return_rx) = mpsc::channel::<()>(4);
    tokio::spawn(async move {
        loop {
            match receiver.receive().await {
                Ok(Message::ReturnControl { edge }) => {
                    info!("← ReturnControl ({edge:?})");
                    let _ = return_tx.send(()).await;
                }
                Ok(Message::Goodbye) | Err(_) => break,
                Ok(other) => warn!("unexpected: {other:?}"),
            }
        }
    });

    // ── State machine ─────────────────────────────────────────────────────────
    let mut state = ControllerState::Local;
    let initial_locked = ipc_state.lock().unwrap().cursor_locked;
    let mut edge  = EdgeDetector::new(screen_width, screen_height)
        .with_settings(deadzone_px, delay_ms, initial_locked);
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
                            if let Some(crossed) = edge.update(*dx, *dy) {
                                info!("→ EdgeCrossed ({crossed:?}) — grabbing");
                                let _ = grab_mouse_tx.send(true).await;
                                let _ = grab_keyboard_tx.send(true).await;
                                let _ = clip_active_tx.send(true).await;
                                sender.send(&Message::EdgeCrossed { edge: crossed }).await?;
                                state = ControllerState::Forwarding;
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
                if state == ControllerState::Forwarding {
                    sender.send(&clip_msg).await?;
                }
            }

            // Peer returning control
            Some(_) = return_rx.recv() => {
                info!("← ReturnControl — ungrabbing, back to local");
                let _ = grab_mouse_tx.send(false).await;
                let _ = grab_keyboard_tx.send(false).await;
                let _ = clip_active_tx.send(false).await;
                state = ControllerState::Local;
                edge  = EdgeDetector::new(screen_width, screen_height);
            }
        }
    }

    sender.send(&Message::Goodbye).await?;
    Ok(())
}
