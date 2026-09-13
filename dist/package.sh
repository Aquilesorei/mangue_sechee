#!/usr/bin/env bash
# package.sh — build release binaries and create a .deb or .rpm package.
#
# Usage:
#   ./dist/package.sh          # auto-detect format from the current OS
#   ./dist/package.sh --deb    # force .deb  (requires dpkg-deb)
#   ./dist/package.sh --rpm    # force .rpm  (requires rpm-build)
#
# On Fedora/COSMIC:  sudo dnf install rpm-build
# On Ubuntu/Debian:  dpkg-deb is included with dpkg (pre-installed)
#
# Output: dist/manguesechee_<version>_<arch>.deb  or  .rpm

set -euo pipefail

# ── Config ────────────────────────────────────────────────────────────────────

VERSION="0.2.0"
NAME="manguesechee"
DESCRIPTION="Software KVM for Linux — control multiple machines with one keyboard and mouse"
MAINTAINER="Manguesechee contributors"
URL="https://github.com/manguesechee/manguesechee"
LICENSE="TBD"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST_DIR="$REPO_ROOT/dist"

ARCH_DEB=$(uname -m \
  | sed 's/x86_64/amd64/' \
  | sed 's/aarch64/arm64/' \
  | sed 's/armv7l/armhf/')
ARCH_RPM=$(uname -m)

# ── Format detection ──────────────────────────────────────────────────────────

FORMAT="${1:-auto}"
case "$FORMAT" in
  --deb) FORMAT=deb ;;
  --rpm) FORMAT=rpm ;;
  auto)
    # Prefer rpm if dnf/rpm present (Fedora, COSMIC, RHEL, openSUSE).
    # Fall back to deb if dpkg present (Debian, Ubuntu, Mint).
    if command -v rpmbuild &>/dev/null; then
      FORMAT=rpm
    elif command -v dpkg-deb &>/dev/null; then
      FORMAT=deb
    else
      echo "error: no supported package tool found."
      echo "  Fedora/COSMIC : sudo dnf install rpm-build"
      echo "  Debian/Ubuntu : dpkg-deb ships with dpkg (should already be present)"
      exit 1
    fi
    ;;
  *)
    echo "Usage: $0 [--deb|--rpm]"
    exit 1
    ;;
esac

echo "==> Format : $FORMAT"
echo "==> Version: $VERSION"

# ── Check build tool is present ───────────────────────────────────────────────

case "$FORMAT" in
  deb)
    if ! command -v dpkg-deb &>/dev/null; then
      echo "error: dpkg-deb not found. Install: sudo apt install dpkg"
      exit 1
    fi
    if ! command -v pkg-config &>/dev/null || ! pkg-config --exists fontconfig 2>/dev/null; then
      echo "error: fontconfig development libraries not found."
      echo "  Install via: sudo apt install -y pkg-config libfontconfig1-dev libxkbcommon-dev"
      exit 1
    fi
    ;;
  rpm)
    if ! command -v rpmbuild &>/dev/null; then
      echo "error: rpmbuild not found. Install: sudo dnf install rpm-build"
      exit 1
    fi
    if ! command -v pkg-config &>/dev/null || ! pkg-config --exists fontconfig 2>/dev/null; then
      echo "error: fontconfig development libraries not found."
      echo "  Install via: sudo dnf install -y pkgconf-pkg-config fontconfig-devel libxkbcommon-devel"
      exit 1
    fi
    ;;
esac

# ── Build release binaries ────────────────────────────────────────────────────

echo ""
echo "==> Building release binaries…"
cd "$REPO_ROOT"
cargo build --release --workspace

AGENT_BIN="$REPO_ROOT/target/release/manguesechee-agent"
UI_BIN="$REPO_ROOT/target/release/manguesechee-ui"
CLI_BIN="$REPO_ROOT/target/release/manguesechee-cli"
ICON_256="$REPO_ROOT/assets/icons/manguesechee-256.png"
ICON_48="$REPO_ROOT/assets/icons/manguesechee-48.png"
DESKTOP="$DIST_DIR/manguesechee-ui.desktop"
AUTOSTART="$DIST_DIR/manguesechee-autostart.desktop"
SERVICE="$DIST_DIR/manguesechee-agent.service"
UDEV="$DIST_DIR/99-manguesechee.rules"

for f in "$AGENT_BIN" "$UI_BIN" "$CLI_BIN"; do
  [[ -f "$f" ]] || { echo "error: binary not found: $f"; exit 1; }
done

# ── .deb ─────────────────────────────────────────────────────────────────────

