#!/usr/bin/env bash
# Portable, user-local install for AnimaEngine's Rust port.
#
# Nothing is built here: it installs binaries that already exist (default
# `target/release`) plus the desktop entry, icon, AppStream metadata and — if a
# catalog is given — the primary-language translations. Everything lands under
# PREFIX (default ~/.local), so no root is needed.
#
#   packaging/install.sh [--bin-dir DIR] [--catalog FILE] [--niri]
#                        [--no-validate] [--uninstall]
#
#   --bin-dir     where the built binaries are (default: target/release)
#   --catalog     anima_translations.json to install (`anima-i18n install --primary`)
#   --niri        also write ~/.config/niri/anima.kdl (autostart snippet)
#   --no-validate skip desktop-file-validate / appstreamcli / cache updates
#   --uninstall   remove everything this script installs
set -euo pipefail

PREFIX="${PREFIX:-$HOME/.local}"
BIN_SRC="target/release"
CATALOG=""
WRITE_NIRI=0
UNINSTALL=0
VALIDATE=1
BINS=(anima-overlay anima-overlay-ctl anima-workshop anima-i18n anima-shepherd)
# Built with `--features studio` (it pulls the whole iced tree). Installed only when the
# binary exists, so an overlay-only build keeps working.
OPTIONAL_BINS=(anima-studio)

while [ $# -gt 0 ]; do
  case "$1" in
    --prefix) PREFIX="$2"; shift 2 ;;
    --bin-dir) BIN_SRC="$2"; shift 2 ;;
    --catalog) CATALOG="$2"; shift 2 ;;
    --niri) WRITE_NIRI=1; shift ;;
    --no-validate) VALIDATE=0; shift ;;
    --uninstall) UNINSTALL=1; shift ;;
    -h|--help) sed -n '2,19p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

REPO="$(cd "$(dirname "$0")/.." && pwd)"
DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
APPS="$DATA_HOME/applications"
ICONS="$DATA_HOME/icons/hicolor/scalable/apps"
METAINFO="$DATA_HOME/metainfo"
METAINFO_FILE="$METAINFO/io.github.swordreforge.AnimaEngine.metainfo.xml"
NIRI_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/niri"

if [ "$UNINSTALL" = 1 ]; then
  for bin in "${BINS[@]}"; do rm -f "$PREFIX/bin/$bin"; done
  rm -f "$APPS/anima-engine-overlay.desktop" "$APPS/anima-engine-workshop.desktop" \
    "$APPS/anima-engine-studio.desktop"
  rm -f "$ICONS/anima-engine.svg" "$ICONS/anima-engine-studio.svg" "$METAINFO_FILE" "$NIRI_DIR/anima.kdl"
  echo "removed the installed files (translation catalog and instance state kept)"
  exit 0
fi

for bin in "${BINS[@]}"; do
  source_binary="$BIN_SRC/$bin"
  if [ ! -x "$source_binary" ]; then
    echo "missing $source_binary — build first, e.g. cargo build --release --features overlay,bc7,dbus" >&2
    exit 1
  fi
  install -Dm755 "$source_binary" "$PREFIX/bin/$bin"
  echo "bin      $PREFIX/bin/$bin"
done

INSTALLED_STUDIO=0
for bin in "${OPTIONAL_BINS[@]}"; do
  source_binary="$BIN_SRC/$bin"
  if [ -x "$source_binary" ]; then
    install -Dm755 "$source_binary" "$PREFIX/bin/$bin"
    echo "bin      $PREFIX/bin/$bin"
    INSTALLED_STUDIO=1
  else
    echo "skipped  $bin — build with: cargo build --release --features overlay,bc7,dbus,studio"
  fi
done

install -Dm644 "$REPO/packaging/anima-engine-overlay.desktop" "$APPS/anima-engine-overlay.desktop"
install -Dm644 "$REPO/packaging/anima-engine-workshop.desktop" "$APPS/anima-engine-workshop.desktop"
if [ "$INSTALLED_STUDIO" = 1 ]; then
  install -Dm644 "$REPO/packaging/anima-engine-studio.desktop" "$APPS/anima-engine-studio.desktop"
  install -Dm644 "$REPO/packaging/anima-engine-studio.svg" "$ICONS/anima-engine-studio.svg"
  echo "desktop  $APPS/anima-engine-studio.desktop"
  echo "icon     $ICONS/anima-engine-studio.svg"
fi
install -Dm644 "$REPO/packaging/anima-engine.svg" "$ICONS/anima-engine.svg"
install -Dm644 "$REPO/packaging/io.github.swordreforge.AnimaEngine.metainfo.xml" "$METAINFO_FILE"
echo "desktop  $APPS/anima-engine-overlay.desktop"
echo "icon     $ICONS/anima-engine.svg"
echo "appdata  $METAINFO_FILE"

if [ -n "$CATALOG" ]; then
  "$PREFIX/bin/anima-i18n" install "$CATALOG" --primary
fi

if [ "$WRITE_NIRI" = 1 ]; then
  mkdir -p "$NIRI_DIR"
  sed "s|@BIN@|$PREFIX/bin|g" "$REPO/packaging/niri-anima.kdl" > "$NIRI_DIR/anima.kdl"
  echo "niri     $NIRI_DIR/anima.kdl (add: include \"anima.kdl\")"
fi

if [ "$VALIDATE" = 1 ]; then
  if command -v desktop-file-validate >/dev/null 2>&1; then
    entries=("$APPS/anima-engine-overlay.desktop" "$APPS/anima-engine-workshop.desktop")
    [ "$INSTALLED_STUDIO" = 1 ] && entries+=("$APPS/anima-engine-studio.desktop")
    desktop-file-validate "${entries[@]}" && echo "desktop-file-validate: ok"
  fi
  if command -v appstreamcli >/dev/null 2>&1; then
    appstreamcli validate --no-net "$METAINFO_FILE" && echo "appstreamcli: ok"
  fi
  command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$APPS" || true
  command -v gtk-update-icon-cache >/dev/null 2>&1 \
    && gtk-update-icon-cache -q -t -f "$DATA_HOME/icons/hicolor" || true
fi

echo "done. $( [ -n "$CATALOG" ] && echo "catalog installed; " )run: anima-overlay <animation.apng>"
