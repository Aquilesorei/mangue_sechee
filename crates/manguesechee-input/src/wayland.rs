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
pub fn detect_screen_size() -> (u32, u32) {
    match try_detect_screen_size() {
        Some(dims) => {
            info!("screen size detected from DRM: {}×{}", dims.0, dims.1);
            dims
        }
        None => {
            warn!("could not detect screen size from DRM — using 1920×1080");
            (1920, 1080)
        }
    }
}

fn try_detect_screen_size() -> Option<(u32, u32)> {
    // /sys/class/drm/card*/card*-*/modes contains newline-separated mode strings
    // like "1920x1080" or "2560x1440". The first line is the preferred mode.
    let drm = std::fs::read_dir("/sys/class/drm").ok()?;

    for entry in drm.filter_map(|e| e.ok()) {
        let path = entry.path().join("modes");
        if !path.exists() {
            continue;
        }
        let content = std::fs::read_to_string(&path).ok()?;
        let first_line = content.lines().next()?;
        if let Some((w, h)) = parse_mode(first_line) {
            return Some((w, h));
        }
    }
    None
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
    use super::parse_mode;

    #[test]
    fn test_parse_mode() {
        assert_eq!(parse_mode("1920x1080"), Some((1920, 1080)));
        assert_eq!(parse_mode("2560x1440"), Some((2560, 1440)));
        assert_eq!(parse_mode("1024x768i"), Some((1024, 768)));
        assert_eq!(parse_mode("invalid"),   None);
    }
}
