# PLAN: MVP VM Sandbox Plan

## Goals & Scope
- Build a local-first, Mac Silicon MVP with a Linux VM running untrusted Moltbot/OpenClaw, a trusted host PEP daemon in Rust with OPA, and a CLI for lifecycle, policy, and audit.
- Enforce deny-by-default for side effects and route allowed effects via the PEP.
- Avoid kernel extensions; use user-space virtualization and networking only.
- Enforce the invariant: from inside the VM, any connection to the public internet fails; only the PEP endpoint is reachable via vsock/virtio-serial (no IP networking).

## Key Decisions (Proposed)
- VM tech: Apple Virtualization.framework (direct integration) for reliable Apple Silicon support and control.
- Network model (chosen): no IP networking in the VM; only vsock/virtio-serial to the host PEP. This makes direct egress non-existent and non-bypassable.
- macOS host vsock limitation: host processes cannot bind `AF_VSOCK` directly; use AVF’s vsock listener in the VM host process and bridge vsock traffic to a local TCP stub on `127.0.0.1` (still no IP networking in the VM).
- AVF entitlement: vm-manager / AVF runner must be codesigned with `com.apple.security.virtualization` (interpreter mode will fail).
- PEP API transports:
  - Host-local (CLI/UI ↔ PEP): gRPC over Unix Domain Socket for efficient IPC and filesystem ACLs.
  - VM ↔ PEP: gRPC over vsock only (no host-only TCP fallback in MVP to preserve no-IP networking).
- LLM/model calls: all model/API HTTP goes through pep.http.request (generic HTTP action) under OPA; no network allowlists in the VM.
- Browser strategy (MVP): CDP request interception + PEP fetch + fulfill inside the VM (Option B / Design A). Explicit exclusions: websockets, SSE, uploads. Downloads allowed only to /workspace/downloads with OPA-enforced size and MIME/extension allowlists plus audit logging. Fallback: loopback proxy relay if interception coverage proves insufficient.
- Browser stack (MVP): use OpenClaw’s existing browser module; interception/fulfill mechanics must align with its CDP hooks and may require modifying OpenClaw to expose/insert interception hooks.
- OpenClaw hook points (initial): add request interception in openclaw/src/browser/pw-session.ts within ensurePageState() (after existing page.on(...) listeners) or as a helper called from createPageViaPlaywright(); this file already tracks requests and is the safest central place to install page.route(...) handlers for all pages.
- Context lifecycle requirement (PEP mode): never reuse browser.contexts()[0]; always newContext({ acceptDownloads: true, ... }), install context.route("**/*", ...) before any navigation, then create pages.
- OPA integration: bundle opa binary managed by the PEP for MVP simplicity; hot-reload policies by file watch; upgrade later to embedding if desired.
- Auth bootstrap (actionable): on VM boot, host mints a short-lived token + nonce and exposes it via a read-only virtiofs mount (e.g., /run/pep/token) or a virtio-serial channel. VM agent reads once, uses it to authenticate to PEP, and token expires quickly; PEP validates timestamp + nonce to reduce replay. Token only identifies the VM instance; OPA still gates all actions.
- Audit log: append-only JSONL with policy hash and redacted input/output snapshots.
- Updates (MVP): host-side updater only; VM image updates are managed by the host app and applied only when VM is stopped.
- Network mediation mechanism (MVP): override global fetch at VM process startup via vm-agent shim to call pep.http.request; compatibility is best-effort and may require proxy fallback for non-fetch clients; websockets are out of scope in MVP.
- Browser mediation mechanism (MVP): browser driver intercepts all HTTP(S) requests; PEP executes and returns responses. Either use pep.http.request with initiator=browser and resource_type/frame_url or add browser.http_request. Proxy relay is fallback only.
- Browser network backend seam: introduce a BrowserNetworkBackend interface (fetch(req) -> resp) with a default direct backend and a PEP backend. Install interception at the context level before any navigation so the first document request is captured.
- Breakage budget (explicit): document restricted OpenClaw capabilities (direct network, websockets, SSE, uploads; downloads only to /workspace/downloads under policy constraints) in CLI status and README so users expect limitations; note some sites may not work under interception.

