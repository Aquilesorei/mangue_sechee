#!/usr/bin/env bash
# install.sh — package and install Manguesechee on the current machine.
#
# Usage:
#   sudo ./dist/install.sh          # auto-detect format
#   sudo ./dist/install.sh --deb    # force .deb
#   sudo ./dist/install.sh --rpm    # force .rpm

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST_DIR="$REPO_ROOT/dist"

FORMAT="${1:-auto}"

# ── Resolve format ─────────────────────────────────────────────────────────────

resolve_format() {
  if [[ "$FORMAT" == "auto" ]]; then
    if command -v apt-get &>/dev/null || command -v dpkg &>/dev/null; then
      FORMAT=deb
    elif command -v dnf &>/dev/null || command -v rpm &>/dev/null; then
      FORMAT=rpm
    else
      echo "error: cannot detect package manager (tried apt/dpkg and dnf/rpm)."
      exit 1
    fi
  fi
  case "$FORMAT" in
    --deb) FORMAT=deb ;;
    --rpm) FORMAT=rpm ;;
    deb|rpm) ;;
    *) echo "Usage: $0 [--deb|--rpm]"; exit 1 ;;
  esac
}

resolve_format

# ── Check for Existing Installation ──────────────────────────────────────────

if [[ "${1:-}" == "--update" ]] || [[ "$FORMAT" == "auto" && -f /usr/bin/manguesechee-agent && -f /usr/bin/manguesechee-ui && "${1:-}" != "--force" ]]; then
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo " Existing Manguesechee installation detected."
    echo " Running fast update (updating binaries & restarting service)…"
    echo " (Use --force to perform a full clean reinstall from package)"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    exec "$DIST_DIR/update.sh"
fi

# ── Check we're running as root (needed for package install) ──────────────────

if [[ "$EUID" -ne 0 ]]; then
  echo "error: install.sh must be run with sudo for initial system installation."
  echo "  sudo ./dist/install.sh"
  exit 1
fi

# ── Ensure build tools are present ───────────────────────────────────────────

check_tool() {
  command -v "$1" &>/dev/null && return 0
  echo "==> Installing build prerequisite: $2"
  case "$FORMAT" in
    deb) apt-get install -y "$2" ;;
    rpm) dnf install -y "$2" ;;
  esac
}

case "$FORMAT" in
  deb)
    check_tool dpkg-deb dpkg
    check_tool cargo    cargo   || true  # Rust installed separately
    ;;
  rpm)
    check_tool rpmbuild rpm-build
    check_tool cargo    cargo   || true
    ;;
esac

if ! command -v cargo &>/dev/null; then
  echo ""
  echo "error: cargo (Rust toolchain) is required to build from source."
  echo "  Install via: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
  exit 1
fi

# ── Build package ─────────────────────────────────────────────────────────────

echo "==> Building package (format: $FORMAT)…"
# Run package.sh as the original user (not root) so Cargo home is correct.
SUDO_USER_HOME=$(getent passwd "${SUDO_USER:-$USER}" | cut -d: -f6)
export HOME="$SUDO_USER_HOME"
export PATH="$HOME/.cargo/bin:$PATH"

# Resolve flag for package.sh
PKG_FLAG="--$FORMAT"
sudo -u "${SUDO_USER:-$USER}" bash "$DIST_DIR/package.sh" "$PKG_FLAG"

# ── Find the produced package ─────────────────────────────────────────────────

case "$FORMAT" in
  deb)
    PKG=$(find "$DIST_DIR" -maxdepth 1 -name "*.deb" | sort | tail -1)
    ;;
  rpm)
    PKG=$(find "$DIST_DIR" -name "*.rpm" | sort | tail -1)
    ;;
esac

if [[ -z "${PKG:-}" || ! -f "$PKG" ]]; then
  echo "error: package file not found after build."
  exit 1
fi

echo ""
echo "==> Installing $PKG"

# ── Install ───────────────────────────────────────────────────────────────────

case "$FORMAT" in
  deb)
    dpkg -i "$PKG"
    # Fix any missing deps
    apt-get install -f -y 2>/dev/null || true
    ;;
  rpm)
    dnf install -y "$PKG" 2>/dev/null \
      || rpm -Uvh --force "$PKG"
    ;;
esac

# ── Post-install configuration ────────────────────────────────────────────────
echo ""
echo "==> Running automated configuration…"
if [[ -x "$DIST_DIR/post-install.sh" ]]; then
  "$DIST_DIR/post-install.sh"
elif [[ -x /usr/lib/manguesechee/post-install.sh ]]; then
  /usr/lib/manguesechee/post-install.sh
fi

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo " Manguesechee has been packaged, installed, and fully configured!"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo " Services, firewall rules, and group permissions are all in place."
echo " You can launch Manguesechee from your application menu, or run:"
echo "   manguesechee-ui"
echo ""
