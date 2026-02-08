#!/usr/bin/env bash
# bake_alpine_image.sh — Build a ready-to-run Alpine VM image with Firefox + Node.js
# Usage: ./bake_alpine_image.sh [--skip-download]
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
VHD_URL="https://dl-cdn.alpinelinux.org/alpine/latest-stable/releases/cloud/aws_alpine-3.23.3-aarch64-uefi-cloudinit-r0.vhd"
VHD_FILE="$SCRIPT_DIR/alpine-3.23.3-aarch64-uefi-cloudinit.vhd"
RAW_FILE="$SCRIPT_DIR/alpine-3.23.3-aarch64.raw"
SEED_FILE="$SCRIPT_DIR/seed.img"
IMAGE_SIZE="2G"
LIMA_INSTANCE="${LIMA_INSTANCE:-lima-default}"
LIMA_BAKE_DIR="/home/on.linux/alpine-bake"

# ── Helpers ───────────────────────────────────────────────────────────

log() { echo "==> $*"; }
die() { echo "ERROR: $*" >&2; exit 1; }

check_deps() {
  for cmd in qemu-img sgdisk mkisofs limactl; do
    command -v "$cmd" >/dev/null || die "Missing: $cmd"
  done
  command -v /opt/homebrew/opt/e2fsprogs/sbin/debugfs >/dev/null || die "Missing: e2fsprogs (brew install e2fsprogs)"
}

# ── Step 1: Download VHD ─────────────────────────────────────────────

download_vhd() {
  if [[ -f "$VHD_FILE" ]]; then
    log "VHD already exists: $(ls -lh "$VHD_FILE" | awk '{print $5}')"
  else
    log "Downloading Alpine VHD..."
    curl -fSL -o "$VHD_FILE" "$VHD_URL"
    log "Downloaded: $(ls -lh "$VHD_FILE" | awk '{print $5}')"
  fi
}

# ── Step 2: Convert + Resize + Partition ─────────────────────────────

prepare_image() {
  log "Converting VHD → raw..."
  qemu-img convert -f vpc -O raw "$VHD_FILE" "$RAW_FILE"
  
  log "Resizing to $IMAGE_SIZE..."
  qemu-img resize -f raw "$RAW_FILE" "$IMAGE_SIZE"
  
  log "Moving backup GPT..."
  sgdisk -e "$RAW_FILE" >/dev/null 2>&1
  
  log "Expanding partition 2..."
  sgdisk -d 2 -n 2:0:0 -t 2:8300 "$RAW_FILE" >/dev/null 2>&1
  
  log "Resizing ext4 filesystem..."
  local dev
  dev=$(hdiutil attach -imagekey diskimage-class=CRawDiskImage -nomount "$RAW_FILE" 2>/dev/null | grep "Linux Filesystem" | awk '{print $1}')
  [[ -n "$dev" ]] || die "Could not attach image"
  
  /opt/homebrew/opt/e2fsprogs/sbin/e2fsck -f -y "$dev" >/dev/null 2>&1 || true
  /opt/homebrew/opt/e2fsprogs/sbin/resize2fs "$dev" >/dev/null 2>&1
  
  log "Image partition expanded"
  
  # Apply GRUB console fix
  log "Applying GRUB console=hvc0 fix..."
  local grub_cfg
  grub_cfg=$(mktemp)
  cat > "$grub_cfg" << 'GRUB'
set default=0
set timeout=0

menuentry 'Alpine Linux' {
  linux /boot/vmlinuz-virt root=UUID=c7fe57a8-09c3-4dbc-b611-873eeee48718 rw modules=sd-mod,usb-storage,ext4 console=hvc0
  initrd /boot/initramfs-virt
}
GRUB
  /opt/homebrew/opt/e2fsprogs/sbin/debugfs -w -R "rm /boot/grub/grub.cfg" "$dev" >/dev/null 2>&1
  /opt/homebrew/opt/e2fsprogs/sbin/debugfs -w -R "write $grub_cfg /boot/grub/grub.cfg" "$dev" >/dev/null 2>&1
  rm -f "$grub_cfg"
  
  # Apply NoCloud datasource fix
  log "Applying NoCloud datasource fix..."
  local nocloud_cfg
  nocloud_cfg=$(mktemp)
  cat > "$nocloud_cfg" << 'NOCLOUD'
datasource_list: ['NoCloud', 'None']
datasource:
  NoCloud:
    fs_label: cidata
NOCLOUD
  /opt/homebrew/opt/e2fsprogs/sbin/debugfs -w -R "write $nocloud_cfg /etc/cloud/cloud.cfg.d/99_nocloud.cfg" "$dev" >/dev/null 2>&1
  rm -f "$nocloud_cfg"
  
  # Detach
  local disk_id
  disk_id=$(echo "$dev" | sed 's|/dev/||; s|s[0-9]*$||')
  hdiutil detach "/dev/$disk_id" >/dev/null 2>&1
  
  log "Image prepared: $(ls -lh "$RAW_FILE" | awk '{print $5}')"
}

