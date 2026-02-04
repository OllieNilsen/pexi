#!/usr/bin/env sh
set -eu

echo "Build host stub..."
cd /Users/on/p/pexi/pep-daemon
cargo build

echo "Start vsock stub (host terminal):"
echo "  ./target/debug/avf-vsock-host vsock-stub --cid 2 --port 4041"

echo "Boot VM (host terminal):"
echo "  ./target/debug/avf-vsock-host boot-vm \\"
echo "    --swift-script ./avf_runner.swift \\"
echo "    --kernel /path/to/vmlinuz \\"
echo "    --initrd /path/to/initrd \\"
echo "    --disk /path/to/disk.img \\"
echo "    --console-log /path/to/console.log \\"
echo "    --status-log /path/to/status.log \\"
echo "    --vsock-port 4040 \\"
echo "    --bridge-port 4041 \\"
echo "    --shared-dir /Users/on/p/pexi"

echo "Inside VM:"
echo "  /Users/on/p/pexi/spikes/vm-node-fetch/bootstrap.sh"
