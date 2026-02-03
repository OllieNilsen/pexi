#!/usr/bin/env sh
set -eu

echo "Disabling any IP networking (best-effort)..."
if command -v ip >/dev/null 2>&1; then
  ip link set dev eth0 down || true
  ip route del default || true
fi

echo "Network check (should fail):"
ping -c 1 example.com >/dev/null 2>&1 && echo "WARN: ping succeeded" || echo "OK: ping blocked"

export PEP_VSOCK_CID="${PEP_VSOCK_CID:-2}"
export PEP_VSOCK_PORT="${PEP_VSOCK_PORT:-4040}"
export PEP_VSOCK_CLIENT="${PEP_VSOCK_CLIENT:-/usr/local/bin/avf-vsock-host}"

echo "Running fetch proxy via host..."
node /workspace/spikes/vm-node-fetch/fetch_proxy.js https://example.com
