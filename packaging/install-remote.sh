#!/bin/sh
# AnimaEngine one-liner installer: builds from source, then installs user-locally.
#
#   curl -fsSL <url-to-this-script> | sh
#   curl -fsSL <url> | sh -s -- --prefix "$HOME/.local" --no-bc7
#   curl -fsSL <url> | sh -s -- --studio        # also build the main window (Iced)
#
# Environment:
#   ANIMA_REPO_URL   git repository to build from (required unless run from a checkout)
#   ANIMA_REF        branch or tag to build (default: main)
#   ANIMA_PREFIX     install prefix (default: ~/.local)
#   ANIMA_NO_BC7=1   skip the BC7 encoder (much faster build, no C++ compiler needed)
#
# It never uses sudo. Needs: git, cargo (rustup), and a C++ compiler when building
# BC7; libwayland and a Vulkan driver are runtime requirements of the overlay.
set -eu

PREFIX="${ANIMA_PREFIX:-$HOME/.local}"
REF="${ANIMA_REF:-main}"
REPO_URL="${ANIMA_REPO_URL:-}"
CATALOG=""
BC7=1
STUDIO=0
DRY=0

usage() {
    sed -n '2,17p' "$0" 2>/dev/null || printf '%s\n' \
        "usage: curl -fsSL <url> | sh -s -- [--prefix DIR] [--ref REF] [--repo URL] [--no-bc7] [--studio] [--catalog FILE]"
    exit 0
}

while [ $# -gt 0 ]; do
    case "$1" in
        --prefix) PREFIX="$2"; shift 2 ;;
        --ref) REF="$2"; shift 2 ;;
        --repo) REPO_URL="$2"; shift 2 ;;
        --catalog) CATALOG="$2"; shift 2 ;;
        --no-bc7) BC7=0; shift ;;
        --studio) STUDIO=1; shift ;;
        --dry-run) DRY=1; shift ;;
        -h|--help) usage ;;
        *) printf 'unknown argument: %s\n' "$1" >&2; exit 2 ;;
    esac
done

say() { printf '%s\n' "$*"; }

MISSING=0
need() {
    if ! command -v "$1" >/dev/null 2>&1; then
        say "missing prerequisite: $1${2:+ — $2}"
        MISSING=1
    fi
}
need git "needed to fetch the sources"
need cargo "install it from https://rustup.rs"
if [ "$BC7" = 1 ]; then
    if ! command -v c++ >/dev/null 2>&1 && ! command -v g++ >/dev/null 2>&1 \
        && ! command -v clang++ >/dev/null 2>&1; then
        say "missing prerequisite: a C++ compiler — needed by the BC7 encoder (pass --no-bc7 to skip it)"
        MISSING=1
    fi
fi
[ "$MISSING" = 0 ] || exit 1

TMP=""
cleanup() { [ -n "$TMP" ] && rm -rf "$TMP"; }
trap cleanup EXIT INT TERM

SELF_DIR=$(dirname "$0")
if [ -f "$SELF_DIR/../Cargo.toml" ] && grep -q 'name = "anima-engine-rs"' "$SELF_DIR/../Cargo.toml" 2>/dev/null; then
    SRC=$(cd "$SELF_DIR/.." && pwd)
    say "building from the checkout at $SRC"
else
    if [ -z "$REPO_URL" ]; then
        say "set ANIMA_REPO_URL (or pass --repo URL) to the repository to build from"
        exit 1
    fi
    TMP=$(mktemp -d)
    say "cloning $REPO_URL ($REF)"
    if ! git clone --depth 1 --branch "$REF" "$REPO_URL" "$TMP/src" >/dev/null 2>&1; then
        say "clone failed — check ANIMA_REPO_URL and ANIMA_REF"
        exit 1
    fi
    SRC="$TMP/src"
fi

FEATURES="overlay,dbus"
[ "$BC7" = 1 ] && FEATURES="overlay,bc7,dbus"
[ "$STUDIO" = 1 ] && FEATURES="$FEATURES,studio"

if [ "$DRY" = 1 ]; then
    say "dry run: cargo build --release --features $FEATURES"
    say "dry run: install into $PREFIX from $SRC/packaging/install.sh"
    exit 0
fi

say "building ($FEATURES) — this can take a few minutes"
( cd "$SRC" && cargo build --release --features "$FEATURES" ) || { say "build failed"; exit 1; }

if [ -n "$CATALOG" ]; then
    sh "$SRC/packaging/install.sh" --prefix "$PREFIX" --catalog "$CATALOG"
else
    sh "$SRC/packaging/install.sh" --prefix "$PREFIX"
fi

say ""
say "installed into $PREFIX/bin."
say "niri autostart: sh $SRC/packaging/install.sh --prefix $PREFIX --niri"
say "  then add  include \"anima.kdl\"  to ~/.config/niri/config.kdl"
say "run: $PREFIX/bin/anima-overlay /path/to/animation.apng"