# ── Step 3: Install packages via Lima chroot ─────────────────────────

bake_packages() {
  log "Baking packages via Lima chroot..."
  
  limactl shell "$LIMA_INSTANCE" -- sudo bash -c "
    set -e
    IMG=$LIMA_BAKE_DIR/alpine.raw
    MNT=$LIMA_BAKE_DIR/mnt

    mkdir -p \"\$(dirname \"\$IMG\")\" \"\$MNT\"
    cp '$RAW_FILE' \"\$IMG\"
    
    OFFSET=\$(fdisk -l \"\$IMG\" 2>/dev/null | grep \"^\${IMG}2\" | awk '{print \$2}')
    mount -o loop,offset=\$((OFFSET * 512)) \"\$IMG\" \"\$MNT\"
    mount -t proc proc \"\$MNT/proc\"
    mount -t sysfs sysfs \"\$MNT/sys\"
    mount --bind /dev \"\$MNT/dev\"
    cp /etc/resolv.conf \"\$MNT/etc/resolv.conf\"

    chroot \"\$MNT\" /bin/sh -c '
      apk add --no-cache firefox nodejs npm curl socat 2>&1 | tail -5
      echo \"firefox: \$(firefox --version 2>&1)\"
      echo \"node: \$(node --version)\"
      echo \"npm: \$(npm --version)\"
      echo \"socat: \$(socat -V 2>&1 | head -1)\"
    '

    echo '' > \"\$MNT/etc/resolv.conf\"
    chroot \"\$MNT\" /bin/sh -c 'rm -rf /var/cache/apk/*'
    
    echo 'Disk usage:'
    df -h \"\$MNT\"

    umount \"\$MNT/dev\" \"\$MNT/proc\" \"\$MNT/sys\" 2>/dev/null || true
    umount \"\$MNT\"
    cp \"\$IMG\" '$RAW_FILE'
  " 2>&1 | grep -v "^time=" | grep -v "Non-strict YAML"
  
  log "Packages baked"
}

# ── Step 4: Build seed ISO ───────────────────────────────────────────

build_seed() {
  log "Building seed ISO..."
  cd "$SCRIPT_DIR"
  rm -f "$SEED_FILE"
  mkisofs -output "$SEED_FILE" -volid cidata -joliet -rock user-data meta-data >/dev/null 2>&1
  log "Seed ISO: $(ls -lh "$SEED_FILE" | awk '{print $5}')"
}

# ── Main ─────────────────────────────────────────────────────────────

main() {
  log "Alpine VM Image Baker"
  check_deps
  
  if [[ "${1:-}" != "--skip-download" ]]; then
    download_vhd
  fi
  
  prepare_image
  bake_packages
  build_seed
  
  log "Done! Files:"
  ls -lh "$RAW_FILE" "$SEED_FILE"
  echo ""
  echo "Boot with:"
  echo "  avf_runner --efi --seed $SEED_FILE --disk $RAW_FILE \\"
  echo "    --shared-dir workspace --cpus 2 --memory-bytes 1073741824 \\"
  echo "    --vsock-port 5000 --bridge-port 5001"
}

main "$@"
