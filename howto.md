# Pexi How-To Guide

Consolidated operational knowledge for the Pexi VM sandbox. Covers the full stack: macOS host, AVF runner, Alpine VM, Firefox browser automation, vsock mediation, and PEP policy enforcement.

---

## Architecture Overview

```
┌─ VM (Alpine 3.23, no network) ─────────────────┐
│  Firefox 145 (headless, --remote-debugging-port)│
│       ↕ WebDriver BiDi (ws://127.0.0.1:9222)   │
│  Node.js + Puppeteer-core (request intercept)   │
│       ↕ TCP (127.0.0.1:4040)                    │
│  socat (TCP:4040 → VSOCK CID:2 port:5000)      │
│       ↕ AF_VSOCK                                │
└─────────────────────────────────────────────────┘
         ↕ virtio-vsock (port 5000)
┌─ Host (macOS Apple Silicon) ────────────────────┐
│  avf_runner SocketBridge (vsock → TCP:5001)      │
│       ↕ TCP (127.0.0.1:5001)                    │
│  PEP daemon (Rust: policy + fetch + audit)       │
│       ↕ reqwest HTTP client                      │
│  Internet (only via PEP)                         │
└──────────────────────────────────────────────────┘
```

**Key invariant**: the VM has no IP networking. All internet access is mediated through the vsock → PEP daemon chain. This is non-bypassable by design.

---

## 1. Prerequisites (macOS)

Install these via Homebrew:

```bash
brew install qemu gptfdisk cdrtools e2fsprogs lima
```

| Tool | Purpose |
|------|---------|
| `qemu-img` | Convert VHD→raw, resize disk images |
| `sgdisk` (from `gptfdisk`) | Expand GPT partition tables |
| `mkisofs` (from `cdrtools`) | Create cloud-init seed ISOs |
| `e2fsprogs` | `debugfs`, `e2fsck`, `resize2fs` for ext4 manipulation |
| Lima | Run a Linux VM for `chroot`-based package baking |

Also needed (already on macOS):
- `hdiutil` — attach raw disk images to expose partitions
- Xcode command line tools (for Swift compilation)

---

## 2. Building the AVF Runner

The AVF runner is a Swift binary that manages the VM lifecycle. It **must** be compiled and codesigned — the Swift interpreter does not work because it lacks the virtualization entitlement.

```bash
cd pep-daemon
# Compile
swiftc -O -o avf_runner avf_runner.swift \
  -framework Virtualization -framework Foundation

# Codesign with virtualization entitlement
cat > /tmp/entitlements.plist << 'XML'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>com.apple.security.virtualization</key><true/>
</dict>
</plist>
XML
codesign --force --sign - --entitlements /tmp/entitlements.plist avf_runner
```

**Gotcha**: running `swift avf_runner.swift` will fail with "The process doesn't have the `com.apple.security.virtualization` entitlement." Always use the compiled binary.

---

## 3. Building the PEP Daemon

The PEP daemon is a Rust binary:

```bash
cd pep-daemon
cargo build        # debug build
cargo build --release  # release build
```

Binary location: `pep-daemon/target/debug/avf-vsock-host` (or `release/`).

---

## 4. Building the Alpine VM Image

### Automated (recommended)

```bash
cd spikes/alpine-img
./bake_alpine_image.sh
```

This does everything: download → convert → resize → partition → fix GRUB → fix cloud-init → bake packages via Lima chroot → build seed ISO.

### Manual step-by-step

#### 4.1 Download and convert

```bash
# Download Alpine UEFI cloud image (VHD format, ~246MB)
curl -fSL -o alpine.vhd \
  https://dl-cdn.alpinelinux.org/alpine/latest-stable/releases/cloud/aws_alpine-3.23.3-aarch64-uefi-cloudinit-r0.vhd

# Convert VHD → raw
qemu-img convert -f vpc -O raw alpine.vhd alpine.raw
```

#### 4.2 Resize and expand partition

```bash
# Resize image file to 2GB
qemu-img resize -f raw alpine.raw 2G

# Move backup GPT to end of disk
sgdisk -e alpine.raw

# Delete and recreate partition 2 to fill available space
sgdisk -d 2 -n 2:0:0 -t 2:8300 alpine.raw

# Attach and resize the ext4 filesystem
hdiutil attach -imagekey diskimage-class=CRawDiskImage -nomount alpine.raw
# Note the /dev/diskNs2 device from output
/opt/homebrew/opt/e2fsprogs/sbin/e2fsck -f -y /dev/diskNs2
/opt/homebrew/opt/e2fsprogs/sbin/resize2fs /dev/diskNs2
```

