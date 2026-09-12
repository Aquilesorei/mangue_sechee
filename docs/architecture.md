# Manguesechee Architecture

Manguesechee is an ultra-low latency, zero-streaming software KVM designed natively for modern Linux desktops (COSMIC, KDE Plasma, GNOME, Sway, and Hyprland).

Unlike screen-sharing tools (VNC, RDP, Moonlight), Manguesechee does not compress or stream display frames. Instead, it captures raw hardware mouse and keyboard events on the controller via Linux `evdev` kernel interfaces, transmits them over an encrypted TLS 1.3 local network stream, and synthesizes them on the remote machine via `/dev/uinput`.

---

## 1. System Overview

```
 ┌─────────────────────────────────────────────────────────────────────────────┐
 │                            LOCAL CONTROLLER                                 │
 │                                                                             │
 │   Physical Hardware (Keyboard / Mouse)                                      │
 │            │                                                                │
 │            ▼                                                                │
 │     /dev/input/event* (evdev capture with EVIOCGRAB)                        │
 │            │                                                                │
 │            ▼                                                                │
 │   manguesechee-agent ◄──────── Unix Socket IPC ────────► manguesechee-ui    │
 │            │        ($XDG_RUNTIME_DIR/manguesechee/sock) │ (Slint Desktop)  │
 │            │                                             │ (ksni Systray)   │
 │            │                                             ▼                  │
 │            │                                     manguesechee-cli           │
 └────────────┼────────────────────────────────────────────────────────────────┘
              │
         TLS 1.3 / TCP
         Port 24800 (Framed binary protocol via bincode)
              │
 ┌────────────┼────────────────────────────────────────────────────────────────┐
 │            ▼                                                                │
 │   manguesechee-agent (Controlled Peer Daemon)                               │
 │            │                                                                │
 │            ▼                                                                │
 │       /dev/uinput (Virtual Input Injection)                                 │
 │            │                                                                │
 │            ▼                                                                │
 │   Wayland Compositor / X11 Server (Remote Desktop Target)                   │
 │                                                                             │
 │                            REMOTE CONTROLLED PEER                           │
 └─────────────────────────────────────────────────────────────────────────────┘
```

---

## 2. Workspace Crate Architecture

The codebase is organized as an idiomatic Rust cargo workspace:

| Crate | Directory | Purpose |
|---|---|---|
| [`manguesechee-core`](../crates/manguesechee-core) | `crates/manguesechee-core` | Shared data structures: network wire protocol (`bincode`), Unix IPC schemas (`serde_json`), 2D topology coordinate routing, and TOML configuration models. |
| [`manguesechee-network`](../crates/manguesechee-network) | `crates/manguesechee-network` | Asynchronous framed TCP streaming, TLS 1.3 mutual handshake and encryption (`rustls`, `rcgen`), and mDNS peer auto-discovery (`mdns-sd`). |
| [`manguesechee-input`](../crates/manguesechee-input) | `crates/manguesechee-input` | Hardware mouse and keyboard capture (`evdev`), device grabs (`EVIOCGRAB`), virtual peripheral injection (`uinput`), global hotkey parsing, and DRM display detection. |
| [`manguesechee-agent`](../crates/manguesechee-agent) | `crates/manguesechee-agent` | Core daemon orchestrating active sessions, mouse edge traversal, focus anti-fight cooldowns, inline/background file transfers, clipboard synchronization, and IPC socket server. |
| [`manguesechee-ui`](../crates/manguesechee-ui) | `crates/manguesechee-ui` | High-performance native GUI built with Slint. Features interactive 2D grid topology canvas, connection wizard, live journalctl viewer, and FreeDesktop StatusNotifierItem system tray (`ksni`). |
| [`manguesechee-cli`](../crates/manguesechee-cli) | `crates/manguesechee-cli` | Command-line control tool built with `clap v4`. Provides terminal status inspection, Waybar JSON widgets, hotkey triggers, topology configuration, and scriptable daemon controls. |

---

## 3. Core Subsystems

### 3.1 Input Capture & Device Grab (`manguesechee-input`)
- **Evdev Hardware Enumeration**: Scans `/dev/input/event*` nodes and filters for mouse (relative pointer motion, buttons) and keyboard devices.
- **Exclusive Access via `EVIOCGRAB`**: When the cursor traverses an active screen edge toward a remote machine, the controller applies `EVIOCGRAB` to the local input devices. This prevents local cursor motion or typed keystrokes from leaking into local applications while the user works remotely.
- **Virtual Input Injection (`uinput`)**: On the controlled peer, a virtual mouse (with absolute/relative axes, wheel, and buttons) and keyboard are instantiated via `/dev/uinput`. Injected events appear identically to native physical devices in any Wayland compositor (COSMIC `cosmic-comp`, KDE `kwin_wayland`, GNOME `mutter`, `sway`, `hyprland`).

