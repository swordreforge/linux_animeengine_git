#!/usr/bin/env bash
# Stage exactly what the Flatpak manifest installs: the four release binaries and
# the desktop assets. Avoids copying the multi-gigabyte `target/` tree into the
# build sandbox.
#
#   packaging/flatpak/stage.sh
#   flatpak run org.flatpak.Builder --user --install --force-clean \
#       packaging/flatpak/build-dir packaging/flatpak/io.github.swordreforge.AnimaEngine.yml
set -euo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
STAGE="$REPO/packaging/flatpak/staging"
BIN="$REPO/target/release"
BINS=(anima-overlay anima-overlay-ctl anima-workshop anima-i18n)

rm -rf "$STAGE"
mkdir -p "$STAGE/bin" "$STAGE/share/applications" "$STAGE/share/icons/hicolor/scalable/apps" "$STAGE/share/metainfo"

for bin in "${BINS[@]}"; do
  if [ ! -x "$BIN/$bin" ]; then
    echo "missing $BIN/$bin — run: cargo build --release --features overlay,bc7" >&2
    exit 1
  fi
  install -m755 "$BIN/$bin" "$STAGE/bin/$bin"
done

install -m644 "$REPO/packaging/anima-engine-overlay.desktop" "$STAGE/share/applications/"
install -m644 "$REPO/packaging/anima-engine-workshop.desktop" "$STAGE/share/applications/"
install -m644 "$REPO/packaging/anima-engine.svg" "$STAGE/share/icons/hicolor/scalable/apps/"
install -m644 "$REPO/packaging/io.github.swordreforge.AnimaEngine.metainfo.xml" "$STAGE/share/metainfo/"

echo "staged $(du -sh "$STAGE" | cut -f1) in $STAGE"