build_deb() {
  local PKG="${NAME}_${VERSION}_${ARCH_DEB}"
  local STAGE="$DIST_DIR/.stage_deb/$PKG"

  rm -rf "$STAGE"

  install -Dm755 "$AGENT_BIN"  "$STAGE/usr/bin/manguesechee-agent"
  install -Dm755 "$UI_BIN"     "$STAGE/usr/bin/manguesechee-ui"
  install -Dm755 "$CLI_BIN"    "$STAGE/usr/bin/manguesechee-cli"
  install -Dm755 "$DIST_DIR/post-install.sh" "$STAGE/usr/lib/manguesechee/post-install.sh"
  install -Dm644 "$SERVICE"    "$STAGE/usr/lib/systemd/user/manguesechee-agent.service"
  install -Dm644 "$UDEV"       "$STAGE/usr/lib/udev/rules.d/99-manguesechee.rules"
  install -Dm644 "$DESKTOP"    "$STAGE/usr/share/applications/manguesechee-ui.desktop"
  install -Dm644 "$AUTOSTART"  "$STAGE/etc/xdg/autostart/manguesechee-ui.desktop"
  install -Dm644 "$AUTOSTART"  "$STAGE/usr/share/manguesechee/manguesechee-autostart.desktop"
  install -Dm644 "$ICON_256"   "$STAGE/usr/share/icons/hicolor/256x256/apps/manguesechee.png"
  install -Dm644 "$ICON_48"    "$STAGE/usr/share/icons/hicolor/48x48/apps/manguesechee.png"
  install -Dm644 "$REPO_ROOT/README.md" "$STAGE/usr/share/doc/manguesechee/README.md"

  mkdir -p "$STAGE/DEBIAN"

  cat > "$STAGE/DEBIAN/control" << EOF
Package: manguesechee
Version: $VERSION
Architecture: $ARCH_DEB
Maintainer: $MAINTAINER
Depends: libc6
Recommends: wl-clipboard, xclip, libnotify-bin
Description: $DESCRIPTION
Homepage: $URL
EOF

  cat > "$STAGE/DEBIAN/postinst" << 'EOF'
#!/bin/sh
set -e
if [ -x /usr/lib/manguesechee/post-install.sh ]; then
    /usr/lib/manguesechee/post-install.sh || true
fi
exit 0
EOF
  chmod 755 "$STAGE/DEBIAN/postinst"

  cat > "$STAGE/DEBIAN/prerm" << 'EOF'
#!/bin/sh
set -e
systemctl --user stop    manguesechee-agent 2>/dev/null || true
systemctl --user disable manguesechee-agent 2>/dev/null || true
exit 0
EOF
  chmod 755 "$STAGE/DEBIAN/prerm"

  local OUT="$DIST_DIR/${PKG}.deb"
  dpkg-deb --build --root-owner-group "$STAGE" "$OUT"
  rm -rf "$DIST_DIR/.stage_deb"

  echo ""
  echo "==> Package: $OUT"
  du -h "$OUT"
}

# ── .rpm ─────────────────────────────────────────────────────────────────────

build_rpm() {
  local RPMBUILD="$DIST_DIR/.stage_rpm"
  rm -rf "$RPMBUILD"
  mkdir -p "$RPMBUILD"/{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS}

  local SPEC="$RPMBUILD/SPECS/manguesechee.spec"

  cat > "$SPEC" << EOF
Name:           manguesechee
Version:        $VERSION
Release:        1%{?dist}
Summary:        $DESCRIPTION
License:        $LICENSE
URL:            $URL
BuildArch:      $ARCH_RPM
Recommends:     wl-clipboard, xclip, libnotify

%description
$DESCRIPTION

%install
install -Dm755 $AGENT_BIN                   %{buildroot}/usr/bin/manguesechee-agent
install -Dm755 $UI_BIN                      %{buildroot}/usr/bin/manguesechee-ui
install -Dm755 $CLI_BIN                     %{buildroot}/usr/bin/manguesechee-cli
install -Dm755 $DIST_DIR/post-install.sh    %{buildroot}/usr/lib/manguesechee/post-install.sh
install -Dm644 $SERVICE                     %{buildroot}/usr/lib/systemd/user/manguesechee-agent.service
install -Dm644 $UDEV                        %{buildroot}/usr/lib/udev/rules.d/99-manguesechee.rules
install -Dm644 $DESKTOP                     %{buildroot}/usr/share/applications/manguesechee-ui.desktop
install -Dm644 $AUTOSTART                   %{buildroot}/etc/xdg/autostart/manguesechee-ui.desktop
install -Dm644 $AUTOSTART                   %{buildroot}/usr/share/manguesechee/manguesechee-autostart.desktop
install -Dm644 $ICON_256                    %{buildroot}/usr/share/icons/hicolor/256x256/apps/manguesechee.png
install -Dm644 $ICON_48                     %{buildroot}/usr/share/icons/hicolor/48x48/apps/manguesechee.png
install -Dm644 $REPO_ROOT/README.md          %{buildroot}/usr/share/doc/manguesechee/README.md

%files
/usr/bin/manguesechee-agent
/usr/bin/manguesechee-ui
/usr/bin/manguesechee-cli
/usr/lib/manguesechee/post-install.sh
/usr/lib/systemd/user/manguesechee-agent.service
/usr/lib/udev/rules.d/99-manguesechee.rules
/usr/share/applications/manguesechee-ui.desktop
/etc/xdg/autostart/manguesechee-ui.desktop
/usr/share/manguesechee/manguesechee-autostart.desktop
/usr/share/icons/hicolor/256x256/apps/manguesechee.png
/usr/share/icons/hicolor/48x48/apps/manguesechee.png
/usr/share/doc/manguesechee/README.md

%post
if [ -x /usr/lib/manguesechee/post-install.sh ]; then
    /usr/lib/manguesechee/post-install.sh || true
fi

%preun
systemctl --user stop    manguesechee-agent 2>/dev/null || true
systemctl --user disable manguesechee-agent 2>/dev/null || true

%changelog
* $(date '+%a %b %d %Y') $MAINTAINER - $VERSION-1
- Initial package
EOF

  rpmbuild \
    --define "_topdir $RPMBUILD" \
    --define "_rpmdir $DIST_DIR" \
    --bb "$SPEC"

  rm -rf "$RPMBUILD"

  local OUT
  OUT=$(find "$DIST_DIR" -name "manguesechee-*.rpm" | sort | tail -1)
  echo ""
  echo "==> Package: $OUT"
  du -h "$OUT"
}

# ── Dispatch ──────────────────────────────────────────────────────────────────

case "$FORMAT" in
  deb) build_deb ;;
  rpm) build_rpm ;;
esac
