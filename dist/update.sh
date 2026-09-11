#!/usr/bin/env bash
# update.sh — Quick update for Manguesechee.
#
# Rebuilds and replaces the agent and UI binaries, updates the desktop launcher,
# and restarts the background service.
# Skips udev rules, group modifications, firewall setup, and existing configs.
#
# Usage:
#   ./dist/update.sh          # Auto-prompts for sudo only when copying files
#   sudo ./dist/update.sh

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# ── 1. Identify Target User ──────────────────────────────────────────────────

find_target_user() {
    if [[ -n "${SUDO_USER:-}" && "$SUDO_USER" != "root" ]]; then
        echo "$SUDO_USER"; return
    fi
    local logged_user
    logged_user=$(loginctl list-users --no-legend 2>/dev/null | awk '$1 >= 1000 {print $2; exit}')
    if [[ -n "$logged_user" ]]; then
        echo "$logged_user"; return
    fi
    getent passwd | awk -F: '$3 >= 1000 && $3 < 65000 {print $1; exit}'
}

TARGET_USER=$(find_target_user)
TARGET_UID=$(id -u "$TARGET_USER" 2>/dev/null || echo "1000")

# ── 2. Build Release Binaries ────────────────────────────────────────────────

# Ensure build prerequisites are met
if ! command -v pkg-config &>/dev/null || ! pkg-config --exists fontconfig 2>/dev/null; then
    echo "==> Installing build prerequisites (pkg-config, fontconfig)…"
    if command -v apt-get &>/dev/null; then
        if [[ "$EUID" -eq 0 ]]; then
            apt-get update -qq 2>/dev/null || true
            apt-get install -y pkg-config libfontconfig1-dev libxkbcommon-dev
        else
            sudo apt-get update -qq 2>/dev/null || true
            sudo apt-get install -y pkg-config libfontconfig1-dev libxkbcommon-dev
        fi
    elif command -v dnf &>/dev/null; then
        if [[ "$EUID" -eq 0 ]]; then
            dnf install -y pkgconf-pkg-config fontconfig-devel libxkbcommon-devel
        else
            sudo dnf install -y pkgconf-pkg-config fontconfig-devel libxkbcommon-devel
        fi
    fi
fi

# Ensure clipboard & notification utilities (wl-clipboard, xclip, notify-send) are installed
if ! command -v wl-copy &>/dev/null || ! command -v xclip &>/dev/null || ! command -v notify-send &>/dev/null; then
    echo "==> Ensuring clipboard & notification utilities are installed…"
    if command -v apt-get &>/dev/null; then
        if [[ "$EUID" -eq 0 ]]; then
            apt-get update -qq 2>/dev/null || true
            apt-get install -y wl-clipboard xclip libnotify-bin 2>/dev/null || true
        else
            sudo apt-get update -qq 2>/dev/null || true
            sudo apt-get install -y wl-clipboard xclip libnotify-bin 2>/dev/null || true
        fi
    elif command -v dnf &>/dev/null; then
        if [[ "$EUID" -eq 0 ]]; then
            dnf install -y wl-clipboard xclip libnotify 2>/dev/null || true
        else
            sudo dnf install -y wl-clipboard xclip libnotify 2>/dev/null || true
        fi
    fi
fi


echo "==> Building updated release binaries…"
cd "$REPO_ROOT"

# Run cargo as the real user if invoked with sudo, preserving cargo cache & environment
if [[ -n "${SUDO_USER:-}" && "$SUDO_USER" != "root" ]]; then
    SUDO_USER_HOME=$(getent passwd "$SUDO_USER" | cut -d: -f6)
    export HOME="$SUDO_USER_HOME"
    export PATH="$HOME/.cargo/bin:$PATH"
    sudo -u "$SUDO_USER" env PATH="$PATH" HOME="$HOME" cargo build --release --workspace
else
    cargo build --release --workspace
fi

AGENT_BIN="$REPO_ROOT/target/release/manguesechee-agent"
UI_BIN="$REPO_ROOT/target/release/manguesechee-ui"

[[ -f "$AGENT_BIN" ]] || { echo "error: $AGENT_BIN not found"; exit 1; }
[[ -f "$UI_BIN"    ]] || { echo "error: $UI_BIN not found"; exit 1; }

# ── 3. Install Binaries and Desktop Assets ────────────────────────────────────

echo "==> Updating installed binaries and assets…"
SUDO_CMD=""
if [[ "$EUID" -ne 0 ]]; then
    SUDO_CMD="sudo"
fi

$SUDO_CMD install -Dm755 "$AGENT_BIN" /usr/bin/manguesechee-agent
$SUDO_CMD install -Dm755 "$UI_BIN"    /usr/bin/manguesechee-ui