#### 4.3 Fix GRUB console output

The AWS cloud image defaults to `ttyS0`/`ttyAMA0`. AVF uses virtio console (`hvc0`), so without this fix the VM appears to hang with zero output.

```bash
# Create fixed grub.cfg
cat > /tmp/alpine-grub.cfg << 'EOF'
set default=0
set timeout=0

menuentry 'Alpine Linux' {
  linux /boot/vmlinuz-virt root=UUID=c7fe57a8-09c3-4dbc-b611-873eeee48718 rw modules=sd-mod,usb-storage,ext4 console=hvc0
  initrd /boot/initramfs-virt
}
EOF

# Write to image via debugfs (while image is attached)
/opt/homebrew/opt/e2fsprogs/sbin/debugfs -w -R "rm /boot/grub/grub.cfg" /dev/diskNs2
/opt/homebrew/opt/e2fsprogs/sbin/debugfs -w -R "write /tmp/alpine-grub.cfg /boot/grub/grub.cfg" /dev/diskNs2
```

**Note**: `debugfs write` fails with "file already exists" if you don't `rm` first.

#### 4.4 Fix cloud-init datasource

The `aws_` prefixed image runs `ds-identify` which selects the EC2 datasource and tries to contact `169.254.169.254` for 240 seconds. Force NoCloud:

```bash
cat > /tmp/99_nocloud.cfg << 'EOF'
datasource_list: ['NoCloud', 'None']
datasource:
  NoCloud:
    fs_label: cidata
EOF

/opt/homebrew/opt/e2fsprogs/sbin/debugfs -w \
  -R "write /tmp/99_nocloud.cfg /etc/cloud/cloud.cfg.d/99_nocloud.cfg" /dev/diskNs2
```

Then detach:
```bash
hdiutil detach /dev/diskN
```

#### 4.5 Bake packages via Lima chroot

Packages cannot be installed via cloud-init (VM has no network). Use Lima to chroot into the image and run `apk add`.

**Critical**: always work on Lima's **local filesystem**, never through virtiofs.

```bash
limactl shell lima-default -- sudo bash -c '
  IMG=/home/on.linux/alpine-bake/alpine.raw
  MNT=/home/on.linux/alpine-bake/mnt

  # Copy image to Lima local FS
  cp /Users/on/p/pexi/spikes/alpine-img/alpine.raw "$IMG"

  # Mount partition 2
  OFFSET=$(fdisk -l "$IMG" | grep "${IMG}2" | awk "{print \$2}")
  mkdir -p "$MNT"
  mount -o loop,offset=$((OFFSET * 512)) "$IMG" "$MNT"

  # Set up chroot
  mount -t proc proc "$MNT/proc"
  mount -t sysfs sysfs "$MNT/sys"
  mount --bind /dev "$MNT/dev"
  cp /etc/resolv.conf "$MNT/etc/resolv.conf"

  # Install packages
  chroot "$MNT" apk add --no-cache firefox nodejs npm curl socat

  # Clean up
  echo "" > "$MNT/etc/resolv.conf"
  chroot "$MNT" rm -rf /var/cache/apk/*
  umount "$MNT/dev" "$MNT/proc" "$MNT/sys"
  umount "$MNT"

  # Copy back
  cp "$IMG" /Users/on/p/pexi/spikes/alpine-img/alpine.raw
'
```

#### 4.6 Build seed ISO

```bash
mkisofs -output seed.img -volid cidata -joliet -rock user-data meta-data
```

**Gotchas**:
- **Always use `mkisofs`**, not `hdiutil makehybrid` (which creates uppercase `CIDATA` label that can cause AVF rejection).
- **Always bump `instance-id`** in `meta-data` when changing `user-data`, otherwise cloud-init skips the new config.

---

## 5. Running the Full Flow

### Terminal 1: Start PEP daemon

```bash
PEP_ALLOWED_DOMAINS="example.com,github.com" \
  PEP_AUDIT_LOG=spikes/alpine-img/workspace/audit.jsonl \
  pep-daemon/target/debug/avf-vsock-host vsock-stub --port 5001
```