## Proposed Repo/Module Structure
- pep-daemon/ Rust daemon (gRPC APIs, OPA management, audit, connectors)
- vm-manager/ macOS Virtualization.framework wrapper (VM lifecycle, networking, mount)
- vm-agent/ small in-VM helper to read token, provide PEP client, and route HTTP for model calls
- policies/ default rego + templates + test data
- cli/ CLI entrypoint and commands (start/stop/status/policy/logs)
- vm-image/ image build scripts (Alpine/Ubuntu minimal, OpenClaw deps)
- api/ protobuf definitions + generated stubs

## Milestones & Acceptance Criteria
### Milestone A0: vsock Feasibility Spike (Pre-Work)
Validate Virtualization.framework vsock support on macOS Apple Silicon.
Acceptance:
- Minimal host vsock server and VM client exchange a request/response.
- If vsock is unavailable, pause and revisit transport model before proceeding.
- Use a host TCP stub bridged from AVF vsock on macOS (no direct host vsock bind).
- Start host stub before VM; VM client should include retry/backoff to avoid early BrokenPipe.

### Milestone A1: Thin PEP Slice (Pre-Work)
Implement minimal pep.http.request with SSRF guard, redirect policy, size caps, and audit logging.
Acceptance:
- PEP can fetch a public URL with allowlist policy.
- SSRF guard blocks private/localhost/link-local.
- Redirects to disallowed domains are blocked.
- Size caps enforced during streaming.

### Milestone A1b: Fetch Override Spike (Pre-Work)
Boot VM with no IP; run minimal Node process with global fetch override wired to PEP.
Acceptance:
- HTTPS request succeeds end-to-end via PEP (host handles DNS/TLS).
- Streaming response works end-to-end.
- Headers and body pass correctly; binary payloads round-trip under caps.
- Size caps enforced for request and response.
- Deny policy returns deterministic error codes.
- Boot verification uses cloud-init markers in shared workspace (console output is not reliable).

### Milestone A2: Browser Interception Spike (Pre-Work)
Launch Chrome in VM (headless ok) with CDP interception enabled.
For 2–3 representative sites:
- Intercept document request → send to PEP → fetch on host → fulfill response.
- Intercept critical subresource requests (CSS/JS/images) and fulfill.
- At least one xhr request is fulfilled via PEP and returns JSON consumed by the page.
- OPA allowlist blocks one domain with clear deny reason.
- Validate a simple automation assertion (page title or DOM selector).
Acceptance:
- No IP networking in VM throughout.
- Interception path works end-to-end; failures are policy-driven and explainable.

### Milestone A2.5: Buffered Fulfill Limits Spike (Pre-Work)
Load a site with ~5–20MB total assets via interception.
Acceptance:
- Page renders under caps or fails with explicit policy error.
- Memory usage and added latency recorded.

### Milestone A3: Modern Site + Performance Spike (Pre-Work)
Load 1 static site and 1 JS-heavy site via interception under no-IP networking.
Acceptance:
- ≥50 subresource requests served correctly.
- One DOM interaction works (click triggers JS).
- Added latency within a defined bound (set a target, even if loose).
- Deny a third-party domain and confirm page fails with an explainable policy reason.

### Milestone 0: MVP Capability Matrix
Define minimum viable functionality under the no-IP invariant and map each capability to mediation:

- LLM/model calls: PEP-mediated via pep.http.request (must).
- Workspace read/write: VM direct via shared mount (must).
- Web fetch (non-LLM): optional, PEP-mediated via pep.http.request.
- One chat connector (e.g., Slack): optional, PEP-mediated via pep.message.send.
- Direct VM outbound network: not supported (never).
- Browser automation: supported via browser-driver interception with explicit limitations (no websockets, no SSE, uploads denied by default; downloads allowed only to /workspace/downloads under policy constraints).

### Milestone A: Host PEP Skeleton
Implement pep-daemon with config, health endpoint, OPA policy evaluation, and JSONL audit logging.
Acceptance:
- PEP starts via CLI.
- pep.policy.evaluate works with sample input.
- Audit log appends entries.

