#!/bin/bash
# Bump Vicuna's version everywhere it lives (ported from cojudge's
# scripts/update-version.sh): package.json + package-lock.json,
# src-tauri/Cargo.toml + Cargo.lock, and src-tauri/tauri.conf.json (the
# version the Tauri updater compares against latest.json).
#
#   scripts/update-version.sh 0.0.2
#
# Then merge to main and run Actions → "Build + release" with that version.
set -e

if [ -z "$1" ]; then
  echo "Error: No version provided."
  echo "Usage: ./scripts/update-version.sh <version>"
  exit 1
fi

VERSION=$1

if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]; then
  echo "Error: Invalid version format '$VERSION'. Must be semver (e.g. 0.0.3 or 1.0.0-beta.1)."
  exit 1
fi

cd "$(dirname "$0")/.."
echo "Updating version to $VERSION..."

# 1. package.json and package-lock.json
npm version "$VERSION" --no-git-tag-version --allow-same-version

# 2. src-tauri/Cargo.toml (the [package] version is the first `version =` line)
node -e "
const fs = require('fs');
const content = fs.readFileSync('src-tauri/Cargo.toml', 'utf8');
const updated = content.replace(/^version\s*=\s*\"[^\"]+\"/m, 'version = \"' + process.argv[1] + '\"');
fs.writeFileSync('src-tauri/Cargo.toml', updated);
" "$VERSION"

# 3. src-tauri/Cargo.lock (the vicuna package entry only)
if [ -f "src-tauri/Cargo.lock" ]; then
  node -e "
const fs = require('fs');
const content = fs.readFileSync('src-tauri/Cargo.lock', 'utf8');
const regex = /(name\s*=\s*\"vicuna\"\s*\r?\nversion\s*=\s*\")[^\"]+(\")/g;
const updated = content.replace(regex, '\$1' + process.argv[1] + '\$2');
fs.writeFileSync('src-tauri/Cargo.lock', updated);
" "$VERSION"
fi

# 4. src-tauri/tauri.conf.json
node -e "
const fs = require('fs');
const path = 'src-tauri/tauri.conf.json';
const content = fs.readFileSync(path, 'utf8');
const updated = content.replace(/(\"version\"\s*:\s*\")[^\"]+(\")/, '\$1' + process.argv[1] + '\$2');
if (JSON.parse(updated).version !== process.argv[1]) throw new Error('tauri.conf.json version not updated');
fs.writeFileSync(path, updated);
" "$VERSION"

echo "Successfully updated version in package.json, package-lock.json, Cargo.toml, Cargo.lock and tauri.conf.json to $VERSION"