### Terminal 2: Boot VM

```bash
cd spikes/alpine-img
pep-daemon/avf_runner --efi \
  --seed seed.img \
  --disk alpine-3.23.3-aarch64.raw \
  --shared-dir workspace \
  --cpus 2 --memory-bytes 1073741824 \
  --vsock-port 5000 --bridge-port 5001
```

### What happens inside the VM (via cloud-init)

1. Workspace mounted via virtiofs
2. socat starts: `TCP-LISTEN:4040,fork,reuseaddr VSOCK-CONNECT:2:5000 &`
3. Firefox starts: `MOZ_HEADLESS=1 firefox --headless --remote-debugging-port=9222 &`
4. Intercept script runs: `node /workspace/intercept-test/test.mjs`

### AVF runner flags

| Flag | Purpose | Example |
|------|---------|---------|
| `--efi` | UEFI boot (required for Alpine) | |
| `--disk` | Main disk image | `alpine.raw` |
| `--seed` | Cloud-init seed ISO | `seed.img` |
| `--shared-dir` | Virtiofs share (tag: `workspace`) | `workspace/` |
| `--cpus` | vCPU count | `2` |
| `--memory-bytes` | RAM in bytes | `1073741824` (1GB) |
| `--vsock-port` | vsock port to listen on | `5000` |
| `--bridge-port` | Host TCP port to bridge to | `5001` |
| `--console-log` | Write console to file | `console.log` |

### PEP daemon environment variables

| Variable | Purpose | Example |
|----------|---------|---------|
| `PEP_ALLOWED_DOMAINS` | Comma-separated domain allowlist | `example.com,api.github.com` |
| `PEP_AUDIT_LOG` | Path to JSONL audit log | `audit.jsonl` |
| `PEP_MAX_REQUEST_BYTES` | Max request body size | `1048576` |
| `PEP_MAX_RESPONSE_BYTES` | Max response body size | `10485760` |

---

## 6. Device Mapping (with seed ISO)

When a seed ISO is provided, it appears as the first disk:

| Device | Contents |
|--------|----------|
| `/dev/vda` | Seed ISO (cidata label, ~370KB) |
| `/dev/vdb` | Main disk (vdb1 = EFI, vdb2 = ext4 root) |
| virtiofs `workspace` | Shared directory from host |

---

## 7. Browser Automation

### Firefox headless

```bash
MOZ_HEADLESS=1 firefox --headless --remote-debugging-port=9222 \
  --no-remote --disable-gpu --user-data-dir=/tmp/fx-profile
```

Log output: `WebDriver BiDi listening on ws://127.0.0.1:9222`

### WebSocket endpoints

| Path | Response | Use |
|------|----------|-----|
| `GET /` | HTTP 200, httpd.js info page | Not useful |
| `GET /json/*` | 404 | Firefox doesn't implement Chrome's JSON API |
| `ws://127.0.0.1:9222/session` | WebDriver BiDi | **Use this one** |

### Puppeteer-core (not Playwright)

**Key discovery**: Playwright requires its own patched glibc-based Firefox binary, which is incompatible with Alpine's musl libc. Puppeteer-core connects to stock Firefox via WebDriver BiDi and works perfectly.

```javascript
import puppeteer from 'puppeteer-core';

const browser = await puppeteer.connect({
  browserWSEndpoint: 'ws://127.0.0.1:9222/session',
  protocol: 'webDriverBiDi',
});

const page = await browser.newPage();
await page.setRequestInterception(true);

page.on('request', async (request) => {
  // Intercept and mediate via PEP
  await request.respond({ status: 200, body: '...' });
});
```

---

## 8. Vsock Communication Protocol

### Framing

All messages use a 4-byte big-endian length prefix followed by a JSON payload:

```
[4 bytes: payload length (BE u32)] [N bytes: JSON UTF-8]
```

### Request (VM → Host)

```json
{
  "method": "GET",
  "url": "https://example.com/path",
  "headers": [["accept", "text/html"], ["user-agent", "Mozilla/5.0"]],
  "body_base64": null
}
```

### Response (Host → VM)