### Milestone B: VM Launcher + Workspace Mount
Implement vm-manager using Virtualization.framework.
Add shared workspace mount (read/write) only via VirtioFS.
Workspace security rules: no other host mounts; host-side path canonicalization on any PEP file operation; reject symlink traversal that escapes workspace; omit import/export from MVP unless paths are rigorously validated.
Acceptance:
- Create file inside VM workspace; appears on host.
- Access to other host paths fails (not mounted).
- Cloud-init updates require new `instance-id` in the seed; build seed ISO via `mkisofs`/`hdiutil` on macOS (no `cloud-image-utils`).

### Milestone C: Network Isolation
Disable VM IP networking entirely; expose only vsock/virtio-serial to host PEP.
Acceptance:
- No IP interface inside VM (no default route).
- curl https://example.com fails inside VM.
- Raw TCP connect to 1.1.1.1:443 fails.
- DNS lookup fails or only resolves the PEP host if a resolver is present.
- No route to host LAN subnets (local network scanning fails).
- Any connection attempt except the PEP vsock endpoint fails.
- PEP health check via vsock succeeds.

### Milestone D: Run OpenClaw in VM
Provision VM image with Node runtime and OpenClaw runtime deps.
Add vm-agent shim/library to route LLM HTTP through PEP (since no IP networking).
Add browser interception module (CDP) that routes browser HTTP(S) requests to PEP and fulfills responses. Proxy relay remains fallback only.
Acceptance:
- OpenClaw starts in VM without external egress.
- Direct external network access fails.
- Malicious plugin attempting fetch("https://example.com") and spawn("curl ...") fails.
- Browser can navigate to allowlisted sites via interception; non-allowlisted sites fail.
- Browser download allowed only to /workspace/downloads under policy constraints.

### Milestone E: Route One Side Effect via PEP
Implement pep.http.request in PEP and client stub in VM; wire OpenClaw model calls to use it via vm-agent fetch override.
OPA decision gates the action.
Acceptance:
- Deny policy: action fails with explainable error; audit record written.
- Allow policy: action succeeds; audit record written.
- Streaming response supported for at least one model provider path.

### Milestone F: CLI + Policy Editing
CLI commands: start/stop/status, policy edit/validate/test, logs tail.
Acceptance:
- User can start/stop VM, edit policy, validate/test policy, and see audit entries.

## Component & Trust Boundary Diagram (ASCII)
```
+----------------------- macOS Host (Trusted) -----------------------+
|                                                                    |
|  +---------------------+      gRPC (UDS)      +-----------------+  |
|  | CLI / Minimal UI    |  <-----------------> |  PEP Daemon      |  |
|  +---------------------+                      |  + OPA           |  |
|                                               |  + Audit Log     |  |
|                                               |  + Connectors    |  |
|                                               +---------+--------+  |
|                                                         ^           |
|                                                         | gRPC       |
|                                                         | vsock only |
|  +-------------------- Virtualization.framework ----------------+   |
|  |                  Linux VM (Untrusted)                        |   |
|  |  OpenClaw + Plugins (no IP networking)                        |   |
|  |  Workspace mount only                                        |   |
|  +--------------------------------------------------------------+   |
+--------------------------------------------------------------------+
```

## Top 10 Risks/Gotchas + Mitigations
1) VM networking bypass (egress leak). Mitigation: no IP networking; only vsock/virtio-serial channels.
2) vsock support/limitations on macOS. Mitigation: verify Virtualization.framework vsock support early; if unavailable, re-evaluate model choice before proceeding.
3) OPA policy reload race. Mitigation: atomic policy file replace + version hash in audit.
4) Token replay from VM. Mitigation: short TTL, nonce, and timestamp; rotate on VM boot.
5) Workspace mount escaping (symlinks). Mitigation: resolve and validate paths on host; deny symlink traversal; canonicalize before any host-side file access.
6) PEP API surface creep. Mitigation: keep minimal actions; deny by default.
7) Audit log integrity. Mitigation: append-only JSONL with periodic hash chain (optional later).
8) Keychain access errors. Mitigation: explicit user consent and per-connector scopes.
9) VM image drift. Mitigation: pinned build pipeline and versioned images.
10) Performance overhead for gRPC hops. Mitigation: batch requests; set max payloads; keep payloads minimal for model calls.

