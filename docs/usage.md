# Manguesechee Usage Guide

This guide covers installing, configuring, and running Manguesechee via its native GUI, system tray, command-line interface (CLI), and desktop widgets.

---

## 1. Quick Setup & Installation

### 1.1 Automated Distribution Install (Recommended)

Manguesechee provides an automated packaging and installation script for RPM (Fedora, RHEL) and DEB (Pop!_OS, Ubuntu, Debian) based distributions:

```sh
sudo ./dist/install.sh
```

The script configures:
- Compilation of release binaries (`manguesechee-agent`, `manguesechee-ui`, `manguesechee-cli`)
- Kernel `uinput` driver and udev rules in `/etc/udev/rules.d/99-manguesechee.rules`
- Adds your current user to the `input` and `uinput` groups
- Firewall rules (`24800/tcp` for KVM and `5353/udp` for mDNS discovery)
- Systemd user service (`manguesechee-agent.service`) enabled and started
- Desktop `.desktop` application entry and icons

### 1.2 Manual Compilation

If building manually from source:

```sh
# Install build dependencies:
# Debian/Ubuntu/Pop!_OS:
sudo apt install -y pkg-config libfontconfig1-dev libxkbcommon-dev
# Fedora/RHEL:
sudo dnf install -y pkgconf-pkg-config fontconfig-devel libxkbcommon-devel

# Build all release binaries
cargo build --release --workspace
```

Ensure `/dev/uinput` is accessible:
```sh
echo "KERNEL==\"uinput\", GROUP=\"input\", MODE=\"0660\"" | sudo tee /etc/udev/rules.d/99-uinput.rules
sudo udevadm control --reload-rules && sudo udevadm trigger
sudo usermod -aG input,uinput $USER
```
*(Log out and log back in for group membership to take effect)*.

---

## 2. Desktop GUI Walkthrough (`manguesechee-ui`)

Launch the graphical interface via your desktop app launcher or run:
```sh
manguesechee-ui
```

### 2.1 First-Run Wizard
On first launch, Manguesechee displays a setup wizard:
1. **Choose Machine Role**: Both (Controller + Peer), Controller Only, or Peer Only.
2. **Configure Primary Peer**: Enter the target peer IP:port or leave blank to use automatic mDNS LAN discovery.
3. **Select Initial Layout**: Specify whether the peer screen sits to the Right, Left, Above, or Below.

### 2.2 2D Screen Topology Layout
Navigate to the **Topology** tab:
- **Visual Canvas**: Displays a 5×3 spatial grid with your machine centered at `(0, 0)`.
- **Nudge Controls**: Use `[⬅️]`, `[➡️]`, `[⬆️]`, `[⬇️]` to reposition any screen on the integer grid.
- **Layout Presets**: Instantly apply common setups:
  - *Dual Right*: Side-by-side secondary display at `(1, 0)`.
  - *Dual Left*: Secondary display at `(-1, 0)`.
  - *Laptop Above*: External display perched above laptop at `(0, 1)`.
  - *Triple Stack*: Multiple displays chained horizontally (`(-1, 0)`, `(0, 0)`, `(1, 0)`).
- **Live Sync**: Repositioning immediately synchronizes with the peer over network IPC.

### 2.3 System Tray & Minimize-to-Tray
Manguesechee includes a native FreeDesktop StatusNotifierItem system tray:
- **Left-Click Tray Icon**: Restores and focuses the main window.
- **Close Button ("X")**: Closing the window hides it to the system tray, keeping background mouse/keyboard forwarding and clipboard sync running.
- **"📥 Hide to Tray" Button**: Quick action in the top header bar to minimize the GUI.
- **Right-Click Context Menu**:
  - `Show Manguesechee`: Restores the GUI window.
  - `Status: [Ready | Forwarding → ... | Stopped]`: Glanceable live state.
  - `Connect to Peer ▶`: Submenu listing all discovered LAN peers for 1-click connection.
  - `Disconnect`: Terminates active remote control session.
  - `🔒 Cursor Locked (Local Screen)`: Interactive checkmark to pin cursor to local screen.
  - `🖥️ Switch Screen (Hotkey)`: Hops focus to adjacent screen.
  - `🔄 Restart Daemon` / `⏹ Stop Daemon` / `▶ Start Daemon`: Controls the background agent.
  - `Quit Manguesechee`: Gracefully unregisters tray and exits UI process.

---

## 3. Command-Line Interface (`manguesechee-cli`)

The CLI provides complete, scriptable control over the daemon and active sessions.

### 3.1 Status & Diagnostics
```sh
# Overview of daemon, connection, lock, and peer list
manguesechee-cli status

# Continuous real-time monitor (refreshes every 2 seconds)
manguesechee-cli status --watch

# Raw structured JSON output
manguesechee-cli status --json
```

### 3.2 Waybar & Polybar Integration
The CLI natively formats output for the Waybar custom module protocol:
```sh
manguesechee-cli status --waybar
```
*Output sample:*
```json
{"text":" ➔ laptop","tooltip":"Manguesechee KVM\nDevice: fedora\nStatus: 192.168.1.50:24800\nCursor: Unlocked\nTLS: Active","class":"forwarding","percentage":100}
```

#### Waybar Configuration Example
Add to `~/.config/waybar/config`:
```json
"custom/manguesechee": {
    "format": "{}",
    "return-type": "json",
    "interval": 2,
    "exec": "manguesechee-cli status --waybar",
    "on-click": "manguesechee-cli toggle-lock",
    "on-click-middle": "manguesechee-cli switch",
    "on-click-right": "manguesechee-ui"
}
```

