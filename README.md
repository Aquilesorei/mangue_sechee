# Manguesechee

Software KVM for Linux. Controls multiple computers with one keyboard and mouse over a local network.

The pointer moves between machines by reaching the screen edge, similar to how it moves between monitors in a multi-monitor setup. No screen streaming is involved — only input events and clipboard data are transmitted.

---

## Status

**Phase 1 ✓** — TCP transport, identity handshake, Ping/Pong  
**Phase 2 ✓** — Mouse capture (evdev) and injection (uinput) over TCP  
**Phase 3 ✓** — Keyboard capture and injection  
**Phase 4 ✓** — Edge switching with device grab (EVIOCGRAB)  
**Phase 5 ✓** — mDNS discovery + pairing (verification codes, known_peers.json)  
**Phase 6 ✓** — Slint GUI (device list, pairing, manual connect)  
**Phase 7 ✓** — Clipboard sync  
**Phase 8 ✓** — COSMIC/Wayland backend (DRM screen detection, wl-clipboard)  

---

## How it works

Every machine runs `manguesechee-agent`. When one agent connects to another:

- The **connector** becomes the controller: it reads keyboard and mouse events from its local input devices and streams them over TCP.
- The **listener** becomes the controlled peer: it receives those events and injects them into a virtual input device via uinput.

```
Controller                             Controlled peer
┌─────────────────────────┐            ┌─────────────────────────┐
│ manguesechee-agent      │  TCP/LAN   │ manguesechee-agent      │
│                         │ ─────────► │                         │
│  evdev capture          │  InputEvent│  uinput injection       │
│  /dev/input/event*      │  Clipboard │  /dev/uinput            │
└─────────────────────────┘            └─────────────────────────┘
  --connect <peer-ip>                    (just listens)
```

Move the cursor past the right screen edge — control switches to the peer. Move it off the peer's left edge — control returns.

---

## Requirements

- Linux kernel with evdev and uinput
- Wayland (COSMIC, GNOME, KDE) or X11
- Local network (Wi-Fi or Ethernet)
- Rust toolchain (1.75+)
- Fontconfig & pkg-config build libraries (required by Slint UI):
  - **Pop!_OS / Ubuntu / Debian**:
    ```sh
    sudo apt update && sudo apt install -y pkg-config libfontconfig1-dev libxkbcommon-dev
    ```
  - **Fedora / RHEL**:
    ```sh
    sudo dnf install -y pkgconf-pkg-config fontconfig-devel libxkbcommon-devel
    ```

---

## Installation & Packaging

Manguesechee includes automated packaging scripts for RPM-based (Fedora, RHEL) and DEB-based (Pop!_OS, Ubuntu, Debian) distributions.

### Automated Install (Recommended)

Run the install script with root privileges:

```sh
sudo ./dist/install.sh
```

The script automatically:
1. Detects distribution package manager (`dnf` or `apt`)
2. Compiles release binaries and creates native `.rpm` or `.deb` packages
3. Installs binaries to `/usr/bin/manguesechee-agent` and `/usr/bin/manguesechee-ui`
4. Adds the current user to `input` and `uinput` groups
5. Configures `/etc/udev/rules.d/99-manguesechee.rules` and persistent `uinput` kernel module
6. Opens firewall ports (`24800/tcp` and `5353/udp` for mDNS) via `firewalld` or `ufw`
7. Ensures clipboard tools (`wl-clipboard` for Wayland / COSMIC / KDE, `xclip` for X11) are present
8. Configures initial `~/.config/manguesechee/config.toml` with the machine's hostname
9. Enables and starts the systemd user service (`manguesechee-agent`)
10. Installs application icons and `.desktop` launcher

### Building Standalone Packages

To build packages without immediately installing:

```sh
# Build .rpm (Fedora / KDE)
./dist/package.sh --rpm

# Build .deb (Pop!_OS / COSMIC)
./dist/package.sh --deb
```

Resulting packages are placed in `dist/`:
- `dist/x86_64/manguesechee-<version>.rpm`
- `dist/manguesechee_<version>_<arch>.deb`

You can transfer and install the package on target machines using:
- Fedora: `sudo dnf install ./dist/x86_64/manguesechee-*.rpm`
- Pop!_OS: `sudo apt install ./dist/manguesechee_*.deb`

### Fast Update to Latest Build

To update an existing installation after pulling git updates:

```sh
sudo ./dist/update.sh
```

This compiles release binaries, ensures required build libraries, installs `/usr/bin/manguesechee-agent` and `/usr/bin/manguesechee-ui`, and restarts the user service without overwriting your custom configuration.

---

## Crate layout

| Crate | Role |
|---|---|
| `manguesechee-core` | Shared types: protocol messages, input events, topology, config |
| `manguesechee-agent` | Background daemon — sessions, edge switching, clipboard, pairing |
| `manguesechee-input` | Mouse/keyboard capture (evdev), injection (uinput), Wayland helpers |
| `manguesechee-network` | TCP transport, framing, mDNS discovery |
| `manguesechee-ui` | Slint GUI |
| `manguesechee-cli` | Command-line interface (stub) |

---

## Building

```sh
cargo build --workspace
```

---

## Running

```sh
# Controlled peer — run on the remote machine (e.g. COSMIC laptop)
RUST_LOG=info manguesechee-agent

# Controller — run on the local machine
RUST_LOG=info manguesechee-agent --connect <remote-ip>:24800
```

Screen resolution is auto-detected from `/sys/class/drm`. Override if needed:

```sh
RUST_LOG=info manguesechee-agent --connect <remote-ip>:24800 --width 2560 --height 1600
```

Override input devices if auto-detection picks the wrong one:

```sh
RUST_LOG=info manguesechee-agent \
    --connect   <remote-ip>:24800 \
    --mouse     /dev/input/event4 \
    --keyboard  /dev/input/event3
```

### First connection — pairing

A 6-digit verification code is shown on both terminals. Confirm the codes match,
then accept on the controlled machine. The pairing is stored in
`~/.config/manguesechee/known_peers.json`; subsequent connections skip this step.

---

## Configuration

`~/.config/manguesechee/config.toml` — created automatically on first run.

```toml
[device]
name = "my-machine"

[network]
discovery = true
port = 24800

[screen]
width  = 1920
height = 1080

[input]
enabled = true
# mouse_device    = "/dev/input/event4"
# keyboard_device = "/dev/input/event3"

[clipboard]
enabled = true

[[peers]]
id       = "other-machine"
address  = "192.168.1.x:24800"
position = "right"
```

If `screen.width` and `screen.height` are left at 1920×1080 (the default) and
no `--width`/`--height` flags are passed, the agent reads the primary output
resolution from `/sys/class/drm` automatically. This works on Wayland, X11,
and TTY sessions.

---

## Security

On first connection a pairing code must be confirmed on both machines. Connections
from unknown peers are rejected. Paired identities are stored in `known_peers.json`.

Traffic encryption is planned for a later phase.

---

## License

TBD