## Proposed Policy Schema (Input/Output)
Input:

- action.type (string)
- action.args (object)
- action.resource (object) with normalized fields (e.g., url, host, path, method)
- subject.user_id (string)
- subject.workspace_id (string)
- context.time (rfc3339)
- context.stage (string)
- context.mode (string, e.g., interactive or background)
- context.cost_estimate (number)
- context.data_sensitivity (string, default unknown)
- connector.scope (string)

Output:

- allow (boolean)
- constraints:
  - allowed_domains (list)
  - max_bytes (number)
  - rate_limit_per_min (number)
  - redactions (list)
  - allowed_mime (list, for downloads)
  - allowed_extensions (list, for downloads)
- reason (string)
- policy_hash (string)
- actual_cost (number, optional; filled post-call)

## Example Policies (Rego)
1) Deny-by-default for all side effects.
2) Allowlist domains for pep.http.request with max_bytes cap (used for model providers).
3) Allow pep.message.send only for specific channels during stage == "review".

## PEP API v1 Contract (Required Details)
- Transport: gRPC (UDS for host-local, vsock for VM).
- VM auth handshake: client includes x-pep-vm-token, x-pep-nonce, and x-pep-ts metadata; PEP validates token TTL and nonce uniqueness.
- Timeouts: default 30s for pep.http.request with per-call override (max 120s).
- Retries: no automatic retries by PEP; caller may retry with idempotency key.
- Streaming: support streaming response for pep.http.request (server streaming) to handle large model outputs.
- Size caps: request body max 5MB; response max 10MB (configurable).
- Error codes: DENIED_BY_POLICY, CONSTRAINT_VIOLATION, CONNECTOR_NOT_CONFIGURED, AUTH_FAILED, RATE_LIMITED, UPSTREAM_ERROR.
- Audit redaction: redact secrets and connector tokens from inputs/outputs; store hashes where needed.
- Browser HTTP support: pep.http.request (or browser.http_request) must include initiator=browser, resource_type, and frame_url for policy; no CONNECT tunneling is required under driver interception.
- Constraint enforcement (PEP must enforce, not just log):
  - Parse URL and enforce allowed_domains and host:port allowlists.
  - Enforce max_bytes on request and response bodies.
  - Block redirects to non-allowed domains unless explicitly permitted.

## Test Plan
Unit tests:
- Rego decision tests for deny/allow cases in policies/.
- PEP policy evaluation outputs (constraints mapping).

Integration tests:
- VM cannot egress to public internet.
- VM has no IP networking; PEP reachable via vsock only.
- PEP action allowed/denied based on policy with audit entries.
- PEP rejects requests that violate allowlist, redirects, or size caps.
- SSRF: deny 127.0.0.1, RFC1918, link-local ranges.
- SSRF rebinding-safe: hostname resolves to private IP → blocked.
- Redirects: allowed→disallowed redirect blocked.
- Decompression bomb: compressed small, decompressed huge → blocked.
- Size caps enforced during streaming (not only after buffering).

## Compatibility Matrix (MVP via Fetch Override)
Supported: HTTP(S) requests with headers/body; redirects (same-domain only by default); streaming responses; binary payloads under caps.
Not supported: websockets; SSE; custom TCP clients; non-HTTP protocols.
Fallback plan if coverage is insufficient: add loopback proxy relay for non-fetch clients (same vsock-to-PEP path) and route undici/custom HTTP clients through it.

## Browser Interception (Design A)
- PEP action: browser.http_request (or pep.http.request with initiator=browser) includes resource_type and frame_url.
- Explicit MVP exclusions: websockets, SSE, file uploads. Downloads allowed only under policy constraints.
- Compatibility contract (MVP): document + subresource + basic XHR for allowlisted domains within caps; no service-worker offline modes; large media streams blocked.
- Performance notes: optional caching for static resources; enforce strict response size limits.

resource_type taxonomy (MVP):
- document (top-level navigation)
- subresource (scripts, styles, images, fonts, media)
- xhr (fetch/XHR)
- iframe (subframe navigations)
- other (fallback; deny-by-default)

