#!/bin/sh
set -eu

REPO="${VICUNA_REPO:-anslwy/vicuna}"
TAG="${1:-${VICUNA_VERSION:-}}"
OS="$(uname -s)"
ARCH="$(uname -m)"

for cmd in curl grep mktemp; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "Error: missing required command: $cmd" >&2
    exit 1
  fi
done

find_asset_url() {
  api_url="$1"
  pattern="$2"
  curl -fsSL "$api_url" 2>/dev/null \
    | grep -Eo "https://github\\.com/${REPO}/releases/download/[^\"]+${pattern}" \
    | head -n 1 || true
}

resolve_asset_url() {
  pattern="$1"
  url=""
  if [ -n "$TAG" ]; then
    url="$(find_asset_url "https://api.github.com/repos/${REPO}/releases/tags/${TAG}" "$pattern")"
  else
    url="$(find_asset_url "https://api.github.com/repos/${REPO}/releases/latest" "$pattern")"
    if [ -z "$url" ]; then
      url="$(find_asset_url "https://api.github.com/repos/${REPO}/releases?per_page=5" "$pattern")"
    fi
  fi
  printf '%s' "$url"
}

TMP_DIR="$(mktemp -d)"
MOUNT_DIR=""
cleanup() {
  if [ -n "$MOUNT_DIR" ] && [ -d "$MOUNT_DIR" ]; then
    hdiutil detach "$MOUNT_DIR" -quiet >/dev/null 2>&1 || true
  fi
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT INT TERM

install_macos() {
  for cmd in hdiutil ditto; do
    if ! command -v "$cmd" >/dev/null 2>&1; then
      echo "Error: missing required command: $cmd" >&2
      exit 1
    fi
  done

  dmg_url="$(resolve_asset_url '\.dmg')"
  if [ -z "$dmg_url" ]; then
    echo "Error: could not find a macOS .dmg release asset for ${REPO}${TAG:+ ($TAG)}." >&2
    exit 1
  fi

  dmg_path="$TMP_DIR/Vicuna.dmg"
  echo "Downloading $dmg_url..."
  curl -fL --progress-bar "$dmg_url" -o "$dmg_path"

  MOUNT_DIR="$TMP_DIR/mnt"
  mkdir -p "$MOUNT_DIR"
  hdiutil attach "$dmg_path" -nobrowse -readonly -mountpoint "$MOUNT_DIR" -quiet

  app_src="$MOUNT_DIR/Vicuna.app"
  if [ ! -d "$app_src" ]; then
    app_src="$(find "$MOUNT_DIR" -maxdepth 2 -name "*.app" -type d | head -n 1 || true)"
  fi
  if [ -z "$app_src" ] || [ ! -d "$app_src" ]; then
    echo "Error: Vicuna.app not found inside DMG." >&2
    exit 1
  fi

  install_dir="${VICUNA_INSTALL_DIR:-/Applications}"
  if [ ! -w "$install_dir" ]; then
    install_dir="$HOME/Applications"
    mkdir -p "$install_dir"
  fi

  app_dest="$install_dir/Vicuna.app"
  echo "Installing to $app_dest..."
  rm -rf "$app_dest"
  ditto "$app_src" "$app_dest"

  if xattr -p com.apple.quarantine "$app_dest" >/dev/null 2>&1; then
    xattr -dr com.apple.quarantine "$app_dest" 2>/dev/null || true
  fi

  echo "Installed Vicuna to $app_dest"
  echo "You can now open Vicuna from Applications or Spotlight."
}

install_linux() {
  case "$ARCH" in
    x86_64|amd64) ;;
    *)
      echo "Error: unsupported Linux architecture '$ARCH' (only x86_64/amd64 builds are published)." >&2
      exit 1
      ;;
  esac

  format="${VICUNA_LINUX_FORMAT:-appimage}"
  case "$format" in
    deb)
      deb_url="$(resolve_asset_url '_amd64\.deb')"
      if [ -z "$deb_url" ]; then
        echo "Error: could not find a Linux .deb release asset for ${REPO}${TAG:+ ($TAG)}." >&2
        exit 1
      fi
      deb_path="$TMP_DIR/vicuna_amd64.deb"
      echo "Downloading $deb_url..."
      curl -fL --progress-bar "$deb_url" -o "$deb_path"
      if command -v apt-get >/dev/null 2>&1; then
        sudo apt-get install -y "$deb_path"
      else
        sudo dpkg -i "$deb_path"
      fi
      echo "Installed Vicuna (.deb). Launch it with: vicuna"
      ;;
    appimage)
      appimage_url="$(resolve_asset_url '_amd64\.AppImage')"
      if [ -z "$appimage_url" ]; then
        echo "Error: could not find a Linux .AppImage release asset for ${REPO}${TAG:+ ($TAG)}." >&2
        exit 1
      fi
      install_dir="${VICUNA_INSTALL_DIR:-$HOME/.local/bin}"
      mkdir -p "$install_dir"
      app_dest="$install_dir/vicuna"
      echo "Downloading $appimage_url..."
      curl -fL --progress-bar "$appimage_url" -o "$TMP_DIR/Vicuna.AppImage"
      chmod +x "$TMP_DIR/Vicuna.AppImage"
      mv -f "$TMP_DIR/Vicuna.AppImage" "$app_dest"
      echo "Installed Vicuna to $app_dest"
      case ":${PATH:-}:" in
        *":$install_dir:"*) echo "Launch it with: vicuna" ;;
        *) echo "Add $install_dir to your PATH, then launch it with: vicuna" ;;
      esac
      ;;
    *)
      echo "Error: unknown VICUNA_LINUX_FORMAT '$format' (expected 'appimage' or 'deb')." >&2
      exit 1
      ;;
  esac
}

case "$OS" in
  Darwin) install_macos ;;
  Linux) install_linux ;;
  *)
    echo "Error: unsupported OS '$OS'. On Windows, use install.ps1 in PowerShell." >&2
    exit 1
    ;;
esac
