#!/usr/bin/env sh
set -eu

ROOT_DIR="$(cd "$(dirname "$0")" && pwd)"
BASE_IMG="${ROOT_DIR}/jammy-arm64.raw"
BAKED_IMG="${ROOT_DIR}/jammy-arm64-chromium.raw"
CHROMIUM_DIR="${ROOT_DIR}/chromium"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

if [ ! -f "$BASE_IMG" ]; then
  echo "Missing base image: $BASE_IMG" >&2
  exit 1
fi

if ! command -v virt-customize >/dev/null 2>&1; then
  echo "virt-customize not found. Run this on a Linux host with libguestfs." >&2
  exit 1
fi

if [ -z "${LIBGUESTFS_BACKEND:-}" ]; then
  export LIBGUESTFS_BACKEND=direct
fi

if [ ! -d "$CHROMIUM_DIR" ]; then
  echo "Missing Chromium bundle at: $CHROMIUM_DIR" >&2
  echo "Run: $ROOT_DIR/prepare_chromium_bundle.sh" >&2
  exit 1
fi

cp "$BASE_IMG" "$BAKED_IMG"

echo "Installing Chromium bundle into baked image..."
echo "baked-at=$(date -u +%Y-%m-%dT%H:%M:%SZ)" >"$TMP_DIR/pexi-baked.txt"
virt-customize -a "$BAKED_IMG" \
  --install libatomic1 \
  --copy-in "$TMP_DIR/pexi-baked.txt":/etc \
  --copy-in "$CHROMIUM_DIR":/opt \
  --run-command "set -eu; CHROME_BIN=\$(find /opt/chromium -type f -name chrome -perm -111 | head -n 1); [ -n \"\$CHROME_BIN\" ] || (echo 'chrome binary not found under /opt/chromium' >&2; exit 1); ln -sf \"\$CHROME_BIN\" /usr/local/bin/chromium"

echo "Baked image ready: $BAKED_IMG"