### 3.3 Connection Management
```sh
# Connect to a target by IP and port
manguesechee-cli connect 192.168.1.50:24800

# Connect by peer name (automatically resolved from discovered peers)
manguesechee-cli connect laptop-fedora

# Disconnect from active peer
manguesechee-cli disconnect
```

### 3.4 Cursor Locking & Screen Switching
```sh
# Lock cursor to local screen (prevents edge jumping during gaming/window drags)
manguesechee-cli lock

# Unlock cursor to enable edge gliding
manguesechee-cli unlock

# Toggle lock state (ideal for custom hotkey binding)
manguesechee-cli toggle-lock

# Instantly hop cursor and keyboard focus to the next screen
manguesechee-cli switch
```

### 3.5 Topology & Layout
```sh
# Print ASCII visual 2D grid of screen arrangement
manguesechee-cli layout

# Arrange peer screen adjacent to this machine
manguesechee-cli layout set 192.168.1.50 left

# Place peer at exact 2D grid coordinates (x=1, y=0)
manguesechee-cli layout grid 192.168.1.50 1 0

# Set edge resistance delay (milliseconds before edge crossing triggers)
manguesechee-cli resistance 150
```

### 3.6 File Transfers
```sh
# Stage a local file for immediate transfer to the connected peer
manguesechee-cli send-file ~/Documents/presentation.pdf
```
*Files are staged in the clipboard with `text/uri-list` and streamed over TLS. On the remote computer, press `Ctrl+V` in your file manager (Nautilus, Dolphin, COSMIC Files) to paste the file.*

### 3.7 Daemon Lifecycle
```sh
# Start background daemon
manguesechee-cli daemon start

# Stop daemon
manguesechee-cli daemon stop

# Restart daemon
manguesechee-cli daemon restart

# Inspect service status
manguesechee-cli daemon status

# Enable/disable launch at login
manguesechee-cli daemon enable
manguesechee-cli daemon disable
```

---

## 4. Global Hotkeys & Window Manager Shortcuts

### Built-in Shortcuts
- `Ctrl + Alt + Arrow Keys`: Directly hops focus across the 2D grid (e.g. `Ctrl+Alt+Right` jumps to the monitor at `(1, 0)`).
- `Ctrl + Alt + L`: Toggles mouse cursor lock.
- `Ctrl + Alt + Escape`: Emergency ungrab (instantly releases all hardware capture back to local display).

### Custom Window Manager Bindings
You can bind `manguesechee-cli` commands in any desktop compositor:

#### Sway / i3 (`~/.config/sway/config` or `~/.config/i3/config`):
```i3
# Toggle cursor lock
bindsym $mod+F12 exec manguesechee-cli toggle-lock

# Switch to next screen
bindsym $mod+grave exec manguesechee-cli switch
```

#### Hyprland (`~/.config/hypr/hyprland.conf`):
```ini
bind = SUPER, F12, exec, manguesechee-cli toggle-lock
bind = SUPER, grave, exec, manguesechee-cli switch
```

---

## 5. Configuration File Reference

The configuration file is located at `~/.config/manguesechee/config.toml`:

```toml
[device]
name = "fedora-desktop"
id   = "8865e9f5-9ecb-4cda-bfa1-6a7d9bc6c06c"

[network]
discovery = true        # mDNS auto-discovery on LAN
port      = 24800       # Listening TCP port
tls       = true        # TLS 1.3 encryption

[input]
enabled                 = true  # True: can send input. False: peer only.
switch_delay_ms         = 100   # Edge dwell delay in ms before crossing (resistance)
corner_deadzone_px      = 50    # Corner pixel deadzone (ignores corner triggers)
cursor_locked           = false # Pin cursor to local screen
edge_velocity_threshold = 20    # Minimum pixel/event velocity to cross edge

[clipboard]
enabled               = true
files_enabled         = true    # File transfer synchronization
fast_limit_mb         = 15      # Inline streaming threshold (MB)
background_limit_mb   = 500     # Maximum background transfer size (MB)

[screen]
width  = 1920           # Screen dimensions (auto-detected from DRM if left at default)
height = 1080

[hotkeys]
enabled            = true
toggle_cursor_lock = "Ctrl+Alt+L"
switch_screen      = "Ctrl+Alt+Right"
emergency_escape   = "Ctrl+Alt+Escape"

[[peers]]
id       = "arch-laptop"
address  = "192.168.1.50:24800"
position = "right"
grid_x   = 1
grid_y   = 0
```

---

## 6. Troubleshooting & Diagnostics

- **Daemon logs**:
  ```sh
  journalctl --user -u manguesechee-agent -f
  ```
- **Socket communication error**:
  Ensure the agent is running: `manguesechee-cli daemon status`.
- **Mouse not moving or input blocked**:
  Verify your user is in the `input` and `uinput` groups:
  ```sh
  groups | grep -E 'input|uinput'
  ```
- **Firewall blocking connection**:
  Ensure port `24800/tcp` and `5353/udp` are open:
  ```sh
  sudo firewall-cmd --add-port=24800/tcp --add-port=5353/udp --permanent && sudo firewall-cmd --reload
  # Or with UFW:
  sudo ufw allow 24800/tcp && sudo ufw allow 5353/udp
  ```
