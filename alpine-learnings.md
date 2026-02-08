# Alpine VM Learnings (2026-02-08)

## Summary
Goal: Replace Ubuntu with Alpine Linux as the VM base image for the PEP sandbox.
Status: **Complete end-to-end flow working.** Alpine boots → Firefox runs headless → Puppeteer intercepts all requests → vsock sends to host PEP daemon → PEP evaluates policy (allow/deny) → fetches content → returns via vsock → browser renders.

## What works (all verified)
- **Alpine 3.23.3** aarch64 UEFI cloud image boots in AVF runner (~10s)
- **Firefox 145.0** headless with WebDriver BiDi at `ws://127.0.0.1:9222/session`
- **Node.js 24.13.0** + npm 11.6.3 (native musl, no glibc shim)
- **Puppeteer-core** connects via BiDi, intercepts all network requests
- **socat** bridges local TCP:4040 → VSOCK CID:2 port:5000
- **avf_runner** bridges VSOCK port 5000 → host TCP:5001
- **PEP daemon** (Rust) on host: policy evaluation + HTTP fetch + audit logging
- **Allowed requests**: `example.com` → 200, rendered "Example Domain" ✅
- **Denied requests**: `evil.com` → 403, `denied_by_policy` ✅
- **Audit trail**: JSONL log captures every request with decision, status, bytes
- **No network in VM**: all requests mediated — no IP stack, no DNS, no direct internet

## Architecture (proven working)

```
┌─ VM (Alpine, no network) ──────────────────────┐
│  Firefox 145 (headless, --remote-debugging-port)│
│       ↕ WebDriver BiDi (ws://127.0.0.1:9222)   │
│  Node.js + Puppeteer-core (request intercept)   │
│       ↕ TCP (127.0.0.1:4040)                    │
│  socat (TCP:4040 → VSOCK CID:2 port:5000)      │
│       ↕ AF_VSOCK                                │
└─────────────────────────────────────────────────┘
         ↕ virtio-vsock (port 5000)
┌─ Host (macOS) ──────────────────────────────────┐
│  avf_runner SocketBridge (vsock → TCP:5001)      │
│       ↕ TCP (127.0.0.1:5001)                    │
│  PEP daemon (Rust: policy + fetch + audit)       │
│       ↕ reqwest HTTP client                      │
│  Internet (only via PEP)                         │
└──────────────────────────────────────────────────┘
```

## Protocol
- **Framing**: 4-byte big-endian length prefix + JSON payload
- **Request** (VM → Host): `{method, url, headers: [[k,v]...], body_base64}`
- **Response** (Host → VM): `{status, headers: [[k,v]...], body_base64, error}`
- **Error**: `{error: {code: "denied_by_policy"|"ssrf_blocked"|..., message: "..."}}`

## Key discovery: Puppeteer-core, not Playwright
- **Playwright** requires its own patched Firefox binary (glibc-based, incompatible with Alpine musl)
- **Puppeteer-core** connects to stock Firefox via WebDriver BiDi — works perfectly
- Connect endpoint: `ws://127.0.0.1:9222/session` (NOT root path `/`)
- `page.setRequestInterception(true)` + `request.respond()` works for full mediation

## Baked image contents
| Package | Version | Size |
|---------|---------|------|
| Firefox | 145.0 | 221MB |
| Node.js | 24.13.0 | 48MB |
| npm | 11.6.3 | included |
| curl | 8.17.0 | included |
| socat | 1.8.0.3 | included |
| **Total image** | — | **~900MB** (2GB partition, 45% used) |

## Image preparation
1. Download VHD: `curl -fSL -o alpine.vhd <url>`
2. Convert: `qemu-img convert -f vpc -O raw alpine.vhd alpine.raw`
3. Resize: `qemu-img resize -f raw alpine.raw 2G`
4. Expand GPT: `sgdisk -e alpine.raw && sgdisk -d 2 -n 2:0:0 -t 2:8300 alpine.raw`
5. Resize ext4: `hdiutil attach ... -nomount` → `e2fsck -f -y` → `resize2fs`
6. Apply GRUB fix: `debugfs -w` to set `console=hvc0`
7. Apply NoCloud fix: `debugfs -w` to write `99_nocloud.cfg`
8. Bake packages: Lima chroot → `apk add firefox nodejs npm curl socat`
9. Automated: `./spikes/alpine-img/bake_alpine_image.sh`

## Running the full flow
```bash
# Terminal 1: Start PEP daemon
PEP_ALLOWED_DOMAINS="example.com" \
  PEP_AUDIT_LOG=workspace/audit.jsonl \
  avf-vsock-host vsock-stub --port 5001

# Terminal 2: Boot VM
avf_runner --efi \
  --seed seed.img --disk alpine-3.23.3-aarch64.raw \
  --shared-dir workspace \
  --cpus 2 --memory-bytes 1073741824 \
  --vsock-port 5000 --bridge-port 5001
```

Inside VM (cloud-init user-data):
```bash
socat TCP-LISTEN:4040,fork,reuseaddr VSOCK-CONNECT:2:5000 &
MOZ_HEADLESS=1 firefox --headless --remote-debugging-port=9222 ... &
node /workspace/intercept-test/test.mjs
```

## Firefox WebSocket endpoints
- `GET /` → HTTP 200 (httpd.js info page)
- `GET /json/*` → 404 (not Chrome)
- `ws://127.0.0.1:9222/session` → WebDriver BiDi ✅

## Key fixes applied to image

### 1. GRUB console (`console=hvc0`)
AWS cloud image defaults to `ttyS0`/`ttyAMA0`. AVF needs `hvc0`.

### 2. Cloud-init NoCloud datasource
AWS image's `ds-identify` tries EC2 metadata (240s timeout). Force `NoCloud`.

### 3. EFI boot
Alpine cloud image is UEFI-only. Use `--efi` flag.

## Tools used on macOS
- `qemu-img` — convert VHD→raw, resize
- `sgdisk` (gptfdisk) — expand GPT partition
- `hdiutil` — attach raw image partitions
- `e2fsprogs` — debugfs, e2fsck, resize2fs
- `mkisofs` — seed ISO
- Lima VM — chroot for package installation

## Filesystem corruption warning
**Never loop-mount raw images through virtiofs.** Always:
1. Copy to Lima's local filesystem
2. Modify there
3. Copy back

## Next steps
1. **Create pexi-boot service** — OpenRC init script to start socat + Firefox + intercept runner on boot (replace cloud-init runcmd)
2. **Persist intercept script in image** — bake the Node.js intercept runner into the image instead of relying on virtiofs
3. **Port from puppeteer-core to the existing browser-intercept spike** — unify the intercept code
4. **Vsock optimization** — replace socat with a native Node.js vsock addon or a small C bridge for lower latency
5. **Test with complex pages** — verify request interception handles redirects, CORS, WebSocket upgrades
6. **OPA policy integration** — replace simple domain allowlist with OPA policy engine

## File inventory
| File | Purpose |
|------|---------|
| `spikes/alpine-img/bake_alpine_image.sh` | Automated image builder |
| `spikes/alpine-img/alpine-3.23.3-aarch64.raw` | Baked image (Firefox+Node+socat) |
| `spikes/alpine-img/seed.img` | Cloud-init seed ISO |
| `spikes/alpine-img/user-data` | Cloud-init config |
| `spikes/alpine-img/meta-data` | Cloud-init metadata |
| `spikes/alpine-img/workspace/intercept-test/` | Puppeteer intercept test |
| `alpine-learnings.md` | This file |
