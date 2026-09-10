//! Clipboard sync.
//!
//! Controller side: polls the local clipboard every 250 ms.
//! When the content changes it sends Message::ClipboardSync to the peer.
//!
//! Controlled side: on receiving ClipboardSync, writes the content into
//! the local clipboard.
//!
//! arboard works on both X11 and Wayland (uses xclip/wl-clipboard backends).

use anyhow::Context;
use arboard::Clipboard;
use manguesechee_core::protocol::Message;
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// Spawn a background task that watches the clipboard and sends
/// `Message::ClipboardSync` on `tx` whenever the text content changes.
///
/// `active_rx`: receives `true` when entering forwarding mode, `false` on return.
/// Clipboard events are only sent while active.
pub fn spawn_watcher(
    tx:        mpsc::Sender<Message>,
    mut active_rx: mpsc::Receiver<bool>,
) {
    std::thread::spawn(move || {
        let mut clipboard = match Clipboard::new() {
            Ok(c)  => c,
            Err(e) => { warn!("clipboard unavailable: {e}"); return; }
        };

        let mut last_text = clipboard.get_text().unwrap_or_default();
        let mut active    = false;

        loop {
            // Drain active-state updates
            while let Ok(a) = active_rx.try_recv() {
                active = a;
                debug!("clipboard watcher: active={active}");
            }

            if active {
                match clipboard.get_text() {
                    Ok(text) if text != last_text => {
                        last_text = text.clone();
                        let msg = Message::ClipboardSync { text };
                        if tx.blocking_send(msg).is_err() {
                            break; // channel closed, stop
                        }
                    }
                    _ => {}
                }
            }

            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    });
}

/// Write text into the local clipboard.
pub fn set_text(text: &str) -> anyhow::Result<()> {
    let mut clipboard = Clipboard::new().context("open clipboard")?;
    clipboard.set_text(text).context("set clipboard text")?;
    Ok(())
}
