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
export PEP_PROJECT_ROOT="${PEP_PROJECT_ROOT:-/Users/on/p/pexi}"
export PEP_VSOCK_CLIENT="${PEP_VSOCK_CLIENT:-$PEP_PROJECT_ROOT/pep-daemon/target/debug/avf-vsock-host}"

ALLOW_URL="${PEP_TEST_ALLOW_URL:-https://example.com}"
DENY_URL="${PEP_TEST_DENY_URL:-}"

echo "Running fetch proxy via host..."
node "$PEP_PROJECT_ROOT/spikes/vm-node-fetch/fetch_proxy.js" "$ALLOW_URL"

if [ -n "$DENY_URL" ]; then
  echo "Running deny test via host..."
  set +e
  node "$PEP_PROJECT_ROOT/spikes/vm-node-fetch/fetch_proxy.js" "$DENY_URL"
  if [ "$?" -eq 0 ]; then
    echo "WARN: deny test succeeded"
  else
    echo "OK: deny test blocked"
  fi
  set -e
fi
