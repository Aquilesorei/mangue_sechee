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
        *s = text.trim().to_string();
    }
}

/// Check if the given text matches the last synchronized content.
pub fn is_already_synced(text: &str) -> bool {
    if let Ok(s) = LAST_SYNCED.lock() {
        *s == text.trim()
    } else {
        false
    }
}

/// Ensure environment variables required for Wayland and X11 clipboard access are set.
/// When running under a systemd --user service, DISPLAY, WAYLAND_DISPLAY, or XAUTHORITY
/// may not have been imported into the service manager's environment.
pub fn ensure_display_env() {
    let uid = unsafe { libc::getuid() };

    // 1. Ensure XDG_RUNTIME_DIR is set
    let runtime_dir = match std::env::var("XDG_RUNTIME_DIR") {
        Ok(dir) if !dir.is_empty() => std::path::PathBuf::from(dir),
        _ => {
            let dir = format!("/run/user/{}", uid);
            let p = std::path::PathBuf::from(&dir);
            if p.exists() {
                std::env::set_var("XDG_RUNTIME_DIR", &dir);
                p
            } else {
                std::path::PathBuf::from(format!("/tmp/manguesechee-{}", uid))
            }
        }
    };

    // 2. Ensure WAYLAND_DISPLAY is set if a Wayland socket exists
    if std::env::var("WAYLAND_DISPLAY").is_err() {
        for i in 0..5 {
            let name = if i == 0 {
                "wayland-0".to_string()
            } else {
                format!("wayland-{}", i)
            };
            if runtime_dir.join(&name).exists() {
                debug!("auto-detected WAYLAND_DISPLAY={name}");
                std::env::set_var("WAYLAND_DISPLAY", &name);
                break;
            }
        }
    }

    // 3. Ensure DISPLAY is set if an X11 socket exists
    if std::env::var("DISPLAY").is_err() {
        for i in 0..5 {
            let x_socket = format!("/tmp/.X11-unix/X{}", i);
            if std::path::Path::new(&x_socket).exists() {
                let disp = format!(":{}", i);
                debug!("auto-detected DISPLAY={disp}");
                std::env::set_var("DISPLAY", &disp);
                break;
            }
        }
    }

    // 4. Ensure XAUTHORITY is set for X11 authentication
    if std::env::var("XAUTHORITY").is_err() {
        if let Some(home) = dirs::home_dir() {
            let xauth = home.join(".Xauthority");
            if xauth.exists() {
                std::env::set_var("XAUTHORITY", xauth.to_string_lossy().as_ref());
            }
        }
        if std::env::var("XAUTHORITY").is_err() && runtime_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&runtime_dir) {
                for entry in entries.flatten() {
                    let file_name = entry.file_name();
                    let name = file_name.to_string_lossy();
                    if name.starts_with("xauth_") || name.ends_with(".xauth") || name == "Xauthority" {
                        std::env::set_var("XAUTHORITY", entry.path().to_string_lossy().as_ref());
                        break;
                    }
                }
            }
        }
    }
}

/// Read text from the local clipboard across all available backends (wl-paste, arboard, xclip, xsel).
/// Prioritizes `text/uri-list` so file manager copies (COSMIC Files, Dolphin, Nautilus) are captured directly.
pub fn get_text() -> Option<String> {
    ensure_display_env();

    // 1. Wayland: Prioritize text/uri-list if available
    if std::env::var("WAYLAND_DISPLAY").is_ok() {
        if let Ok(output) = std::process::Command::new("wl-paste")
            .args(["-t", "text/uri-list", "--no-newline"])
            .output()
        {
            if output.status.success() && !output.stdout.is_empty() {
                if let Ok(text) = String::from_utf8(output.stdout) {
                    if !text.trim().is_empty() {
                        return Some(text);
                    }
                }
            }
        }

        // 1b. Wayland: Check GNOME / COSMIC Files copied files format
        if let Ok(output) = std::process::Command::new("wl-paste")
            .args(["-t", "x-special/gnome-copied-files", "--no-newline"])
            .output()
        {
            if output.status.success() && !output.stdout.is_empty() {
                if let Ok(text) = String::from_utf8(output.stdout) {
                    if !text.trim().is_empty() {
                        return Some(text);
                    }
                }
            }
        }

        // Standard text via wl-paste
        if let Ok(output) = std::process::Command::new("wl-paste")
            .arg("--no-newline")
            .output()
        {
            if output.status.success() && !output.stdout.is_empty() {
                if let Ok(text) = String::from_utf8(output.stdout) {
                    if !text.is_empty() {
                        return Some(text);
                    }
                }
            }
        }
    }

    // 2. Native arboard
    if let Ok(mut guard) = CLIPBOARD.lock() {
        if guard.is_none() {
            *guard = Clipboard::new().ok();
        }
        if let Some(cb) = guard.as_mut() {
            if let Ok(text) = cb.get_text() {
                if !text.is_empty() {
                    return Some(text);
                }
            }
            // CRITICAL: Do NOT re-create `*guard = Clipboard::new().ok()` on Err!
            // In arboard/X11, cb holds ownership of the selection. Recreating Clipboard
            // closes the X11 connection window, destroying any active selection.
        }
    }

    // 3. X11 fallback via xclip: Prioritize text/uri-list and gnome-copied-files if available
    if std::env::var("DISPLAY").is_ok() {
        if let Ok(output) = std::process::Command::new("xclip")
            .args(["-selection", "clipboard", "-t", "text/uri-list", "-o"])
            .output()
        {
            if output.status.success() && !output.stdout.is_empty() {
                if let Ok(text) = String::from_utf8(output.stdout) {
                    if !text.trim().is_empty() {
                        return Some(text);
                    }
                }
            }
        }

        if let Ok(output) = std::process::Command::new("xclip")
            .args(["-selection", "clipboard", "-t", "x-special/gnome-copied-files", "-o"])
            .output()
        {
            if output.status.success() && !output.stdout.is_empty() {
                if let Ok(text) = String::from_utf8(output.stdout) {
                    if !text.trim().is_empty() {
                        return Some(text);
                    }
                }
            }
        }

        if let Ok(output) = std::process::Command::new("xclip")
            .args(["-selection", "clipboard", "-o"])
            .output()
        {
            if output.status.success() && !output.stdout.is_empty() {
                if let Ok(text) = String::from_utf8(output.stdout) {
                    if !text.is_empty() {
                        return Some(text);
                    }
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
                    if !text.is_empty() {
                        return Some(text);
                    }
                }
            }
        }
    }

    None
}

/// Write text into the local clipboard and keep it available for pasting across all applications.
pub fn set_text(text: &str) -> anyhow::Result<()> {
    ensure_display_env();
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
    let mut wayland_copied = false;
    if std::env::var("WAYLAND_DISPLAY").is_ok() {
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
                if let Ok(st) = child.wait() {
                    if st.success() {
                        wayland_copied = true;
                        success = true;
                    }
                }
            }
        }
    }

    // 3. X11 fallback via xclip & xsel
    // CRITICAL: On Wayland sessions with Xwayland, running both wl-copy and xclip
    // simultaneously causes an Xwayland selection race that supersedes wl-copy.
    // Only invoke xclip/xsel if Wayland is not active or wl-copy was not successful.
    if !wayland_copied && std::env::var("DISPLAY").is_ok() {
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

            std::thread::sleep(Duration::from_millis(750));
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