Success:
```json
{
  "status": 200,
  "headers": [["content-type", "text/html"], ["server", "cloudflare"]],
  "body_base64": "PGh0bWw+Li4uPC9odG1sPg==",
  "error": null
}
```

Denied:
```json
{
  "status": 0,
  "headers": [],
  "body_base64": null,
  "error": {
    "code": "denied_by_policy",
    "message": "domain not allowlisted"
  }
}
```

### Error codes

| Code | Meaning |
|------|---------|
| `denied_by_policy` | Domain not in allowlist |
| `ssrf_blocked` | Target resolves to private/loopback/link-local IP |
| `redirect_blocked` | Redirect target failed policy check |
| `constraint_violation` | Request/response size exceeds limit |
| `invalid_method` | HTTP method not allowed |
| `invalid_url` | Malformed URL |
| `http_error` | Upstream HTTP error |

### Vsock bridge chain

```
VM Node.js  →  TCP 127.0.0.1:4040  →  socat  →  VSOCK CID:2 port:5000
    →  avf_runner SocketBridge  →  TCP 127.0.0.1:5001  →  PEP daemon
```

On macOS, host processes cannot bind `AF_VSOCK` directly. The avf_runner's `SocketBridge` accepts VM-initiated vsock connections and bridges them to a local TCP port where the PEP daemon listens.

---

## 9. Cloud-init

### user-data format

```yaml
#cloud-config
users:
  - name: alpine
    sudo: ALL=(ALL) NOPASSWD:ALL
    groups: users, wheel
    shell: /bin/ash
    lock_passwd: true
ssh_pwauth: false
runcmd:
  - mkdir -p /workspace
  - mount -t virtiofs workspace /workspace || true
  - echo "booted" > /workspace/boot-ok.txt
```

### meta-data format

```yaml
instance-id: alpine-001
local-hostname: pexi-alpine
```

### Rules

- **Always bump `instance-id`** when changing user-data. Cloud-init caches by instance-id and will silently skip re-runs otherwise.
- Build seed ISO with: `mkisofs -output seed.img -volid cidata -joliet -rock user-data meta-data`
- Don't use `hdiutil makehybrid` — it creates uppercase `CIDATA` which can fail.

---

## 10. Modifying Disk Images from macOS

### Using debugfs (for small edits)

```bash
# Attach image (find the Linux Filesystem partition)
hdiutil attach -imagekey diskimage-class=CRawDiskImage -nomount alpine.raw
# e.g. /dev/disk4s2

# Read a file
/opt/homebrew/opt/e2fsprogs/sbin/debugfs -R "cat /etc/hostname" /dev/disk4s2

# Write a file (must rm first if exists)
/opt/homebrew/opt/e2fsprogs/sbin/debugfs -w -R "rm /path/to/file" /dev/disk4s2
/opt/homebrew/opt/e2fsprogs/sbin/debugfs -w -R "write /local/file /path/in/image" /dev/disk4s2

# List directory
/opt/homebrew/opt/e2fsprogs/sbin/debugfs -R "ls /boot" /dev/disk4s2

# Detach
hdiutil detach /dev/disk4
```

### Using Lima chroot (for package installation)

See section 4.5 above.

### NEVER do this

**Never loop-mount a raw disk image through virtiofs.** The ext4 journal/metadata flush semantics are incompatible with virtiofs caching. This causes severe filesystem corruption — running `e2fsck -y` after such a mount has been observed to delete ~16,000 files.

**Always**: copy the image to Lima's local filesystem first, do all modifications there, then copy back.

---

## 11. Troubleshooting

### VM won't start: "storage device attachment is invalid"

- Check for stale processes: `lsof <image-file>` and kill any `com.apple.Virtualization.VirtualMachine` XPC processes.
- Ensure no other `avf_runner` process is running: `pkill -f avf_runner`
- Verify the image format: `qemu-img info <image-file>` (should say `raw`).

### VM starts but no console output

- The kernel console isn't set to `hvc0`. Fix the GRUB config (section 4.3).
- For Alpine UEFI images, you must use `--efi` flag.

### Cloud-init doesn't run

- Check `instance-id` — it must be different from previous boots.
- Verify the seed ISO has lowercase `cidata` volume label.
- Check for EC2 datasource timeout (240s) — apply the NoCloud fix (section 4.4).

### Firefox port 9222 not available