Policy guidance:
- allow document only for allowlisted domains;
- allow subresource only if parent document was allowed;
- allow xhr only to same-origin unless explicitly allowlisted;
- deny other by default.

Enforcement spec:
- document allowlist only;
- subresource allowed only if initiator frame domain is allowlisted or same-site;
- xhr same-origin by default;
- iframe treated like document with stricter defaults;
- other denied.

Interception implementation (MVP):
- use OpenClaw’s existing browser module request interception to respond with PEP-fetched bodies;
- fulfill is buffered (not a true stream), so enforce caps and deny/abort large media.
- Service workers and offline caches are not supported in MVP.

Scheme handling in route handler:
- allow http/https via PEP;
- allow data, blob, about, file via route.continue();
- deny other schemes by default.

OpenClaw hook point (browser interception):
- attach interception in src/browser/pw-session.ts when pages are observed (ensurePageState / observeContext) so every Page created via chromium.connectOverCDP has interception enabled. This is the central Playwright/CDP connection point.

Downloads (MVP):
- allow only to /workspace/downloads with OPA constraints max_bytes and allowed_mime/allowed_extensions;
- record audit {url, filename, bytes, mime, policy_hash} for each download.

## BrowserNetworkBackend Seam (MVP)
Minimal interface:

fetch(req: BrowserReq): Promise<BrowserResp>

Default backend: direct (no interception).
PEP backend: calls pep.http.request with browser metadata.

BrowserReq fields (derived from Playwright Request):
- url, method, headers, post_data (if any)
- resource_type (document|subresource|xhr|iframe|other)
- frame_url (best-effort, from request.frame().url())
- is_navigation (request.isNavigationRequest())
- request_id (generated per request for audit correlation)

BrowserResp fields:
- status, headers, body (buffered, size-capped)
- reason/error_code for policy denials or upstream failures

## Download Enforcement (MVP)
Download path (MVP): for download responses, PEP streams to /workspace/downloads/<sanitized> under caps, and route handler returns a small synthetic response (or 204). The agent is informed of the saved file path.
Caps: default 25–50MB (configurable); enforce while streaming to disk.
Audit: record {url, filename, bytes, mime, policy_hash} for each download.

Caching policy (MVP optional):
- Cache only subresource responses under 1MB with Cache-Control permitting.
- Key by URL + headers subset (User-Agent, Accept, Accept-Language).
- TTL honors max-age up to 10 minutes; no persistent disk cache (memory only).
- Never cache document or xhr responses.
- Invalidate cache on policy changes or workspace switch.

## Secure Defaults (Gateway Exposure)
- PEP listens only on UDS + vsock (no TCP by default).
- Any UI/dashboard disabled by default or bound to localhost with auth.
- Provide a safe diagnostics command via CLI (no open ports).

## PEP Hardening (MVP)
- SSRF guard: deny private IP ranges, localhost, link-local, and non-HTTP schemes.
- SSRF guard is rebinding-safe: resolve DNS and validate all resolved IPs are public before connect and on redirects.
- Strict redirect policy enforced for all initiators (model/browser).
- Global concurrency limits + per-VM rate limits.
- Enforce size caps during streaming (not only after full buffering).
- Defend against decompression bombs (limit decompressed size).

## Policy Explanation UX (MVP)
- On deny: show reason, policy_hash, and a suggested remediation (e.g., add domain to allowlist or increase max_bytes).

## Packaging & Release (MVP Dev Install)
- Codesign/notarization: sign host binaries and notarize DMG; avoid requiring reduced security settings.
- AVF runner binaries must include the virtualization entitlement at codesign time.
- VM image location: store versioned VM image under ~/Library/Application Support/<app>/vm-images/ with a manifest.
- Updates: PEP app updater manages VM image versions; image updates only when VM is stopped; verify checksums before use.

## Planned Files (initial)
/Users/on/p/peppa/pep-daemon/
/Users/on/p/peppa/vm-manager/
/Users/on/p/peppa/policies/
/Users/on/p/peppa/cli/
/Users/on/p/peppa/vm-image/
/Users/on/p/peppa/api/
