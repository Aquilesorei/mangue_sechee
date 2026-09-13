# Manguesechee

[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org)
[![Platform](https://img.shields.io/badge/platform-Linux%20(Wayland%20%2F%20X11)-blue.svg)](#requirements)
[![License](https://img.shields.io/badge/license-MIT%20%2F%20Apache--2.0-green.svg)](#license)

**Manguesechee** is a modern, ultra-low latency software KVM for Linux. It allows you to control multiple computers seamlessly with a single keyboard and mouse over your local network.

Simply glide your mouse cursor past the edge of your screen to take control of an adjacent machine. No video capture or screen streaming is involved — only raw, encrypted hardware input events and clipboard data are transmitted.

---

## Features

- **2D Grid Topology & Edge Gliding**: Arrange screens on an arbitrary $(x, y)$ coordinate grid. Features proportional resolution normalization ($y/H$), customizable edge resistance dwell delay, and focus anti-fight protection.
- **TLS 1.3 Encryption & Secure Pairing**: All network traffic is encrypted end-to-end via TLS 1.3 (`rustls`). New connections require one-time mutual 6-digit cryptographic PIN verification.
- **Seamless Text & File Clipboard**: Syncs clipboard text in real-time. Supports direct file and directory drag-and-drop transfers with automatic dual-path streaming (fast-path inline for files ≤ 15MB, dedicated background channel for large files).
- **Native Slint GUI**: Hardware-accelerated desktop interface featuring an interactive 2D monitor canvas, nudge controls, topology presets, and live journal logs.
- **System Tray & Minimize-to-Tray**: Native FreeDesktop/KDE `StatusNotifierItem` tray icon (compatible with **COSMIC**, **KDE Plasma**, **GNOME**, and **Sway/Waybar**). Closing the window hides it to tray while keeping input forwarding active.
- **Powerful CLI (`manguesechee-cli`)**: Full terminal management with `--waybar` JSON widget output, scriptable session controls, hotkey toggles, and daemon management.

---

## Documentation

Detailed documentation is available in the [`docs/`](docs/) directory:

- **[Architecture & Internal Design](docs/architecture.md)** — Explains the kernel `evdev`/`uinput` subsystem, network wire protocol, coordinate routing, and IPC architecture.
- **[Complete Usage Guide](docs/usage.md)** — Detailed walkthrough for the Slint GUI, CLI reference, Waybar/Polybar status bar integration, window manager hotkeys (Sway, Hyprland, i3), and `config.toml` options.

---

## Quick Start

### 1. Automated Installation (Recommended)

Run the automated installer on Debian/Ubuntu/Pop!_OS or Fedora/RHEL:

```sh
sudo ./dist/install.sh
```

This compiles optimized release binaries, sets up `/dev/uinput` udev rules, adds your user to the `input` and `uinput` groups, opens firewall ports (`24800/tcp`, `5353/udp`), and registers the systemd user service.

### 2. Manual Build

```sh
# Build all release binaries
cargo build --release --workspace

# Run the GUI
./target/release/manguesechee-ui

# Or inspect status via CLI
./target/release/manguesechee-cli status
```

---

## Workspace Crates

| Crate | Description |
|---|---|
| [`manguesechee-core`](crates/manguesechee-core) | Wire protocol (`bincode`), IPC schemas, 2D topology math, and config models. |
| [`manguesechee-agent`](crates/manguesechee-agent) | Headless daemon managing sessions, input dispatch, clipboard, and TLS sockets. |
| [`manguesechee-input`](crates/manguesechee-input) | Kernel `evdev` capture, `EVIOCGRAB`, virtual `uinput` injection, and DRM screen detection. |
| [`manguesechee-network`](crates/manguesechee-network) | Framed async TCP transport, TLS 1.3 handshakes, and mDNS discovery. |
| [`manguesechee-ui`](crates/manguesechee-ui) | Slint graphical interface and FreeDesktop `StatusNotifierItem` system tray (`ksni`). |
| [`manguesechee-cli`](crates/manguesechee-cli) | Terminal client (`clap v4`) with Waybar JSON formatting and daemon management. |

---

## License

Dual-licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.