- Firefox takes 2-3 seconds to start listening. Use a port-wait loop before connecting.
- Check `firefox.log` in workspace for errors.

### PEP connection fails (ECONNREFUSED on port 4040)

- Ensure socat is running inside the VM: `socat TCP-LISTEN:4040,fork,reuseaddr VSOCK-CONNECT:2:5000 &`
- Ensure the PEP daemon is running on the host before booting the VM.
- Ensure bridge port matches: avf_runner `--bridge-port` must match PEP daemon `--port`.

### Orphaned processes after VM crash

```bash
pkill -9 -f avf_runner
pkill -9 -f avf-vsock-host
pkill -9 -f "com.apple.Virtualization"
```

Always kill these before restarting.

### Lima SSH connection reset

Lima VMs can enter an error state. Restart with:
```bash
limactl stop <instance> && limactl start <instance>
```

---

## 12. Why Alpine, Not Ubuntu

| Factor | Ubuntu | Alpine |
|--------|--------|--------|
| Image size | 23GB baked | ~900MB baked |
| Boot time | Minutes | ~10 seconds |
| Browser | Chromium (glibc issues) | Firefox (native musl) |
| glibc issues | Constant version mismatches | N/A (musl) |
| Package install | apt (slow, large) | apk (fast, small) |
| Automation lib | Playwright (bundled Chromium) | Puppeteer-core (stock Firefox) |

Ubuntu was used for initial spikes but suffered from:
1. **glibc version mismatches** — Chromium binaries required glibc ≥ 2.36/2.38 but Ubuntu 22.04 ships 2.35.
2. **ldconfig cache staleness** — `virt-customize --copy-in` places libraries after `--install` runs ldconfig, so the cache never learns about new shared objects.
3. **Massive image sizes** — 23GB for a baked Chromium image.
4. **Chromium has no aarch64 Alpine package** — only x86_64 in Alpine repos (musl incompatibility). Firefox is the correct choice for Alpine.

---

## 13. Baked Image Contents (Alpine)

| Package | Version | Size |
|---------|---------|------|
| Alpine Linux | 3.23.3 | base |
| Kernel | 6.18.7-0-virt | aarch64 |
| Firefox | 145.0 | 221MB |
| Node.js | 24.13.0 | 48MB |
| npm | 11.6.3 | included |
| curl | 8.17.0 | included |
| socat | 1.8.0.3 | included |
| cloud-init | 25.3 | included |
| **Total** | — | **~900MB** / 2GB partition |

---

## 14. Operational Checklist

Before every VM boot:
1. Kill orphaned `avf_runner` and `com.apple.Virtualization` processes.
2. Start the PEP daemon (`avf-vsock-host vsock-stub --port <N>`).
3. Bump `instance-id` in `meta-data` if `user-data` changed.
4. Rebuild seed ISO with `mkisofs`.
5. Clear workspace output files.

After image modifications:
1. Copy to Lima local FS (never modify through virtiofs).
2. Run `e2fsck` after modifications.
3. Detach with `hdiutil detach` before booting.

---

## 15. File Inventory

| File | Purpose |
|------|---------|
| `pep-daemon/avf_runner.swift` | AVF VM lifecycle manager (Swift) |
| `pep-daemon/avf_runner` | Compiled + codesigned binary |
| `pep-daemon/src/main.rs` | PEP daemon (Rust) — policy + fetch + audit |
| `spikes/alpine-img/bake_alpine_image.sh` | Automated image builder |
| `spikes/alpine-img/alpine-3.23.3-aarch64.raw` | Baked Alpine image |
| `spikes/alpine-img/seed.img` | Cloud-init seed ISO |
| `spikes/alpine-img/user-data` | Cloud-init user config |
| `spikes/alpine-img/meta-data` | Cloud-init instance metadata |
| `spikes/alpine-img/workspace/` | Virtiofs shared directory |
| `spikes/alpine-img/workspace/intercept-test/test.mjs` | Puppeteer intercept test |
| `spikes/browser-intercept/src/run.ts` | Playwright browser intercept spike |
| `spikes/cdp-intercept/run.js` | Raw CDP intercept spike |
| `spikes/cdp-intercept/vsock_pep.py` | Python vsock helper |
| `docs/hla.md` | High-level architecture (Arc42) |
| `docs/plan-mvp-vm-sandbox.md` | MVP implementation plan |
