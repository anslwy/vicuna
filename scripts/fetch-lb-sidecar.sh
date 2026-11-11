#!/bin/sh
# Fetch the pinned `lb` (live-browser) release into src-tauri/lb-sidecar/
# for bundling with Vicuna (Browser tab sidecar + `lb mcp` agent tools).
#
#   scripts/fetch-lb-sidecar.sh            # pinned LB_VERSION (default below)
#   LB_VERSION=v0.5.0 scripts/fetch-lb-sidecar.sh   # override (skips checksum)
#
# Layout produced:
#   src-tauri/lb-sidecar/
#     lb            (linux), lb.exe (windows), lb-arm64 + lb-x64 (macOS universal)
#     web/dist/     (the lb web UI served by `lb web`; shared by both mac archs)
#     LB_VERSION    (pin marker; makes the fetch idempotent)
#
# macOS builds are universal, but an `lb` binary is arch-specific, so both
# darwin assets are fetched and the backend picks by runtime arch at spawn.

set -eu

PINNED_VERSION="v0.4.20"
VERSION="${LB_VERSION:-$PINNED_VERSION}"
PINNED=1
if [ "$VERSION" != "$PINNED_VERSION" ]; then PINNED=0; fi

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="$ROOT/src-tauri/lb-sidecar"

# Idempotent: skip when the pin marker already matches and a binary is present.
if [ -f "$DEST/LB_VERSION" ] && [ "$(cat "$DEST/LB_VERSION")" = "$VERSION" ]; then
  if ls "$DEST"/lb* >/dev/null 2>&1; then
    echo "lb-sidecar: $VERSION already present, skipping."
    exit 0
  fi
fi
# Version change: wipe the previous stage (binaries + UI) so nothing stale
# survives the bump (the darwin "first arch wins" UI copy below would
# otherwise keep the old web/dist next to new binaries).
if [ -f "$DEST/LB_VERSION" ] && [ "$(cat "$DEST/LB_VERSION")" != "$VERSION" ]; then
  rm -rf "$DEST"/lb "$DEST"/lb.exe "$DEST"/lb-arm64 "$DEST"/lb-x64 "$DEST"/web
fi

OS="$(uname -s)"
ARCH="$(uname -m)"
EXT="tar.gz"

case "$OS" in
  Darwin) PLATFORM="darwin" ;;
  Linux) PLATFORM="linux" ;;
  MINGW*|MSYS*|CYGWIN*|Windows_NT) PLATFORM="windows"; EXT="zip" ;;
  *) echo "fetch-lb-sidecar: unsupported OS: $OS" >&2; exit 1 ;;
esac

# Pinned SHA-256 per asset (v0.4.20, computed from the downloaded release assets).
sha_for() {
  case "$1" in
    live-browser-v0.4.20-darwin-arm64.tar.gz) echo "8785b05f12893cb2cb878961da3e2107664bbd88a4031b74ae57818bd073e02a" ;;
    live-browser-v0.4.20-darwin-x86_64.tar.gz) echo "36c6c6782ec17ba79456964ee8b9e9d129545941450435e365bc6298ffab7ab3" ;;
    live-browser-v0.4.20-linux-x86_64.tar.gz) echo "9f4abb6926bc098da0982783290cae99b6fb1204f813e512f9da550b0b814202" ;;
    live-browser-v0.4.20-windows-x86_64.zip) echo "864ba8b2a07e0bd98d22995be57eb5da5d559235b1039e1ebf9c86eac33b08d7" ;;
    *) echo "" ;;
  esac
}

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

