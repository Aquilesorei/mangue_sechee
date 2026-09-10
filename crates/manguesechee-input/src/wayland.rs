//! Wayland-specific backend helpers.
//!
//! COSMIC runs a Wayland-native compositor. For the agent running on COSMIC:
//!
//! ── Input capture ────────────────────────────────────────────────────────────
//! evdev + EVIOCGRAB works on COSMIC just as on X11 — device nodes are
//! kernel-level and the compositor does not intercept them.
//! No special Wayland protocol is needed for the capture path.
//!
//! ── Input injection ──────────────────────────────────────────────────────────
//! uinput is also kernel-level and works on COSMIC unchanged.
//!
//! ── Pointer lock (Phase 4.1 alternative) ─────────────────────────────────────
//! On pure Wayland, `zwp_pointer_constraints_v1` can lock the pointer to a
//! surface. This is compositor-side and complementary to EVIOCGRAB.
//! We skip this for now because EVIOCGRAB alone is sufficient for the agent
//! (we read raw events before the compositor, so the compositor's cursor
//! simply stops receiving relative deltas while grabbed).
//!
//! ── Screen dimensions ────────────────────────────────────────────────────────
//! `detect_screen_size()` below reads the primary output geometry from
//! /sys/class/drm without requiring a Wayland connection or X11.
//! This is the canonical way to get screen size in a headless or pre-display
//! context on Linux.
//!
//! ── Clipboard ────────────────────────────────────────────────────────────────
//! arboard uses smithay-clipboard on Wayland, which calls `wl-copy`/`wl-paste`
//! from the `wl-clipboard` package. Ensure `wl-clipboard` is installed:
//!   sudo apt install wl-clipboard   # Debian/Ubuntu
//!   sudo dnf install wl-clipboard   # Fedora / COSMIC
//!   sudo pacman -S wl-clipboard     # Arch

use tracing::{info, warn};

/// Attempt to detect the primary screen resolution from the DRM subsystem.
/// Falls back to `(1920, 1080)` if detection fails.
///
/// This works on Wayland, X11, and even TTY sessions because it reads
/// the kernel's DRM connector state directly.
/// Attempt to detect the primary screen resolution from DRM or display compositor.
/// Returns `Some((width, height))` on success, or `None` if detection fails.
pub fn try_detect_screen_size() -> Option<(u32, u32)> {
    // 1. Try reading DRM connector states directly
    if let Ok(drm) = std::fs::read_dir("/sys/class/drm") {
        for entry in drm.filter_map(|e| e.ok()) {
            let p = entry.path();
            if !p.is_dir() {
                continue;
            }
            // Check status file — must exist and be "connected"
            let status_path = p.join("status");
            if !status_path.exists() {
                continue;
            }
            if let Ok(status) = std::fs::read_to_string(&status_path) {
                if status.trim() != "connected" {
                    continue;
                }
            } else {
                continue;
            }
            let modes_path = p.join("modes");
            if let Ok(content) = std::fs::read_to_string(&modes_path) {
                for line in content.lines() {
                    if let Some((w, h)) = parse_mode(line) {
                        if w > 0 && h > 0 {
                            info!("detected display geometry from DRM ({:?}): {}×{}", p.file_name().unwrap_or_default(), w, h);
                            return Some((w, h));
                        }
                    }
                }
            }
        }
    }

    // 2. Fallback via kscreen-doctor (KDE Plasma)
    if let Ok(output) = std::process::Command::new("kscreen-doctor").arg("-o").output() {
        if output.status.success() {
            let s = String::from_utf8_lossy(&output.stdout);
            if let Some((w, h)) = parse_kscreen_doctor_output(&s) {
                info!("detected display geometry from kscreen-doctor: {}×{}", w, h);
                return Some((w, h));
            }
        }
    }

    // 3. Fallback via wlr-randr (COSMIC, Sway, wlroots)
    if let Ok(output) = std::process::Command::new("wlr-randr").output() {
        if output.status.success() {
            let s = String::from_utf8_lossy(&output.stdout);
            for line in s.lines() {
                let trimmed = line.trim();
                if trimmed.contains("(current)") || trimmed.contains("preferred") {
                    if let Some(mode_str) = trimmed.split_whitespace().next() {
                        if let Some(dims) = parse_mode(mode_str) {
                            info!("detected display geometry from wlr-randr: {}×{}", dims.0, dims.1);
                            return Some(dims);
                        }
                    }
                }
            }
        }
    }

    // 4. Fallback via xrandr (X11 / Xwayland)
    if let Ok(output) = std::process::Command::new("xrandr").output() {
        if output.status.success() {
            let s = String::from_utf8_lossy(&output.stdout);
            for line in s.lines() {
                if line.contains(" connected") {
                    for token in line.split_whitespace() {
                        if token.contains('x') {
                            let clean = token.split('+').next().unwrap_or(token);
                            if let Some(dims) = parse_mode(clean) {
                                info!("detected display geometry from xrandr: {}×{}", dims.0, dims.1);
                                return Some(dims);
                            }
                        }
                    }
                }
            }
        }
    }

    None
}

