# Milestone A0 Learnings

This file captures all learnings from the vsock + fetch mediation spike.

## AVF / macOS constraints
- AVF requires the `com.apple.security.virtualization` entitlement. Running `swift` in interpreter mode fails with: "The process doesn’t have the com.apple.security.virtualization entitlement."
- The AVF runner must be compiled and codesigned (ad-hoc signing works) with that entitlement.
- On macOS, host processes cannot bind `AF_VSOCK` directly; attempting to bind with the Rust `vsock` crate returns "Operation not supported by device."
- The practical workaround is: use AVF's vsock listener inside the VM host process and bridge vsock traffic to a TCP listener on `127.0.0.1` (host stub uses TCP on macOS).
- Multiple previous `boot-vm` attempts can leave orphaned `avf_runner` processes; ensure they are killed before restarting to avoid confusion.

## AVF boot configuration
- EFI boot requires:
  - `VZGenericPlatformConfiguration` (platform config must be set),
  - a `VZEFIVariableStore` (error: "variableStore is nil" otherwise).
- For EFI, kernel/initrd are not required; the VM can boot from the disk image.
- For Linux boot (non-EFI), kernel + initrd must be provided and validated.

## Console output and logging
- Serial console output did not appear on stdout or a log file, even after `console=hvc0` / `console=ttyAMA0` and `earlycon` variations.
- `VZFileSerialPortAttachment` is the most reliable host-side logging option, but the guest still may not emit to the configured console.
- To verify boot progress, use cloud-init in the guest to write marker files into the shared workspace (e.g., `/workspace/boot-ok.txt`, `/workspace/boot-dmesg.txt`).

## Cloud-init / seed ISO
- On macOS, `cloud-image-utils` is not available; use `cdrtools` (`mkisofs`) or `hdiutil makehybrid` to build the `seed.iso`.
- Changing `user-data` requires a new `instance-id` in `meta-data`, otherwise cloud-init may skip the updated config.
- A small `sleep` in `runcmd` can help ensure host services (vsock bridge) are ready before running the vsock test.

## Ubuntu image details
- Use Ubuntu cloud image (arm64) for speed of setup:
  - `*.img` (disk), `*.vmlinuz` (kernel), `*.initrd` (initrd) from `cloud-images.ubuntu.com`.
- Convert the `.img` to raw for AVF (`qemu-img convert -O raw`).

## Vsock mediation behavior
- VM network is disabled (`ip=none`), but vsock still works.
- The vsock bridge path is:
  - VM client -> vsock port in AVF -> host AVF listener -> TCP stub on `127.0.0.1:4041` -> host HTTP fetch.
- Initial failures (BrokenPipe) were due to the vsock client attempting a connection before the host stub was running or ready; retry logic resolves this.

## Fetch mediation verification
- Successful test captured in `/workspace/vsock-fetch.out`:
  - `status=200` and HTML response from `https://example.com`.
- This confirms: no IP networking in VM, vsock mediation works, and host-only egress is enforced.

## Tooling lessons
- `openssl passwd -6` is not available on macOS OpenSSL; use Python `crypt` or skip password auth.
- When running the compiled AVF runner, use the binary directly (do not invoke with `swift`), or you will see "invalid UTF-8 found in source file" errors.

## Operational checklist (good defaults)
- Always start the host stub before the VM.
- Use cloud-init markers for boot verification rather than relying on console output.
- Track VM state logs with a status file to confirm the VM is running (state=1).
