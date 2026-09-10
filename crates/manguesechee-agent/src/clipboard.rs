//! Clipboard sync.
//!
//! Provides bidirectional clipboard synchronization between controller and peer.
//! Supports Wayland (via arboard with wayland-data-control and wl-clipboard) and X11.
//!
//! Automatically avoids echo/feedback loops by tracking the last synchronized content.

use arboard::Clipboard;
use manguesechee_core::protocol::Message;
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, info};

static CLIPBOARD: Mutex<Option<Clipboard>> = Mutex::new(None);
static LAST_SYNCED: Mutex<String> = Mutex::new(String::new());

/// Max clipboard text payload (5 MB) to avoid network saturation
const MAX_CLIPBOARD_BYTES: usize = 5 * 1024 * 1024;

/// Mark text as already synchronized so our local watcher doesn't echo it back.
pub fn mark_synced(text: &str) {
    if let Ok(mut s) = LAST_SYNCED.lock() {
        *s = text.to_string();
    }
}

/// Check if the given text matches the last synchronized content.
pub fn is_already_synced(text: &str) -> bool {
    if let Ok(s) = LAST_SYNCED.lock() {
        *s == text
    } else {
        false
    }
}

/// Read text from the local clipboard across all available backends (arboard, wl-paste, xclip, xsel).
pub fn get_text() -> Option<String> {
    // 1. Try native arboard
    if let Ok(mut guard) = CLIPBOARD.lock() {
        if guard.is_none() {
            *guard = Clipboard::new().ok();
        }
        if let Some(cb) = guard.as_mut() {
            if let Ok(text) = cb.get_text() {
                if !text.is_empty() {
                    return Some(text);
                }
            } else {
                // Re-initialize if compositor connection was reset
                *guard = Clipboard::new().ok();
                if let Some(cb) = guard.as_mut() {
                    if let Ok(text) = cb.get_text() {
                        if !text.is_empty() {
                            return Some(text);
                        }
                    }
                }
            }
        }
    }

    // 2. Wayland fallback via wl-paste (standard across KDE, COSMIC, GNOME, Sway)
    if let Ok(output) = std::process::Command::new("wl-paste")
        .arg("--no-newline")
        .output()
    {
        if output.status.success() && !output.stdout.is_empty() {
            if let Ok(text) = String::from_utf8(output.stdout) {
                return Some(text);
            }
        }
    }

    // 3. X11 fallback via xclip
    if let Ok(output) = std::process::Command::new("xclip")
        .args(["-selection", "clipboard", "-o"])
        .output()
    {
        if output.status.success() && !output.stdout.is_empty() {
            if let Ok(text) = String::from_utf8(output.stdout) {
                return Some(text);
            }
        }
    }

    // 4. X11 fallback via xsel
    if let Ok(output) = std::process::Command::new("xsel")
        .args(["--clipboard", "--output"])
        .output()
    {
        if output.status.success() && !output.stdout.is_empty() {
            if let Ok(text) = String::from_utf8(output.stdout) {
                return Some(text);
            }
        }
    }

    None
}

/// Write text into the local clipboard and keep it available for pasting across all applications.
pub fn set_text(text: &str) -> anyhow::Result<()> {
    mark_synced(text);
    let mut success = false;

    // 1. Persistent native arboard
    if let Ok(mut guard) = CLIPBOARD.lock() {
        if guard.is_none() {
            *guard = Clipboard::new().ok();
        }
        if let Some(cb) = guard.as_mut() {
            if cb.set_text(text).is_ok() {
                success = true;
            } else {
                *guard = Clipboard::new().ok();
                if let Some(cb) = guard.as_mut() {
                    if cb.set_text(text).is_ok() {
                        success = true;
                    }
                }
            }
        }
    }

    // 2. Wayland native fallback via wl-copy
    // wl-copy forks into background and retains the selection across app switches
    if let Ok(mut child) = std::process::Command::new("wl-copy")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            let _ = stdin.write_all(text.as_bytes());
            drop(stdin);
            let _ = child.wait();
            success = true;
        }
    }

    // 3. X11 fallback via xclip
    if let Ok(mut child) = std::process::Command::new("xclip")
        .args(["-selection", "clipboard"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            let _ = stdin.write_all(text.as_bytes());
            drop(stdin);
            let _ = child.wait();
            success = true;
        }
    }

    // 4. X11 fallback via xsel
    if let Ok(mut child) = std::process::Command::new("xsel")
        .args(["--clipboard", "--input"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            let _ = stdin.write_all(text.as_bytes());
            drop(stdin);
            let _ = child.wait();
            success = true;
        }
    }

    if success {
        info!("local clipboard updated ({} bytes)", text.len());
        Ok(())
    } else {
        anyhow::bail!("failed to write clipboard via any backend")
    }
}

/// Spawn a continuous background watcher that monitors local clipboard changes
/// and sends `Message::ClipboardSync` over `tx`. Automatically stops when `tx` is closed.
pub fn spawn_watcher(tx: mpsc::Sender<Message>) {
    std::thread::spawn(move || {
        // Initialize last_seen with current clipboard content to prevent spurious startup sync
        if let Some(initial) = get_text() {
            mark_synced(&initial);
        }

        loop {
            if let Some(current) = get_text() {
                if !current.is_empty() && current.len() <= MAX_CLIPBOARD_BYTES && !is_already_synced(&current) {
                    info!("clipboard changed locally ({} bytes) -> sending ClipboardSync", current.len());
                    mark_synced(&current);
                    let msg = Message::ClipboardSync { text: current };
                    if tx.blocking_send(msg).is_err() {
                        // Channel receiver dropped, connection closed
                        debug!("clipboard watcher: channel closed, stopping");
                        break;
                    }
                }
            }

            std::thread::sleep(Duration::from_millis(300));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clipboard_rw() {
        let test_str = "manguesechee_unit_test_content";
        set_text(test_str).expect("set_text");
        assert!(is_already_synced(test_str));
        let read = get_text().expect("get_text");
        assert_eq!(read, test_str);
    }
}