/// Detect screen resolution with fallback to 1920x1080 if detection fails.
pub fn detect_screen_size() -> (u32, u32) {
    match try_detect_screen_size() {
        Some(dims) => dims,
        None => {
            warn!("could not detect screen size from display subsystem — using 1920×1080 default");
            (1920, 1080)
        }
    }
}

fn parse_kscreen_doctor_output(s: &str) -> Option<(u32, u32)> {
    for line in s.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Modes:") {
            let modes_part = &trimmed["Modes:".len()..];
            // Prefer current mode marked with '*'
            for token in modes_part.split_whitespace() {
                if token.contains('*') {
                    if let Some(dims) = parse_kscreen_token(token) {
                        return Some(dims);
                    }
                }
            }
            // Fallback: take first valid mode token
            for token in modes_part.split_whitespace() {
                if let Some(dims) = parse_kscreen_token(token) {
                    return Some(dims);
                }
            }
        }
    }
    None
}

fn parse_kscreen_token(token: &str) -> Option<(u32, u32)> {
    // Format: "1:3072x1920@60.14*!" or "*1:3072x1920"
    let clean = token.trim_matches(|c: char| !c.is_alphanumeric() && c != ':' && c != '@' && c != 'x' && c != '.');
    let mode_str = clean.split(':').nth(1).unwrap_or(clean);
    let resolution = mode_str.split('@').next().unwrap_or(mode_str);
    parse_mode(resolution)
}

fn parse_mode(s: &str) -> Option<(u32, u32)> {
    // Format: "WIDTHxHEIGHT" e.g. "1920x1080" or "2560x1440i"
    let s = s.trim().trim_end_matches('i'); // strip interlaced suffix
    let mut parts = s.split('x');
    let w = parts.next()?.parse::<u32>().ok()?;
    let h = parts.next()?.parse::<u32>().ok()?;
    Some((w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_mode() {
        assert_eq!(parse_mode("1920x1080"), Some((1920, 1080)));
        assert_eq!(parse_mode("2560x1440"), Some((2560, 1440)));
        assert_eq!(parse_mode("1024x768i"), Some((1024, 768)));
        assert_eq!(parse_mode("invalid"),   None);
    }

    #[test]
    fn test_parse_kscreen_doctor() {
        let sample = "Output: 1 eDP-1\n\tenabled\n\tModes:  1:3072x1920@60.14*!  2:3072x1920@40.09  3:1600x1200@59.87\n\tGeometry: 0,0 1536x960";
        assert_eq!(parse_kscreen_doctor_output(sample), Some((3072, 1920)));
    }

    #[test]
    fn test_detect_screen_size() {
        let (w, h) = super::detect_screen_size();
        println!("Detected screen size: {w}x{h}");
        assert!(w > 0 && h > 0);
    }
}
