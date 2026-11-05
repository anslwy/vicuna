#!/bin/sh
# Fetch the pinned `fx` (Vercel Labs coding agent) release into
# src-tauri/fx-sidecar/ so Work mode can spawn `fx acp` without requiring a
# user install. fx is a single native Zig binary (~4-6 MB), macOS + Linux
# only (no Windows release).
#
#   scripts/fetch-fx-sidecar.sh                 # pinned FX_VERSION
#   FX_VERSION=v0.0.17 scripts/fetch-fx-sidecar.sh   # override (skips checksum)
#
# Layout produced:
#   src-tauri/fx-sidecar/
#     fx                (linux)
#     fx-arm64 / fx-x64 (macOS universal builds; backend picks by arch)
#     FX_VERSION        (pin marker; makes the fetch idempotent)
#
# Windows: fx ships no Windows binary; the script exits 0 without staging so
# a Windows developer build still works (Work mode falls back to the native
# agent there).

set -eu

PINNED_VERSION="v0.0.16"
VERSION="${FX_VERSION:-$PINNED_VERSION}"
PINNED=1
if [ "$VERSION" != "$PINNED_VERSION" ]; then PINNED=0; fi

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="$ROOT/src-tauri/fx-sidecar"

OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
  MINGW*|MSYS*|CYGWIN*|Windows_NT)
    echo "fx-sidecar: fx has no Windows release; skipping (native Work agent stays available)."
    exit 0
    ;;
  Darwin) PLATFORM="macos" ;;
  Linux) PLATFORM="linux" ;;
  *) echo "fetch-fx-sidecar: unsupported OS: $OS" >&2; exit 1 ;;
esac

case "$ARCH" in
  arm64|aarch64) ARCH_TAG="aarch64" ;;
  x86_64|amd64) ARCH_TAG="x86_64" ;;
  *) echo "fetch-fx-sidecar: unsupported arch: $ARCH" >&2; exit 1 ;;
esac

# Idempotent: skip when the pin matches and binaries are present.
if [ -f "$DEST/FX_VERSION" ] && [ "$(cat "$DEST/FX_VERSION")" = "$VERSION" ]; then
  if ls "$DEST"/fx "$DEST"/fx-arm64 "$DEST"/fx-x64 >/dev/null 2>&1; then
    echo "fx-sidecar: $VERSION already present, skipping."
    exit 0
  fi
fi

if [ -f "$DEST/FX_VERSION" ] && [ "$(cat "$DEST/FX_VERSION")" != "$VERSION" ]; then
  rm -f "$DEST/fx" "$DEST/fx-arm64" "$DEST/fx-x64"
fi

# Pinned SHA-256 per asset (from the release .sha256 files).
sha_for() {
  case "$1" in
    fx-macos-aarch64.tar.gz) echo "2158a8d42e77457a774400d225d9802bb6c0e60a0bb0674a1116bc31252490d6" ;;
    fx-macos-x86_64.tar.gz) echo "b287974f97a0879f50b4439c46537fcd44b83326c11a230d87f596073f8d8d9b" ;;
    fx-linux-aarch64.tar.gz) echo "ca9441b104357e2105039dbfd4c2eaf0d6ee65a84085f23f51e880c95cbf46af" ;;
    fx-linux-x86_64.tar.gz) echo "f0fb822c7be546c80bcc89d50521bc1454435e0c6f586e8420c4e2b84fa80599" ;;
    *) echo "" ;;
  esac
}

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

fetch_asset() {
  ASSET="$1"
  URL="https://github.com/vercel-labs/fx/releases/download/$VERSION/$ASSET"
  echo "fx-sidecar: fetching $ASSET ..."
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$URL" -o "$TMP/$ASSET"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO "$TMP/$ASSET" "$URL"
  else
    echo "fetch-fx-sidecar: need curl or wget" >&2; exit 1
  fi
  if [ "$PINNED" = "1" ]; then
    WANT="$(sha_for "$ASSET")"
    if [ -z "$WANT" ]; then echo "fetch-fx-sidecar: no pinned checksum for $ASSET" >&2; exit 1; fi
    if command -v sha256sum >/dev/null 2>&1; then
      GOT="$(sha256sum "$TMP/$ASSET" | cut -d' ' -f1)"
    else
      GOT="$(shasum -a 256 "$TMP/$ASSET" | cut -d' ' -f1)"
    fi
    if [ "$GOT" != "$WANT" ]; then
      echo "fetch-fx-sidecar: checksum mismatch for $ASSET" >&2; exit 1
    fi
  else
    echo "fx-sidecar: WARNING: unpinned $VERSION, skipping checksum." >&2
  fi
}

stage_one() {
  ASSET="fx-$PLATFORM-$1.tar.gz"
  OUT="$2"
  fetch_asset "$ASSET"
  mkdir -p "$TMP/$1"
  tar -xzf "$TMP/$ASSET" -C "$TMP/$1"
  # release archives contain `fx` (or `fx.exe`); take the first executable
  BIN="$(find "$TMP/$1" -type f -name 'fx*' | head -n 1)"
  if [ -z "$BIN" ]; then echo "fx-sidecar: $ASSET had no fx binary" >&2; exit 1; fi
  cp "$BIN" "$DEST/$OUT"
  chmod +x "$DEST/$OUT"
}

mkdir -p "$DEST"

if [ "$PLATFORM" = "macos" ]; then
  # Universal app: both arch binaries, picked at runtime.
  stage_one aarch64 fx-arm64
  stage_one x86_64 fx-x64
else
  stage_one "$ARCH_TAG" fx
fi

echo "$VERSION" > "$DEST/FX_VERSION"
echo "fx-sidecar: staged $VERSION into src-tauri/fx-sidecar/"
ls -la "$DEST"
