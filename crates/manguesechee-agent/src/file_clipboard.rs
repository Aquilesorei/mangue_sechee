
use manguesechee_core::protocol::FileInfo;
use std::path::{Path, PathBuf};
use tracing::info;

/// Encode a local filesystem path to an RFC 2483 / RFC 3986 percent-encoded file URI.
pub fn path_to_file_uri(path: &Path) -> String {
    let bytes = path.as_os_str().as_encoded_bytes();
    let mut uri = String::from("file://");
    for &b in bytes {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                uri.push(b as char);
            }
            _ => {
                use std::fmt::Write;
                let _ = write!(uri, "%{:02X}", b);
            }
        }
    }
    uri
}

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
/// Accepts:
/// - RFC 2483 file URIs (`file:///path` or `file://localhost/path`), percent-encoded or unencoded
/// - Raw absolute filesystem paths (`/path/to/file`) from modern desktop environments
/// - GNOME/Nautilus clipboard headers (`copy\n...`, `cut\n...`)
/// Returns None if the text does not represent valid existing local files.
pub fn parse_clipboard_file_uris(text: &str) -> Option<Vec<PathBuf>> {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if lines.is_empty() {
        return None;
    }

    let mut paths = Vec::new();
    for line in lines {
        if line == "copy" || line == "cut" {
            continue;
        }
        if line.starts_with('#') {
            continue;
        }

        // Strip surrounding quotes if present (e.g. "/path/to/file")
        let line_clean = line.trim_matches(|c| c == '"' || c == '\'');

        if let Some(stripped) = line_clean.strip_prefix("file://") {
            let without_host = if let Some(after_localhost) = stripped.strip_prefix("localhost") {
                after_localhost
            } else {
                stripped
            };
            let decoded = percent_decode(without_host);
            let path = PathBuf::from(decoded);
            if path.exists() {
                paths.push(path);
            } else {
                // If a referenced path does not exist, treat as regular text
                return None;
            }
        } else if line_clean.starts_with('/') {
            let path = PathBuf::from(line_clean);
            if path.exists() {
                paths.push(path);
            } else {
                return None;
            }
        } else {
            // Line does not match any file or URI pattern
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
            if let Ok(ft) = entry.file_type() {
                if ft.is_symlink() && path.is_dir() {
                    continue;
                }
            }
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
    for p in paths {
        let uri = path_to_file_uri(p);
        uri_list.push_str(&uri);
        uri_list.push_str("\r\n");
    }

    crate::clipboard::mark_synced(&uri_list);
    let mut success = false;

    let mut wayland_copied = false;

    // 1. Wayland wl-copy with text/uri-list
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
                if let Ok(st) = child.wait() {
                    if st.success() {
                        wayland_copied = true;
                        success = true;
                    }
                }
            }
        }
    }

    // 2. X11 xclip with text/uri-list
    // CRITICAL: NEVER run xclip if wl-copy succeeded!
    // In Wayland sessions with Xwayland (like Pop!_OS and Fedora), running xclip
    // makes Xwayland claim the Wayland clipboard, terminating wl-copy and advertising
    // text/plain to native Wayland apps, which causes COSMIC Files and Dolphin
    // to paste a text file instead of the actual file!
    if !wayland_copied && std::env::var("DISPLAY").is_ok() {
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
                if let Ok(st) = child.wait() {
                    if st.success() {
                        success = true;
                    }
                }
            }
        }
    }

    // CRITICAL: If wl-copy or xclip succeeded, return immediately!
    // NEVER call crate::clipboard::set_text(&uri_list) when wl-copy / xclip succeeds.
    // In Wayland and X11, calling set_text creates a new text/plain clipboard selection
    // which immediately overwrites and terminates the text/uri-list data source, causing
    // COSMIC Files and Dolphin to paste a text file instead of the actual file!
    if success {
        info!("local file clipboard set ({} files)", paths.len());
        Ok(())
    } else {
        // Fallback arboard plain text ONLY if wl-copy / xclip were unavailable
        let _ = crate::clipboard::set_text(&uri_list);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_to_file_uri() {
        let path = Path::new("/home/user/test.txt");
        assert_eq!(path_to_file_uri(path), "file:///home/user/test.txt");

        let space_path = Path::new("/home/user/my test file.png");
        assert_eq!(path_to_file_uri(space_path), "file:///home/user/my%20test%20file.png");
    }

    #[test]
    fn test_percent_decode() {
        assert_eq!(percent_decode("/home/user/my%20file.txt"), "/home/user/my file.txt");
        assert_eq!(percent_decode("%2Ftmp%2Ftest"), "/tmp/test");
    }

    #[test]
    fn test_parse_clipboard_file_uris() {
        let temp_path = std::env::temp_dir().join(format!("mangue_test_{}.txt", std::process::id()));
        std::fs::write(&temp_path, "hello").unwrap();
        let path_str = temp_path.to_str().unwrap();

        // Standard RFC 2483 URI
        let uri_text = format!("file://{path_str}\r\n");
        let parsed = parse_clipboard_file_uris(&uri_text);
        assert!(parsed.is_some());
        assert_eq!(parsed.unwrap()[0], PathBuf::from(path_str));

        // Raw path
        let raw_text = format!("{path_str}\n");
        let parsed_raw = parse_clipboard_file_uris(&raw_text);
        assert!(parsed_raw.is_some());
        assert_eq!(parsed_raw.unwrap()[0], PathBuf::from(path_str));

        // Non-existent path
        let fake = "file:///nonexistent/path/for/sure/12345.txt";
        assert!(parse_clipboard_file_uris(fake).is_none());

        // Plain text
        let text = "just some random text without paths";
        assert!(parse_clipboard_file_uris(text).is_none());

        let _ = std::fs::remove_file(temp_path);
    }
}
