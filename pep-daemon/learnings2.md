# Milestone A2 Debugging Learnings

This file records the VM bring-up troubleshooting between A1/A2 and returning
to a working A0-style flow.

## Summary
- The VM failure was not the vsock stub or policy path; it was the VM boot path.
- The compiled Swift runner with virtualization entitlement is required; the
  Swift interpreter is not viable.
- Storage device attachment errors were a major source of failure when disk
  artifacts drifted or were converted incorrectly.
- EFI boot proved the most reliable path once the disk image and runner binary
  were stable.

## What we tried (and what failed)
- Running the Swift runner via `swift` or treating a `.swift` file as a script.
  This fails due to entitlement restrictions; the VM never starts.
- Boot attempts with broken `vmlinuz`/`initrd` artifacts (tiny 286-byte files).
  These cause immediate boot failure.
- Seed ISO rebuilds that accidentally captured the entire `ubuntu-img` directory,
  producing huge ISOs and confusing boot behavior.
- VM boots with stale `instance-id`, which prevents cloud-init from re-running.
- Repeated boot attempts without killing orphaned `avf_runner` processes, which
  can wedge the virtualization stack.

## What worked
- Compiling and codesigning `avf_runner` with `com.apple.security.virtualization`.
- Using a clean raw disk image derived from the Ubuntu cloud image.
- EFI boot with a valid `VZEFIVariableStore`.
- Fresh cloud-init seed ISO built from *only* `user-data` and `meta-data`.
- Bumping `instance-id` for every cloud-init change.
- Boot verification via `/workspace/boot-ok.txt` and `/workspace/vsock-fetch.out`.

## Key signals to watch
- `status.log` should show `VM state: 1` repeatedly.
- `boot-ok.txt` should update to a new timestamp on each successful boot.
- `vsock-fetch.out` should show `status=200` when allowlist matches.

## Operational guardrails (from pain)
- Always run the compiled runner binary (never `swift avf_runner.swift`).
- Always rebuild `seed.iso` from a minimal `cidata` directory.
- Always bump `instance-id` before testing cloud-init.
- Kill any orphaned `avf_runner` processes before boot attempts.