### 3.2 2D Grid Topology & Spatial Navigation (`manguesechee-core`)
- **Coordinate Grid**: Screens are situated on an integer Cartesian grid $(x, y)$. The primary controller sits at `(0, 0)`. Adjacent screens occupy positions such as:
  - Left: `(-1, 0)`
  - Right: `(1, 0)`
  - Above: `(0, 1)`
  - Below: `(0, -1)`
- **Multi-Screen Stacking**: Supports arbitrarily chained monitors (e.g. `(2, 0)` for a third display to the right). Gliding the cursor off the far edge of `(1, 0)` smoothly transitions focus to `(2, 0)`.
- **Perpendicular Resolution Normalization**: When crossing an edge between displays of disparate resolutions (e.g., a 4K 2160p monitor and a 1080p laptop), the perpendicular coordinate ratio $y / H \in [0.0, 1.0]$ is preserved. The cursor enters the remote display at the identical proportional height, preventing cursor jumps or clipping.
- **Edge Resistance Dwell Delay**: Configurable resistance (`switch_delay_ms`, 0–500ms) requires the cursor to dwell at the border before switching, preventing unintended transitions when clicking close window buttons or scrollbars. Fast deliberate flicks immediately bypass the dwell timer.
- **Focus Anti-Fight Protection**: A 300ms cooldown window and center-teleportation suppress mouse jitter after manual hotkey switches (`Ctrl+Alt+Arrows`), preventing immediate accidental bounce-backs.

### 3.3 Network Transport & Security (`manguesechee-network`)
- **Framed TCP Protocol**: Packets are framed using a 4-byte length prefix followed by compact binary payloads serialized via `bincode`.
- **TLS 1.3 Encryption**: All inter-machine communications can be secured using TLS 1.3 (`rustls`). Certificates are automatically provisioned and managed on first launch using `rcgen`.
- **Mutual Verification & Pairing**: First-time connections require confirmation of a cryptographically random 6-digit numeric verification code displayed on both screens. Verified fingerprints are permanently stored in `~/.config/manguesechee/known_peers.json`. Unknown or unverified peers cannot control input devices.
- **mDNS Auto-Discovery**: The daemon broadcasts and browses for `_manguesechee._tcp.local` using `mdns-sd`. Peer machines on the same local subnet appear automatically in the UI and CLI.

### 3.4 Clipboard & File Transfer Engine (`manguesechee-agent`)
- **Text Clipboard**: Watches the native Wayland (`wl-clipboard` / data-control) or X11 (`xclip`) selection. When local text changes, an MD5 hash check prevents feedback loops and broadcasts the updated content to the peer.
- **Dual-Path File Transfers**:
  - **Inline Fast Path (≤ 15 MiB)**: Small files and images are sliced into 64 KiB chunks and multiplexed directly over the primary input channel with zero noticeable latency.
  - **Dedicated Background Channel (> 15 MiB)**: Large archives, videos, or documents stream asynchronously across a secondary socket, ensuring high-speed bulk transfers never degrade mouse polling rates or induce cursor stutter.

### 3.5 Inter-Process Communication (IPC)
- The agent daemon runs headless and listens on a dedicated Unix Domain Socket:
  `$XDG_RUNTIME_DIR/manguesechee/agent.sock` (fallback: `/tmp/manguesechee-$UID/agent.sock`).
- An active PID file (`agent.pid`) guarded by process liveness checks (`libc::kill(pid, 0)`) prevents multiple daemon collisions.
- The Slint GUI (`manguesechee-ui`) and CLI (`manguesechee-cli`) communicate with the daemon via bidirectional newline-delimited JSON commands (`GuiCommand` and `AgentEvent`).
- Both GUI and CLI can start, configure, monitor, and stop the daemon without needing root access.

### 3.6 Desktop Panel Integration (`ksni`)
- Native system tray icon integration using the KDE/FreeDesktop `StatusNotifierItem` (SNI) D-Bus protocol.
- Works across COSMIC Desktop, KDE Plasma, GNOME Shell (via AppIndicator / KSNI), and Sway/Waybar.
- Employs an in-memory 48×48 ARGB32 pixmap of the Manguesechee logo with fallback theme icons (`input-keyboard`, `changes-prevent`, `network-transmit-receive`).
- Intercepts window manager close events (`CloseRequestResponse::HideWindow`) to enable close-to-tray and minimize-to-tray behaviors.
