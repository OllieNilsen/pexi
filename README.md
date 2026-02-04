# pexi

Mac-first local product that runs Moltbot/OpenClaw inside a Linux VM with **no IP
networking**. All side effects are routed through a **host Policy Enforcement
Point (PEP)** in Rust, evaluated by OPA with **deny-by-default** policy and
append-only, redacted audit logs.

## Architecture invariants (do not break)
- macOS Apple Silicon only (MVP).
- VM has no IP networking; only vsock/virtio-serial to host.
- Host PEP is the only component with external network access.
- OPA policies are deny-by-default with explicit allowlists.
- All HTTP side effects go through `pep.http.request` and are policy-checked.
- Audit logs are append-only JSONL with redaction.

## Repo layout
- `pep-daemon/` — Host PEP stub (Rust). Current focus for Milestone A1.
- `spikes/` — Pre-work spikes and experiments.
- `docs/` — Architecture and planning docs.
- `config.toml` — Single source of truth for commands/paths/guardrails.

## Quick start (spike flow)
The repo is currently in the **spikes** stage. The main host stub is in
`pep-daemon/`.

### Build and test the PEP stub
```
cargo fmt --manifest-path "pep-daemon/Cargo.toml" --all
cargo clippy --manifest-path "pep-daemon/Cargo.toml" --all-targets --all-features -- -D warnings
cargo test --manifest-path "pep-daemon/Cargo.toml" --all
```

### Run the host stub
```
cargo run --manifest-path "pep-daemon/Cargo.toml" -- vsock-stub --cid 2 --port 4041
```

### Boot the VM (host)
```
cargo run --manifest-path "pep-daemon/Cargo.toml" -- boot-vm \
  --swift-script ./pep-daemon/avf_runner.swift \
  --kernel /path/to/vmlinuz \
  --initrd /path/to/initrd \
  --disk /path/to/disk.img \
  --console-log /path/to/console.log \
  --status-log /path/to/status.log \
  --vsock-port 4040 \
  --bridge-port 4041 \
  --shared-dir /Users/on/p/pexi
```

### Inside the VM
```
/Users/on/p/pexi/spikes/vm-node-fetch/bootstrap.sh
```

### Reproducible A1b verification
Set allow/deny URLs so the VM bootstrap exercises both code paths:
```
export PEP_TEST_ALLOW_URL="https://example.com"
export PEP_TEST_DENY_URL="https://example.org"
```

## PEP environment configuration
These environment variables control the PEP HTTP stub:

- `PEP_ALLOWED_DOMAINS` — comma-separated allowlist (required; deny-by-default).
- `PEP_MAX_REQUEST_BYTES` — request body cap (default 5MB).
- `PEP_MAX_RESPONSE_BYTES` — response body cap (default 10MB).
- `PEP_MAX_REDIRECTS` — max redirects (default 5).
- `PEP_AUDIT_LOG` — JSONL audit log path.

## Notes
- The validate script is a best-effort spike helper at `spikes/validate_spike.sh`.
- See `docs/plan-mvp-vm-sandbox.md` for milestones and acceptance criteria.