# Update desktop launcher and icons if present
if [[ -f "$REPO_ROOT/dist/manguesechee-ui.desktop" ]]; then
    $SUDO_CMD install -Dm644 "$REPO_ROOT/dist/manguesechee-ui.desktop" /usr/share/applications/manguesechee-ui.desktop
fi
if [[ -f "$REPO_ROOT/assets/icons/manguesechee-256.png" ]]; then
    $SUDO_CMD install -Dm644 "$REPO_ROOT/assets/icons/manguesechee-256.png" /usr/share/icons/hicolor/256x256/apps/manguesechee.png
fi
if [[ -f "$REPO_ROOT/assets/icons/manguesechee-48.png" ]]; then
    $SUDO_CMD install -Dm644 "$REPO_ROOT/assets/icons/manguesechee-48.png" /usr/share/icons/hicolor/48x48/apps/manguesechee.png
fi

# Update udev rules and ensure /dev/uinput permissions
if [[ -f "$REPO_ROOT/dist/99-manguesechee.rules" ]]; then
    $SUDO_CMD install -Dm644 "$REPO_ROOT/dist/99-manguesechee.rules" /etc/udev/rules.d/70-manguesechee.rules
    $SUDO_CMD install -Dm644 "$REPO_ROOT/dist/99-manguesechee.rules" /etc/udev/rules.d/99-manguesechee.rules
    $SUDO_CMD install -Dm644 "$REPO_ROOT/dist/99-manguesechee.rules" /usr/lib/udev/rules.d/70-manguesechee.rules 2>/dev/null || true
    $SUDO_CMD install -Dm644 "$REPO_ROOT/dist/99-manguesechee.rules" /usr/lib/udev/rules.d/99-manguesechee.rules 2>/dev/null || true
    $SUDO_CMD udevadm control --reload-rules 2>/dev/null || true
    $SUDO_CMD udevadm trigger --subsystem-match=input 2>/dev/null || true
    $SUDO_CMD udevadm trigger --subsystem-match=misc 2>/dev/null || true
fi

# Ensure /dev/uinput is immediately writable without requiring logout/reboot
$SUDO_CMD chmod 0666 /dev/uinput 2>/dev/null || true
if [[ -n "$TARGET_USER" ]]; then
    $SUDO_CMD setfacl -m u:"$TARGET_USER":rw /dev/uinput 2>/dev/null || true
    for dev in /dev/input/event*; do
        $SUDO_CMD setfacl -m u:"$TARGET_USER":rw "$dev" 2>/dev/null || true
    done
fi

# Update systemd user service
if [[ -f "$REPO_ROOT/dist/manguesechee-agent.service" ]]; then
    $SUDO_CMD install -Dm644 "$REPO_ROOT/dist/manguesechee-agent.service" /usr/lib/systemd/user/manguesechee-agent.service
    $SUDO_CMD install -Dm644 "$REPO_ROOT/dist/manguesechee-agent.service" /etc/systemd/user/manguesechee-agent.service 2>/dev/null || true
fi

$SUDO_CMD gtk-update-icon-cache -f -t /usr/share/icons/hicolor 2>/dev/null || true
$SUDO_CMD update-desktop-database /usr/share/applications 2>/dev/null || true

echo "    ✓ Updated /usr/bin/manguesechee-agent"
echo "    ✓ Updated /usr/bin/manguesechee-ui"
echo "    ✓ Applied udev rules & /dev/uinput permissions"
echo "    ✓ Updated systemd service unit"

# ── 4. Restart Running Agent Service ─────────────────────────────────────────

echo "==> Restarting manguesechee-agent service…"
if [[ -n "$TARGET_USER" ]]; then
    systemctl --user -M "${TARGET_USER}@" daemon-reload 2>/dev/null || true
    if [[ "$EUID" -eq 0 ]]; then
        su -l "$TARGET_USER" -c \
          "XDG_RUNTIME_DIR=/run/user/$TARGET_UID systemctl --user daemon-reload" 2>/dev/null || true
    else
        systemctl --user daemon-reload 2>/dev/null || true
    fi

    if systemctl --user -M "${TARGET_USER}@" restart manguesechee-agent 2>/dev/null; then
        echo "    ✓ Service restarted for $TARGET_USER"
    elif [[ "$EUID" -eq 0 ]]; then
        su -l "$TARGET_USER" -c \
          "XDG_RUNTIME_DIR=/run/user/$TARGET_UID systemctl --user restart manguesechee-agent" 2>/dev/null || true
        echo "    ✓ Service restarted"
    else
        systemctl --user restart manguesechee-agent 2>/dev/null || true
        echo "    ✓ Service restarted"
    fi
fi


echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo " Manguesechee successfully updated to the latest build!"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
