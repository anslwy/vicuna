#!/bin/sh
# Install the `vicuna` CLI (headless server + `vicuna open`) from a GitHub release.
#
#   curl -fsSL https://github.com/anslwy/vicuna/releases/latest/download/install-cli.sh | sh
#   vicuna serve --tailscale
#
# Env: VICUNA_VERSION=0.0.12 (default: latest release), VICUNA_INSTALL_DIR
# (default ~/.local/bin), VICUNA_FORCE=1 to replace a foreign `vicuna`.
# No sudo: everything lands in your home folder.
set -eu

REPO="anslwy/vicuna"
DIR="${VICUNA_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf '%s\n' "$*"; }
die() { printf 'vicuna install: %s\n' "$*" >&2; exit 1; }

os=$(uname -s)
arch=$(uname -m)
case "$os" in
  Linux)
    case "$arch" in
      x86_64 | amd64) target="linux-x86_64" ;;
      aarch64 | arm64) target="linux-aarch64" ;;
      *) die "unsupported Linux architecture: $arch" ;;
    esac ;;
  Darwin) target="macos-universal" ;;
  *) die "unsupported OS: $os (Linux and macOS are supported)" ;;
esac

if [ -n "${VICUNA_DOWNLOAD_BASE:-}" ]; then
  base="$VICUNA_DOWNLOAD_BASE"   # tests / mirrors
elif [ -n "${VICUNA_VERSION:-}" ]; then
  base="https://github.com/$REPO/releases/download/v${VICUNA_VERSION#v}"
else
  base="https://github.com/$REPO/releases/latest/download"
fi
asset="vicuna-$target.tar.gz"

if command -v curl >/dev/null 2>&1; then
  fetch() { curl -fsSL "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -q "$1" -O "$2"; }
else
  die "need curl or wget"
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

say "Downloading $asset ..."
fetch "$base/$asset" "$tmp/$asset" || die "no $target build in this release yet ($base/$asset). Supported: linux-x86_64, macos-universal (linux-aarch64 when available)."
if fetch "$base/$asset.sha256" "$tmp/$asset.sha256" 2>/dev/null; then
  want=$(cut -d' ' -f1 < "$tmp/$asset.sha256")
  if command -v sha256sum >/dev/null 2>&1; then
    got=$(sha256sum "$tmp/$asset" | cut -d' ' -f1)
  else
    got=$(shasum -a 256 "$tmp/$asset" | cut -d' ' -f1)
  fi
  [ "$want" = "$got" ] || die "checksum mismatch for $asset"
fi
tar -xzf "$tmp/$asset" -C "$tmp"
[ -f "$tmp/vicuna" ] || die "archive has no vicuna binary"

mkdir -p "$DIR"
dest="$DIR/vicuna"
if [ -e "$dest" ] && [ "${VICUNA_FORCE:-}" != "1" ]; then
  # ours: an earlier vicuna binary, or the old desktop launcher script
  if grep -q "Installed by Vicuna" "$dest" 2>/dev/null; then
    say "Replacing the old Vicuna launcher at $dest (vicuna open does its job now)."
  elif "$dest" --version 2>/dev/null | grep -q '^vicuna '; then
    :
  else
    die "$dest exists and isn't Vicuna's. Set VICUNA_INSTALL_DIR, or VICUNA_FORCE=1 to replace it."
  fi
fi
install -m 755 "$tmp/vicuna" "$dest" 2>/dev/null || { cp "$tmp/vicuna" "$dest" && chmod 755 "$dest"; }

# The pinned live-browser engine (`lb` + its web UI) for the Browser tab.
# `vicuna serve` looks for it in <prefix>/share/vicuna/lb-sidecar, so the
# server runs the lb it was released with, not whatever `lb` is on PATH.
share="$(cd "$DIR/.." && pwd)/share/vicuna"
if [ -d "$tmp/lb-sidecar" ]; then
  mkdir -p "$share"
  rm -rf "$share/lb-sidecar.new"
  cp -R "$tmp/lb-sidecar" "$share/lb-sidecar.new"
  # swap: a running lb keeps its open inode, the next start gets the new one
  rm -rf "$share/lb-sidecar"
  mv "$share/lb-sidecar.new" "$share/lb-sidecar"
fi

if [ "$os" = "Darwin" ]; then
  # unsigned download: drop the quarantine flag so Gatekeeper lets it run
  xattr -d com.apple.quarantine "$dest" 2>/dev/null || true
  [ -d "$share/lb-sidecar" ] && xattr -dr com.apple.quarantine "$share/lb-sidecar" 2>/dev/null || true
fi

say "Installed $("$dest" --version) to $dest"
[ -f "$share/lb-sidecar/LB_VERSION" ] && say "Browser engine: lb $(cat "$share/lb-sidecar/LB_VERSION") in $share/lb-sidecar"
case ":$PATH:" in
  *":$DIR:"*) ;;
  *) say ""; say "Add it to your PATH (then open a new shell):"; say "  echo 'export PATH=\"$DIR:\$PATH\"' >> ~/.profile" ;;
esac
say ""
say "Next:"
say "  vicuna serve --tailscale        # run a server for the Vicuna desktop app"
say "  vicuna install-service --tailscale   # keep it running in the background"
say "  vicuna status | start | stop | restart   # manage that background server"
say "  vicuna upgrade                  # install a newer release in place"
