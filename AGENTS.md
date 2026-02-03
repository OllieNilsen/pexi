# AGENTS.md

This repository builds a **Mac-first local product** that runs Moltbot/OpenClaw inside a **Linux VM with no IP networking**, and routes **all side effects** through a **trusted host PEP (Rust)** enforced by **OPA** with **deny-by-default** policy and **auditable, policy-gated side effects**. [[11]]

This file defines how coding agents (human or AI) must operate in this repo: **small iterations**, **review gates**, **types-first**, **tests-first**, Rust best practices, and security guardrails.

> Single source of truth for commands, CI checks, paths, and thresholds lives in `config.toml`.
> `AGENTS.md` describes *process*; `config.toml` describes *mechanics*.

---

## 0) Architectural invariants (do not break)

These are non-negotiable and must hold in every iteration:

1. **macOS Apple Silicon only (MVP)** and **no reduced security settings**. [[11]]
2. **VM has no IP networking**; VM↔host communication is via **vsock/virtio-serial** only. [[11]]
3. The **host PEP** is the **only** component with external network access. [[11]]
4. Policies are **deny-by-default** with explicit user allowlists. [[11]]
5. All HTTP side effects are routed via `pep.http.request` and evaluated by OPA before executing. [[11]]
6. Audit logging is **append-only JSONL with redaction**. [[11]]
7. CLI provides local-first UX for lifecycle/policy/logs: `start/stop/status/policy/logs`. [[11]]

If a requested change would violate an invariant: **stop**, explain the conflict, and ask for direction.

---

## 1) Working style: SMALL iterations + explicit review gates

### 1.1 Micro-iteration contract (required)
All work happens in micro-iterations that are safe to review:

**Each micro-iteration must:**
- introduce **one** behavioral change or **one** capability,
- include tests (or a justified seam to enable testing),
- avoid drive-by refactors,
- be reversible (clear rollback).

**Workflow**
1. **Plan (pre-code)**: post a short plan:
   - Goal (1 sentence)
   - Files/modules/crates to touch
   - Data model changes (if any)
   - Tests to add
   - Risk + rollback
2. **Stop for approval** (you review the plan).
3. **Implement** minimal change.
4. **Run required checks** (from `config.toml`).
5. **Stop for code review** (you review the diff).
6. Only after approval: proceed to next micro-iteration.

### 1.2 Mandatory “stop points”
Stop and request review *before* any of the following:

- Any change to VM networking / vsock / virtio-serial assumptions (must remain no-IP). [[11]]
- Any change to the gRPC boundary (host UDS API or VM vsock API). [[11]]
- Any schema change affecting:
  - `PepHttpRequest` / `PepHttpResponse`
  - `PolicyInput` / `PolicyDecision`
  - `AuditEntry`
  - `IdentityToken`
  - `ErrorEnvelope` [[11]]
- Any change to OPA evaluation inputs/outputs, policy hash, or deny explanations. [[11]]
- Any change to audit log format/redaction rules. [[11]]
- Any new dependency that increases attack surface (HTTP client, TLS, parser, VM mgmt, policy engine wrappers).

---

## 2) “Types-first, tests-first” (repo doctrine)

### 2.1 Types-first (required)
Before implementing behavior, define/update the relevant types at the boundary.
Your architecture explicitly calls out stable schemas and additive-only changes. [[11]]

**Rules**
- Prefer **strong types** over strings:
  - newtypes for `RequestId`, `DecisionId`, `PolicyHash`, `Token`
  - enums for `RedirectMode`, `ResourceType`, `HttpMethod`
  - validated URL type (reject unsupported schemes early)
- Put invariants in types when possible:
  - bounded sizes (`NonZeroU32`, capped `Bytes`)
  - validated header maps (size/length caps)
- Keep boundary models versioned; only additive changes unless version bump + migration note. [[11]]

### 2.2 Tests-first (required)
Add tests before or alongside implementation.

**Minimum test expectations by change type**
- Pure logic (SSRF guards, redirect rules, caps enforcement): unit tests
- gRPC boundary (request → OPA decision → allow/deny → response): integration tests
- Serialization compatibility for boundary types: contract/golden tests
- Policy: OPA tests must cover deny-by-default + explicit allowlist success. [[11]]

If something is difficult to test (e.g., Virtualization.framework VM lifecycle):
- introduce a trait seam and a fake,
- or create hermetic state-machine tests for transitions.

