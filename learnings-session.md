# Session Learnings – Chromium in VM (2026-02-07)

## Summary
Goal: Get Chromium running headless inside the AVF Linux VM for CDP interception.

## What was wrong

### 1. ldconfig cache stale after virt-customize --copy-in
The `bake_chromium_image.sh` used `virt-customize --install` (which runs apt and ldconfig)
followed by `--copy-in` to place Chromium bundle libraries into `/usr/lib/aarch64-linux-gnu/`.
The `--copy-in` happens AFTER `--install`, so ldconfig's cache never learns about the new
shared libraries. Result: `libasyncns.so.0: cannot open shared object file`.

**Fix**: append `; ldconfig` to the `--run-command` in the bake script, after the copy-in.

### 2. Chromium bundle glibc mismatch
`prepare_chromium_bundle.sh` downloads the Chromium binary from one Debian version but
helper libraries (libFLAC, libevent, libopus, libsndfile, etc.) from different Debian
versions. Several bundled libs require GLIBC ≥ 2.36 or 2.38, but Ubuntu 22.04 (Jammy)
ships GLIBC 2.35. Even with ldconfig fixed, Chromium still fails with missing `libFLAC.so.14`,
`libvorbis.so.0`, `libmpg123.so.0`, plus GLIBC version errors on libevent, libopus, etc.

**Root cause**: version skew between the Chromium binary (Debian bullseye, glibc ≤ 2.35)
and the extra libs (Debian trixie/sid, glibc ≥ 2.38).

### 3. Seed ISO format matters
`hdiutil makehybrid` creates a `.iso` file (not `.dmg`) and uses uppercase `CIDATA` label.
The old working seed was built with `mkisofs` (from `cdrtools`), lowercase `cidata`, 374 KB.
When the new seed was used, AVF rejected it with "The storage device attachment is invalid."

**Fix**: always use `mkisofs -output seed.img.dmg -volid cidata -joliet -rock user-data meta-data`.

### 4. Never chroot/loop-mount a raw image through virtiofs
Mounting a raw disk image via `loop` device on a file served by Lima's virtiofs causes
**severe ext4 corruption**. The journal/metadata flush semantics of ext4 are incompatible
with virtiofs's caching. Running `e2fsck -y` after such a mount deleted ~16 000 files,
including the entire Chromium installation and pexi-boot setup.

**Fix**: always copy the raw image into Lima's **local filesystem** first
(`cp --sparse=always`), do all loop-mount/chroot work there, then copy back.

### 5. Ubuntu 24.04 (Noble) solves glibc compat
Noble ships GLIBC 2.39. The Chromium snap (144.0.7559.109) and its libraries all work
on Noble. A Noble-based VM image was built and Chromium runs cleanly inside it.
The Jammy-based image also works when baked properly (apt deps + bundle + ldconfig)
with the Debian bullseye Chromium (120.0.6099.224), because that binary only needs
GLIBC ≤ 2.35.

## What works now

- **Jammy image** (`jammy-arm64-chromium.raw`): baked in Lima local FS with apt deps +
  Debian Chromium bundle + ldconfig. Chromium 120 runs headless, CDP intercept passes.
- **Noble image** (`noble-arm64-chromium.raw`): baked with apt deps + snap Chromium
  copied from Lima. Chromium 144 runs headless. Available as upgrade path.
- **CDP intercept spike** (`spikes/cdp-intercept/run.js`): launches headless Chromium,
  installs Fetch.requestPaused handler, navigates allow/deny URLs via PEP vsock mediation.
- **Vsock stub** running on host (port 4041), PEP_ALLOWED_DOMAINS controls access.

## Operational checklist (updated)

1. Kill orphaned `avf_runner` processes before any boot attempt.
2. Always bump `instance-id` in `meta-data` before rebuilding seed ISO.
3. Build seed with `mkisofs` (not `hdiutil`).
4. **Never** loop-mount images through virtiofs; copy to Lima local FS first.
5. After any `--copy-in` of shared libraries, ensure `ldconfig` runs inside the image.
6. Verify Chromium with `chromium-smoke.out` and `cdp-intercept.out` on workspace mount.
7. For Noble images: copy Chromium from `/snap/chromium/current/usr/lib/chromium-browser/`.

## File inventory (main repo, not worktree)

| File | Purpose |
|------|---------|
| `spikes/ubuntu-img/jammy-arm64-chromium.raw` | Baked Jammy image (working) |
| `spikes/ubuntu-img/noble-arm64-chromium.raw` | Baked Noble image (working) |
| `spikes/ubuntu-img/noble-arm64.raw` | Noble base (pre-bake) |
| `spikes/ubuntu-img/jammy-arm64.raw` | Jammy base (pre-bake) |
| `spikes/ubuntu-img/seed.img.dmg` | Cloud-init seed ISO |
| `spikes/ubuntu-img/chromium/` | Debian Chromium bundle |
| `spikes/ubuntu-img/node/` | Node.js arm64 Linux bundle |
| `spikes/ubuntu-img/pexi-boot.sh` | Boot-time diagnostics + CDP test trigger |
| `spikes/ubuntu-img/pexi-boot.service` | systemd unit for pexi-boot |
| `spikes/cdp-intercept/run.js` | CDP interception spike (raw CDP, no Playwright) |
| `spikes/cdp-intercept/package.json` | deps: chrome-remote-interface |

## Next steps

- Wire the CDP intercept to actually use vsock PEP mediation for every request
  (currently navigates but Fetch fulfillment needs the vsock stub).
- Decide Jammy vs Noble as the long-term base image.
- Integrate the bake into a reproducible script that runs entirely inside Lima local FS.
- Move from cloud-init runcmd to the baked pexi-boot.service for all boot-time setup.
