//! File clipboard parsing and injection.
//!
//! Translates between Linux file manager clipboard formats (`text/uri-list`,
//! `x-special/gnome-copied-files`) and local disk paths.

use manguesechee_core::protocol::FileInfo;
use std::path::{Path, PathBuf};
use tracing::info;

/// Decode percent-encoded URI strings (e.g. `%20` -> space).
pub fn percent_decode(s: &str) -> String {
    let mut bytes = Vec::with_capacity(s.len());
    let s_bytes = s.as_bytes();
    let mut i = 0;
    while i < s_bytes.len() {
        if s_bytes[i] == b'%' && i + 2 < s_bytes.len() {
            if let Ok(b) = u8::from_str_radix(std::str::from_utf8(&s_bytes[i + 1..i + 3]).unwrap_or(""), 16) {
                bytes.push(b);
                i += 3;
                continue;
            }
        }
        bytes.push(s_bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&bytes).to_string()
}

/// Detect and extract local file paths if the clipboard text represents copied files.
/// Returns None if the text does not contain valid existing local file URIs.
pub fn parse_clipboard_file_uris(text: &str) -> Option<Vec<PathBuf>> {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if lines.is_empty() {
        return None;
    }

    // Must start with "file://" or "copy" / "cut" header from GNOME/Nautilus
    let mut paths = Vec::new();
    for line in lines {
        if line == "copy" || line == "cut" {
            continue;
        }
        if let Some(stripped) = line.strip_prefix("file://") {
            let decoded = percent_decode(stripped);
            let path = PathBuf::from(decoded);
            if path.exists() {
                paths.push(path);
            } else {
                // If a referenced path does not exist, treat as regular text
                return None;
            }
        } else {
            // Non-URI line encountered (unless comment)
            if line.starts_with('#') {
                continue;
            }
            return None;
        }
    }

    if paths.is_empty() {
        None
    } else {
        Some(paths)
    }
}

/// Recursively gather file entries and total size for file transfer.
pub fn collect_file_entries(paths: &[PathBuf]) -> (Vec<FileInfo>, Vec<PathBuf>, u64) {
    let mut file_infos = Vec::new();
    let mut disk_paths = Vec::new();
    let mut total_size = 0u64;

    for root in paths {
        if root.is_file() {
            let size = root.metadata().map(|m| m.len()).unwrap_or(0);
            let name = root.file_name().unwrap_or_default().to_string_lossy().to_string();
            file_infos.push(FileInfo {
                filename: name,
                size,
                relative_path: None,
            });
            disk_paths.push(root.clone());
            total_size += size;
        } else if root.is_dir() {
            let root_name = root.file_name().unwrap_or_default().to_string_lossy().to_string();
            collect_dir_entries(root, &root_name, &mut file_infos, &mut disk_paths, &mut total_size);
        }
    }

    (file_infos, disk_paths, total_size)
}

fn collect_dir_entries(
    dir: &Path,
    rel_prefix: &str,
    infos: &mut Vec<FileInfo>,
    paths: &mut Vec<PathBuf>,
    total_size: &mut u64,
) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let file_name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
            let rel = format!("{rel_prefix}/{file_name}");
            if path.is_file() {
                let size = path.metadata().map(|m| m.len()).unwrap_or(0);
                infos.push(FileInfo {
                    filename: file_name,
                    size,
                    relative_path: Some(rel),
                });
                paths.push(path);
                *total_size += size;
            } else if path.is_dir() {
                collect_dir_entries(&path, &rel, infos, paths, total_size);
            }
        }
    }
}

/// Get destination directory in user cache for receiving files.
pub fn staging_dir(transfer_id: &str) -> PathBuf {
    let base = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from(format!("/tmp/manguesechee-{}", unsafe { libc::getuid() })))
        .join("manguesechee")
        .join("staged");
    let _ = std::fs::create_dir_all(&base);
    base.join(transfer_id)
}

/// Clean old staged transfers, keeping only recent ones.
pub fn clean_old_staged_dirs() {
    let base = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from(format!("/tmp/manguesechee-{}", unsafe { libc::getuid() })))
        .join("manguesechee")
        .join("staged");
    if let Ok(entries) = std::fs::read_dir(&base) {
        let mut dirs: Vec<_> = entries.flatten().filter(|e| e.path().is_dir()).collect();
        // Keep up to 5 most recent transfer dirs
        if dirs.len() > 5 {
            dirs.sort_by_key(|e| e.metadata().and_then(|m| m.modified()).ok());
            for entry in dirs.iter().take(dirs.len().saturating_sub(5)) {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
}

/// Set the local clipboard to point to local staged files so Nautilus/Dolphin/COSMIC paste them.
pub fn set_file_clipboard(paths: &[PathBuf]) -> anyhow::Result<()> {
    crate::clipboard::ensure_display_env();

    let mut uri_list = String::new();
    let mut gnome_copied = String::from("copy\n");
    for p in paths {
        let uri = format!("file://{}\r\n", p.display());
        uri_list.push_str(&uri);
        gnome_copied.push_str(&format!("file://{}\n", p.display()));
    }

    crate::clipboard::mark_synced(&uri_list);
    let mut success = false;

    // 1. Wayland wl-copy with text/uri-list and x-special/gnome-copied-files
    if std::env::var("WAYLAND_DISPLAY").is_ok() {
        if let Ok(mut child) = std::process::Command::new("wl-copy")
            .args(["-t", "text/uri-list"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                use std::io::Write;
                let _ = stdin.write_all(uri_list.as_bytes());
                drop(stdin);
                let _ = child.wait();
                success = true;
            }
        }
    }

    // 2. X11 xclip with text/uri-list
    if std::env::var("DISPLAY").is_ok() {
        if let Ok(mut child) = std::process::Command::new("xclip")
            .args(["-selection", "clipboard", "-t", "text/uri-list"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                use std::io::Write;
                let _ = stdin.write_all(uri_list.as_bytes());
                drop(stdin);
                let _ = child.wait();
                success = true;
            }
        }
    }

    // 3. Fallback arboard plain text
    let _ = crate::clipboard::set_text(&uri_list);

    if success {
        info!("local file clipboard set ({} files)", paths.len());
        Ok(())
    } else {
        anyhow::bail!("failed to set file clipboard via wl-copy or xclip")
    }
}

/// Show desktop notification using standard `notify-send`.
pub fn show_notification(title: &str, body: &str) {
    let _ = std::process::Command::new("notify-send")
        .args(["-a", "Manguesechee", "-i", "manguesechee", title, body])
        .spawn();
}

/// Format bytes into human-readable string (e.g. "14.2 MB").
pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}