---

## 3) Rust best practices (hard guardrails)

### 3.1 Safety baseline
- Default: `#![forbid(unsafe_code)]`.
- No `unwrap()` / `expect()` in non-test code.
- Panics must not be reachable from daemon request paths.

### 3.2 Error handling and stability
Your architecture requires **stable error codes and clear policy explanations on deny**. [[11]]

**Rules**
- Use a single error envelope type across boundaries (`ErrorEnvelope`). [[11]]
- Error codes are stable and documented.
- Include `request_id`/correlation id in errors.
- Never leak secrets (tokens, headers, bodies) in errors.

### 3.3 Concurrency and cancellation
Your architecture includes caps, rate limits, and deterministic enforcement. [[11]]

**Rules**
- All network/IO operations:
  - must be cancellable,
  - must obey timeouts (`timeout_ms` where applicable). [[11]]
- Use bounded concurrency in the PEP to avoid DoS.
- Avoid detached tasks that outlive request context.

### 3.4 Dependency hygiene
- Prefer well-known crates and minimal features.
- Every new dependency must be justified in the PR summary:
  - why needed
  - alternatives considered
  - security implications

---

## 4) Security guardrails (PEP/OPA/audit/SSRF)

### 4.1 “No side effects without PEP” (core rule)
All side effects must go through the host PEP and be policy-checked. [[11]]

Side effects include:
- outbound network
- filesystem writes outside explicitly allowed workspace paths
- process execution
- secrets/tokens access

### 4.2 SSRF + redirect handling
The PEP is a concentrated attack surface; SSRF is explicitly called out. [[11]]

**Must-haves**
- Parse URLs via a proper parser (no regex parsing).
- Default allow only `http`/`https` unless policy explicitly extends.
- Block private/loopback/link-local by default (unless policy allows).
- Redirects:
  - cap max redirects,
  - re-check policy on each hop,
  - disallow scheme changes unless explicit,
  - ensure final resolved target is still allowed.

### 4.3 OPA discipline
Policy is deny-by-default with explicit allowlists. [[11]]

**Rules**
- Construct a structured `PolicyInput` and treat it as API. [[11]]
- Persist/return `PolicyDecision` with:
  - allow/deny
  - reasons (user-comprehensible)
  - obligations (redaction/caps)
  - `decision_id` and `policy_version` [[11]]
- Obligations must be enforced by code (not advisory).

### 4.4 Audit logging (append-only JSONL, redacted)
Append-only audit logs with redaction are required. [[11]]

**Rules**
- Every request results in exactly one audit entry:
  - allow → record decision + digests + latency
  - deny → record deny reasons + error code
- Never log secrets; log digests where useful.
- Audit format changes:
  - additive-only, or
  - version bump + migration note (stop for review).

---

## 5) Component boundaries (expected repo modules)

Based on the architecture building blocks: [[11]]

- `pep-daemon` (Rust): gRPC APIs, OPA evaluation, SSRF guard, audit. [[11]]
- `vm-manager` (macOS): VM lifecycle via Virtualization.framework, mounts, vsock. [[11]]
- `vm-agent` (inside VM): fetch override + PEP client; no direct egress. [[11]]
- `openclaw` browser module: CDP interception + fulfill. [[11]]
- `policies` (Rego + tests): deny-by-default + templates. [[11]]
- `cli` (host): `start/stop/status/policy/logs`. [[11]]

If the actual repo deviates from these names, update both `AGENTS.md` and `config.toml`.

---

## 6) Pre-review checklist (must pass)

Before asking for review:
- Run all required commands from `config.toml` (`format`, `lint`, `test`, plus policy checks).
- Confirm no changes violate invariants (no-IP VM; PEP-only egress; deny-by-default). [[11]]
- Confirm secrets are not logged and audit redaction is intact. [[11]]

---

## 7) PR / review package format

When requesting review, include:

1. Intent (1–2 sentences)
2. Diff summary (bullets)
3. Tests added/updated + exact commands run
4. Security impact (SSRF, egress, token/audit implications)
5. Compatibility impact (schema/policy/log format)
6. Rollback plan
7. Open questions / TODOs

---

## 8) If anything is unclear: stop and ask

Do not guess:
- gRPC/proto shapes
- error code taxonomy
- policy input schema
- audit format

Ask for the relevant file(s) or guidance first. [[11]]
