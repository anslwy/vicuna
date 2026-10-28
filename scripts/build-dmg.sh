#!/usr/bin/env bash
# Build the Vicuna macOS app (.app) and installer (.dmg) via Tauri.
#
# Usage:
#   scripts/build-dmg.sh [--debug] [--universal] [--bundles dmg,app] [-- <extra tauri build args...>]
#
# Examples:
#   scripts/build-dmg.sh                    # release .app + .dmg for this Mac (arm64/x86_64)
#   scripts/build-dmg.sh --debug            # faster debug bundle for smoke-testing
#   scripts/build-dmg.sh --universal        # universal (arm64 + x86_64) binary, needs both rust targets
#
# Output:
#   src-tauri/target/release/bundle/dmg/*.dmg
#   src-tauri/target/release/bundle/macos/*.app
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$PWD"

BUNDLES="dmg,app"
PROFILE="release"   # or "debug" with --debug
UNIVERSAL=0
EXTRA_ARGS=()

usage() {
  awk 'NR==1 && /^#!/ {next} /^#/ {sub(/^# ?/, ""); print; next} {exit}' "$0"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    --debug) PROFILE="debug"; shift ;;
    --universal) UNIVERSAL=1; shift ;;
    --bundles) BUNDLES="${2:?--bundles needs a value like 'dmg,app'}"; shift 2 ;;
    --) shift; EXTRA_ARGS+=("$@"); break ;;
    *) EXTRA_ARGS+=("$1"); shift ;;
  esac
done

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "error: this script builds a macOS .dmg and must run on macOS (uname: $(uname -s))" >&2
  exit 1
fi

for cmd in node npm cargo rustc; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "error: required tool '$cmd' not found in PATH" >&2
    exit 1
  fi
done

if [[ ! -f "src-tauri/tauri.conf.json" ]]; then
  echo "error: run from the repo root (src-tauri/tauri.conf.json not found)" >&2
  exit 1
fi

if [[ ! -d node_modules ]]; then
  echo "==> Installing npm dependencies..."
  npm install
fi

TARGET_DIR="src-tauri/target/release"
TARGET_ARGS=()
if [[ "$PROFILE" == "debug" ]]; then
  TARGET_DIR="src-tauri/target/debug"
fi

if [[ "$UNIVERSAL" -eq 1 ]]; then
  echo "==> Adding Rust targets for a universal build..."
  rustup target add aarch64-apple-darwin x86_64-apple-darwin
  TARGET_ARGS+=(--target universal-apple-darwin)
  TARGET_DIR="src-tauri/target/universal-apple-darwin/$PROFILE"
fi

echo "==> Building Vicuna for macOS ($PROFILE)..."
CMD=(npx tauri build)
if [[ "$PROFILE" == "debug" ]]; then
  CMD+=(--debug)
fi
CMD+=(--bundles "$BUNDLES")
if [[ "${#TARGET_ARGS[@]}" -gt 0 ]]; then
  CMD+=("${TARGET_ARGS[@]}")
fi
if [[ "${#EXTRA_ARGS[@]}" -gt 0 ]]; then
  CMD+=("${EXTRA_ARGS[@]}")
fi
"${CMD[@]}"

echo ""
echo "==> Build finished. Artifacts:"
if compgen -G "$TARGET_DIR/bundle/dmg/*.dmg" > /dev/null; then
  ls -lh "$TARGET_DIR"/bundle/dmg/*.dmg
else
  echo "  (no .dmg found under $TARGET_DIR/bundle/dmg — check the build log above)"
fi
if compgen -G "$TARGET_DIR/bundle/macos/*.app" > /dev/null; then
  ls -d "$TARGET_DIR"/bundle/macos/*.app
else
  echo "  (no .app found under $TARGET_DIR/bundle/macos — check the build log above)"
fi