fetch_asset() {
  ASSET="$1"
  URL="https://github.com/anslwy/live-browser/releases/download/$VERSION/$ASSET"
  echo "lb-sidecar: fetching $ASSET ..."
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$URL" -o "$TMP/$ASSET"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO "$TMP/$ASSET" "$URL"
  else
    echo "fetch-lb-sidecar: need curl or wget" >&2; exit 1
  fi
  if [ "$PINNED" = "1" ]; then
    WANT="$(sha_for "$ASSET")"
    if [ -z "$WANT" ]; then echo "fetch-lb-sidecar: no pinned checksum for $ASSET" >&2; exit 1; fi
    if command -v sha256sum >/dev/null 2>&1; then
      GOT="$(sha256sum "$TMP/$ASSET" | cut -d' ' -f1)"
    else
      GOT="$(shasum -a 256 "$TMP/$ASSET" | cut -d' ' -f1)"
    fi
    if [ "$GOT" != "$WANT" ]; then
      echo "fetch-lb-sidecar: checksum mismatch for $ASSET" >&2; exit 1
    fi
  else
    echo "lb-sidecar: WARNING: unpinned $VERSION, skipping checksum." >&2
  fi
}

mkdir -p "$DEST"

# In-run flag (NOT filesystem state: must re-copy on version bumps even
# when a stale web/dist already exists — see the wipe above).
UI_STAGED=0

if [ "$PLATFORM" = "darwin" ]; then
  for A in arm64 x86_64; do
    ASSET="live-browser-$VERSION-darwin-$A.tar.gz"
    fetch_asset "$ASSET"
    tar -xzf "$TMP/$ASSET" -C "$TMP"
    SRC="$TMP/live-browser-$VERSION-darwin-$A"
    SUFFIX="x64"
    if [ "$A" = "arm64" ]; then SUFFIX="arm64"; fi
    cp "$SRC/lb" "$DEST/lb-$SUFFIX"
    chmod +x "$DEST/lb-$SUFFIX"
    # web/dist is identical across archs; first one wins
    if [ "$UI_STAGED" = "0" ]; then
      mkdir -p "$DEST/web"
      rm -rf "$DEST/web/dist"
      cp -r "$SRC/web/dist" "$DEST/web/dist"
      UI_STAGED=1
    fi
  done
elif [ "$PLATFORM" = "windows" ]; then
  ASSET="live-browser-$VERSION-windows-x86_64.zip"
  fetch_asset "$ASSET"
  # Git Bash tar is GNU tar (no zip support) — unzip first, PowerShell
  # Expand-Archive as fallback (needs Windows paths via cygpath).
  if command -v unzip >/dev/null 2>&1; then
    unzip -q "$TMP/$ASSET" -d "$TMP"
  elif command -v powershell.exe >/dev/null 2>&1 && command -v cygpath >/dev/null 2>&1; then
    WTMP="$(cygpath -w "$TMP")"
    powershell.exe -NoProfile -NonInteractive -Command "Expand-Archive -LiteralPath '${WTMP}\\${ASSET}' -DestinationPath '${WTMP}'"
  else
    echo "fetch-lb-sidecar: need unzip or powershell to extract $ASSET" >&2
    exit 1
  fi
  SRC="$TMP/live-browser-$VERSION-windows-x86_64"
  cp "$SRC/lb.exe" "$DEST/lb.exe"
  mkdir -p "$DEST/web"
  rm -rf "$DEST/web/dist"
  cp -r "$SRC/web/dist" "$DEST/web/dist"
else
  case "$ARCH" in
    x86_64) ;;
    *) echo "fetch-lb-sidecar: no linux asset for $ARCH (x86_64 only)" >&2; exit 1 ;;
  esac
  ASSET="live-browser-$VERSION-linux-x86_64.tar.gz"
  fetch_asset "$ASSET"
  tar -xzf "$TMP/$ASSET" -C "$TMP"
  SRC="$TMP/live-browser-$VERSION-linux-x86_64"
  cp "$SRC/lb" "$DEST/lb"
  chmod +x "$DEST/lb"
  mkdir -p "$DEST/web"
  rm -rf "$DEST/web/dist"
  cp -r "$SRC/web/dist" "$DEST/web/dist"
fi

echo "$VERSION" > "$DEST/LB_VERSION"
echo "lb-sidecar: staged $VERSION into src-tauri/lb-sidecar/"
ls -la "$DEST" "$DEST/web" | head -n 20
