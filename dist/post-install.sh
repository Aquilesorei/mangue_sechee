#!/usr/bin/env bash
# post-install.sh
# Automated system configuration for Manguesechee.
# Idempotent: detects existing configuration and skips redundant steps.

set -euo pipefail

# ── 1. Identify Target User ──────────────────────────────────────────────────

find_target_user() {
    if [[ -n "${SUDO_USER:-}" && "$SUDO_USER" != "root" ]]; then
        echo "$SUDO_USER"
        return
    fi
    local logged_user
    logged_user=$(loginctl list-users --no-legend 2>/dev/null | awk '$1 >= 1000 {print $2; exit}')
    if [[ -n "$logged_user" ]]; then
        echo "$logged_user"
        return
    fi
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
    echo "==> Configuring for user: $TARGET_USER (uid=$TARGET_UID)"
fi

# ── 2. User Groups (Input & Uinput) ──────────────────────────────────────────

if [[ -n "$TARGET_USER" ]]; then
    CURRENT_GROUPS=$(id -nG "$TARGET_USER" 2>/dev/null || echo "")
    if [[ "$CURRENT_GROUPS" =~ (^|[[:space:]])input([[:space:]]|$) ]] && \
       [[ "$CURRENT_GROUPS" =~ (^|[[:space:]])uinput([[:space:]]|$) ]]; then
        echo "    ✓ User $TARGET_USER already in input and uinput groups"
    else
        echo "==> Adding $TARGET_USER to input and uinput groups…"
        groupadd -f input  2>/dev/null || true
        groupadd -f uinput 2>/dev/null || true
        usermod -aG input,uinput "$TARGET_USER"
        echo "    ✓ Added $TARGET_USER to groups: input, uinput"
    fi
fi

# ── 3. Udev and Kernel Modules ───────────────────────────────────────────────

echo "==> Configuring udev and persistent uinput module…"
if [[ -f /usr/lib/udev/rules.d/99-manguesechee.rules ]]; then
    cp -f /usr/lib/udev/rules.d/99-manguesechee.rules /etc/udev/rules.d/70-manguesechee.rules 2>/dev/null || true
    cp -f /usr/lib/udev/rules.d/99-manguesechee.rules /etc/udev/rules.d/99-manguesechee.rules 2>/dev/null || true
fi
udevadm control --reload-rules 2>/dev/null || true
udevadm trigger --subsystem-match=input 2>/dev/null || true
udevadm trigger --subsystem-match=misc 2>/dev/null || true

modprobe uinput 2>/dev/null || true
mkdir -p /etc/modules-load.d
echo "uinput" > /etc/modules-load.d/uinput.conf

# Grant immediate read/write permissions
chmod 0666 /dev/uinput 2>/dev/null || true
if [[ -n "$TARGET_USER" ]]; then
    setfacl -m u:"$TARGET_USER":rw /dev/uinput 2>/dev/null || true
    for dev in /dev/input/event*; do
        setfacl -m u:"$TARGET_USER":rw "$dev" 2>/dev/null || true
    done
fi
echo "    ✓ udev rules applied, permissions granted, and uinput persistent"

# ── 4. Firewall (Ports 24800/tcp and 5353/udp) ───────────────────────────────

if command -v firewall-cmd &>/dev/null; then
    if firewall-cmd --query-port=24800/tcp &>/dev/null && firewall-cmd --query-port=5353/udp &>/dev/null; then
        echo "    ✓ Firewall ports 24800/tcp and 5353/udp already open"
    else
        echo "==> Opening ports in firewalld…"
        firewall-cmd --permanent --add-port=24800/tcp --quiet 2>/dev/null || true
        firewall-cmd --permanent --add-port=5353/udp  --quiet 2>/dev/null || true
        firewall-cmd --reload --quiet 2>/dev/null || true
        echo "    ✓ firewalld: opened 24800/tcp and 5353/udp"
    fi
elif command -v ufw &>/dev/null; then
    if ufw status 2>/dev/null | grep -q "24800/tcp"; then
        echo "    ✓ ufw ports already configured"
    else
        echo "==> Opening ports in ufw…"
        ufw allow 24800/tcp >/dev/null 2>&1 || true
        ufw allow 5353/udp  >/dev/null 2>&1 || true
        echo "    ✓ ufw: opened 24800/tcp and 5353/udp"
    fi
fi

# ── 5. Clipboard Tools (wl-clipboard / xclip) ────────────────────────────────

echo "==> Ensuring clipboard utilities (wl-clipboard & xclip) are installed…"
if command -v dnf &>/dev/null; then
    dnf install -y wl-clipboard xclip 2>/dev/null || true
elif command -v apt-get &>/dev/null; then
    apt-get update -qq 2>/dev/null || true
    apt-get install -y wl-clipboard xclip 2>/dev/null || true
fi


# ── 6. User Configuration File ───────────────────────────────────────────────

if [[ -n "$TARGET_USER" && -n "$TARGET_HOME" ]]; then
    CFG_DIR="$TARGET_HOME/.config/manguesechee"
    CFG_FILE="$CFG_DIR/config.toml"

    if [[ -f "$CFG_FILE" ]]; then
        # If device.name was previously empty, fix it
        if grep -qE '^\s*name\s*=\s*""' "$CFG_FILE" 2>/dev/null; then
            HOST=$(hostname -s 2>/dev/null || hostname 2>/dev/null || uname -n 2>/dev/null || echo "manguesechee-device")
            sed -i "s/^\s*name\s*=\s*\"\".*/name = \"$HOST\"/" "$CFG_FILE"
            echo "    ✓ Updated empty device.name in $CFG_FILE to \"$HOST\""
        else
            echo "    ✓ Existing config preserved at $CFG_FILE"
        fi
    else
        mkdir -p "$CFG_DIR"
        HOST=$(hostname -s 2>/dev/null || hostname 2>/dev/null || uname -n 2>/dev/null || echo "manguesechee-device")
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
    fi
fi

# ── 7. Systemd User Service ──────────────────────────────────────────────────

if [[ -n "$TARGET_USER" && -n "$TARGET_UID" ]]; then
    loginctl enable-linger "$TARGET_USER" 2>/dev/null || true
    systemctl --user -M "${TARGET_USER}@" enable manguesechee-agent 2>/dev/null || true
    if systemctl --user -M "${TARGET_USER}@" restart manguesechee-agent 2>/dev/null; then
        echo "    ✓ manguesechee-agent service restarted"
    else
        su -l "$TARGET_USER" -c \
          "XDG_RUNTIME_DIR=/run/user/$TARGET_UID systemctl --user restart manguesechee-agent || systemctl --user enable --now manguesechee-agent" 2>/dev/null || true
        echo "    ✓ manguesechee-agent service active"
    fi
fi

# ── 8. Icon and Desktop Database ─────────────────────────────────────────────

gtk-update-icon-cache -f -t /usr/share/icons/hicolor 2>/dev/null || true
update-desktop-database /usr/share/applications 2>/dev/null || true
echo "    ✓ Desktop and icon caches refreshed"

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo " Manguesechee is ready to use!"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
