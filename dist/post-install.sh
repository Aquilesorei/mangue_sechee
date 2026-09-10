#!/usr/bin/env bash
# post-install.sh
# Automated system configuration for Manguesechee.
# Configures groups, udev, kernel modules, firewall, clipboard, config, and systemd service.

set -euo pipefail

# ── 1. Identify Target User ──────────────────────────────────────────────────

find_target_user() {
    # If invoked via sudo, use SUDO_USER
    if [[ -n "${SUDO_USER:-}" && "$SUDO_USER" != "root" ]]; then
        echo "$SUDO_USER"
        return
    fi
    # Active GUI / login session
    local logged_user
    logged_user=$(loginctl list-users --no-legend 2>/dev/null | awk '$1 >= 1000 {print $2; exit}')
    if [[ -n "$logged_user" ]]; then
        echo "$logged_user"
        return
    fi
    # First regular user in passwd
    getent passwd | awk -F: '$3 >= 1000 && $3 < 65000 {print $1; exit}'
}

TARGET_USER=$(find_target_user)
if [[ -z "$TARGET_USER" ]]; then
    echo "Warning: Could not identify non-root user. Skipping user-specific configurations."
    TARGET_UID=""
    TARGET_HOME=""
else
    TARGET_UID=$(id -u "$TARGET_USER" 2>/dev/null || echo "1000")
    TARGET_HOME=$(getent passwd "$TARGET_USER" | cut -d: -f6)
    echo "==> Configuring for user: $TARGET_USER (uid=$TARGET_UID, home=$TARGET_HOME)"
fi

# ── 2. User Groups ───────────────────────────────────────────────────────────

echo "==> Setting up input and uinput groups…"
groupadd -f input  2>/dev/null || true
groupadd -f uinput 2>/dev/null || true

if [[ -n "$TARGET_USER" ]]; then
    usermod -aG input,uinput "$TARGET_USER"
    echo "    ✓ Added $TARGET_USER to groups: input, uinput"
fi

# ── 3. Udev and Kernel Modules ───────────────────────────────────────────────

echo "==> Configuring udev and uinput kernel module…"
# Ensure uinput rule exists
if [[ -f /usr/lib/udev/rules.d/99-manguesechee.rules ]]; then
    cp -f /usr/lib/udev/rules.d/99-manguesechee.rules /etc/udev/rules.d/99-manguesechee.rules 2>/dev/null || true
fi

udevadm control --reload-rules 2>/dev/null || true
udevadm trigger 2>/dev/null || true

modprobe uinput 2>/dev/null || true
mkdir -p /etc/modules-load.d
echo "uinput" > /etc/modules-load.d/uinput.conf
echo "    ✓ udev rules reloaded and uinput persistent in /etc/modules-load.d/uinput.conf"

# ── 4. Firewall ──────────────────────────────────────────────────────────────

echo "==> Configuring firewall (ports 24800/tcp and 5353/udp)…"
if command -v firewall-cmd &>/dev/null; then
    firewall-cmd --permanent --add-port=24800/tcp --quiet 2>/dev/null || true
    firewall-cmd --permanent --add-port=5353/udp  --quiet 2>/dev/null || true
    firewall-cmd --reload --quiet 2>/dev/null || true
    echo "    ✓ firewalld: opened 24800/tcp and 5353/udp"
elif command -v ufw &>/dev/null; then
    ufw allow 24800/tcp >/dev/null 2>&1 || true
    ufw allow 5353/udp  >/dev/null 2>&1 || true
    echo "    ✓ ufw: opened 24800/tcp and 5353/udp"
else
    echo "    ℹ No firewalld or ufw detected."
fi

# ── 5. Clipboard Tools (wl-clipboard / xclip) ────────────────────────────────

echo "==> Checking clipboard utilities…"
if ! command -v wl-copy &>/dev/null && ! command -v xclip &>/dev/null; then
    if command -v dnf &>/dev/null; then
        echo "    Installing wl-clipboard via dnf…"
        dnf install -y wl-clipboard 2>/dev/null || true
    elif command -v apt-get &>/dev/null; then
        echo "    Installing wl-clipboard via apt-get…"
        apt-get update -qq 2>/dev/null || true
        apt-get install -y wl-clipboard 2>/dev/null || true
    fi
else
    echo "    ✓ Clipboard utility available ($(command -v wl-copy 2>/dev/null || command -v xclip 2>/dev/null))"
fi

# ── 6. User Configuration File ───────────────────────────────────────────────

if [[ -n "$TARGET_USER" && -n "$TARGET_HOME" ]]; then
    CFG_DIR="$TARGET_HOME/.config/manguesechee"
    CFG_FILE="$CFG_DIR/config.toml"
    mkdir -p "$CFG_DIR"

    # Determine a non-empty hostname
    HOST=$(hostname -s 2>/dev/null || hostname 2>/dev/null || uname -n 2>/dev/null || echo "manguesechee-device")
    [[ -z "$HOST" ]] && HOST="manguesechee-device"

    if [[ ! -f "$CFG_FILE" ]]; then
        cat > "$CFG_FILE" << EOF
[device]
name = "$HOST"

[network]
discovery = true
port = 24800

[screen]
width  = 1920
height = 1080

[input]
enabled = true

[clipboard]
enabled = true

peers = []
EOF
        chown -R "$TARGET_USER:$TARGET_USER" "$CFG_DIR"
        echo "    ✓ Created default config at $CFG_FILE (name = \"$HOST\")"
    else
        # If device.name is empty, populate it
        if grep -qE '^\s*name\s*=\s*""' "$CFG_FILE" 2>/dev/null; then
            sed -i "s/^\s*name\s*=\s*\"\".*/name = \"$HOST\"/" "$CFG_FILE"
            echo "    ✓ Updated empty device.name in $CFG_FILE to \"$HOST\""
        fi
        chown -R "$TARGET_USER:$TARGET_USER" "$CFG_DIR"
    fi
fi

# ── 7. Systemd User Service ──────────────────────────────────────────────────

if [[ -n "$TARGET_USER" && -n "$TARGET_UID" ]]; then
    echo "==> Enabling and starting manguesechee-agent systemd user service…"
    loginctl enable-linger "$TARGET_USER" 2>/dev/null || true

    # Enable and start/restart user service via systemctl -M
    systemctl --user -M "${TARGET_USER}@" enable manguesechee-agent 2>/dev/null || true
    if systemctl --user -M "${TARGET_USER}@" restart manguesechee-agent 2>/dev/null; then
        echo "    ✓ manguesechee-agent active for $TARGET_USER"
    else
        su -l "$TARGET_USER" -c \
          "XDG_RUNTIME_DIR=/run/user/$TARGET_UID systemctl --user restart manguesechee-agent || systemctl --user enable --now manguesechee-agent" 2>/dev/null || \
          echo "    ℹ Service enabled for next session login."
    fi
fi

# ── 8. Icon and Desktop Database ─────────────────────────────────────────────

echo "==> Updating desktop and icon caches…"
gtk-update-icon-cache -f -t /usr/share/icons/hicolor 2>/dev/null || true
update-desktop-database /usr/share/applications 2>/dev/null || true
echo "    ✓ Desktop caches updated"

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo " Manguesechee has been fully configured and is ready to use!"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
