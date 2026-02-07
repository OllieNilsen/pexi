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
  --install libatomic1,libatk1.0-0,libatk-bridge2.0-0,libcups2,fontconfig,libxkbcommon0,libxcomposite1,libxdamage1,libxrandr2,libxfixes3,libxext6,libxrender1,libxcursor1,libxss1,libxi6,libxtst6,libnss3,libnspr4,libgtk-3-0,libgbm1,libasound2,libdrm2,libexpat1,libpango-1.0-0,libpangocairo-1.0-0,libcairo2,libatk1.0-0,libgdk-pixbuf2.0-0,libglu1-mesa,libegl1,libgles2,xdg-utils \
  --copy-in "$TMP_DIR/pexi-baked.txt":/etc \
  --copy-in "$ROOT_DIR/pexi-boot.sh":/tmp \
  --copy-in "$ROOT_DIR/pexi-boot.service":/tmp \
  --copy-in "$ROOT_DIR/rc.local":/tmp \
  --copy-in "$CHROMIUM_DIR/etc":/ \
  --copy-in "$CHROMIUM_DIR/usr":/ \
  --run-command "set -eu; install -m 0755 /tmp/pexi-boot.sh /usr/local/bin/pexi-boot; install -m 0644 /tmp/pexi-boot.service /etc/systemd/system/pexi-boot.service; install -m 0755 /tmp/rc.local /etc/rc.local; mkdir -p /etc/systemd/system/multi-user.target.wants /etc/systemd/system/graphical.target.wants; ln -sf /etc/systemd/system/pexi-boot.service /etc/systemd/system/multi-user.target.wants/pexi-boot.service; ln -sf /etc/systemd/system/pexi-boot.service /etc/systemd/system/graphical.target.wants/pexi-boot.service; if [ -f /lib/systemd/system/rc-local.service ]; then ln -sf /lib/systemd/system/rc-local.service /etc/systemd/system/multi-user.target.wants/rc-local.service; ln -sf /lib/systemd/system/rc-local.service /etc/systemd/system/graphical.target.wants/rc-local.service; fi; CHROME_BIN=\"\"; if [ -x /usr/bin/chromium ]; then CHROME_BIN=/usr/bin/chromium; elif [ -x /usr/lib/chromium/chromium ]; then CHROME_BIN=/usr/lib/chromium/chromium; elif [ -x /usr/lib/chromium/chrome ]; then CHROME_BIN=/usr/lib/chromium/chrome; fi; [ -n \"\$CHROME_BIN\" ] || (echo 'chromium binary not found under /usr/bin or /usr/lib/chromium' >&2; exit 1); ln -sf \"\$CHROME_BIN\" /usr/local/bin/chromium"

echo "Baked image ready: $BAKED_IMG"
