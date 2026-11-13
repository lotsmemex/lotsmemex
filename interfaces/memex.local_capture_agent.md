# memex.local_capture_agent

## Summary

`memex daemon run` is the product-owned user-session coordinator for one-URL public capture. It
owns an owner-only Unix socket, an owner-only SQLite operational queue, and an owner-only staged
artifact CAS. `memex-store` remains the sole canonical Item/Revision/Evidence/receipt writer; the
queue and staging CAS are never commit authority.

The supported foreground client flow is:

```text
memex capture '<ordinary sentence containing exactly one URL>' [--yes] [--request-id <opaque-id>]
  → redacted host/path-class confirmation
  → owner-authenticated Unix socket command
  → resolve every A/AAAA address and reject the whole hop if any is special
  → pinned-address GET with original Host/SNI/certificate identity
  → typed admission before staging or Store
  → frozen immutable artifact
  → translator
  → memex-store::ingest
  → memex-analysis translation + summary queues (post-success derivative hook)
```

The direct mobile flow uses the same queue and coordinator:

```text
iPhone /capture → same-origin POST /memex/capture → owner-only UDS Command::Enqueue
                → Coordinator → Store → live /memex
```

## Local command and status contract

The socket accepts length-prefixed JSON commands for `capture`, `jobs list`, `jobs status`, `jobs
retry`, `jobs cancel`, and `jobs approve`. `jobs list` returns at most 64 rows in stable
`(created_at, job_id)` order; a non-null opaque `next_cursor` is passed back with `jobs list
--cursor <opaque-id>` to request the next page. Invalid cursors fail closed. A frame is at most 64
KiB and must complete within the socket read/write timeout. Peer credentials must identify the
daemon's effective user.

`Command::Enqueue` carries an enum producer selected by product-owned callers. CLI jobs use
`local_cli`; direct mobile jobs use the distinct `local_surface` namespace. Browser input cannot
select or override that namespace.

Status output contains only generated opaque job IDs, enum state/stage, bounded reason codes,
timestamps/attempt count, and—only for `needs_preview`—the final host, `root|single_segment|nested`
path class, and a query-present boolean. It never contains a full URL, title, body, local path,
header, credential, or device key.

`capture_jobs_recent {max_sensitivity, limit}` (UDS capability `capture_jobs_recent_v1`) is the
read-only Mac progress view and the one deliberate exception that returns the submitted URL. It
requires `max_sensitivity >= internal`, clamps `limit` to 1..=100 and returns `capture_jobs:
{schema_version:1, jobs, counts}`. `jobs` lists running (`leased|artifact_ready`) first, then waiting
(`queued|retry_wait`) in processing order, then the rest by `updated_at` newest first. Each row has
exactly `job_id, url, state, stage, reason_code, created_at, updated_at, attempt_count,
result_item_id, next_attempt_at, producer`; `producer` is `chrome|mobile|other`, never the raw
namespace; `next_attempt_at` is Unix seconds or null; `reason_code` is only a bounded machine code.
A succeeded row whose item is above the ceiling or trashed has null `url` and `result_item_id`.
`counts` holds per-state totals over the whole queue. No options, payload, artifact path, title or
body is returned; nothing is written.

## Direct same-origin capture ingress

The provider-neutral public ingress contract is exact `POST /memex/capture` with
`Content-Type: application/json`, credentials scoped to the same origin, and one opaque random
`Idempotency-Key` of 1–128 ASCII alphanumeric/`._-` bytes. The key MUST NOT be derived from the
sentence or URL. The exact V1 JSON body is:

```json
{
  "schema_version": 1,
  "command": "capture_public_url",
  "text": "one bounded sentence containing exactly one synthetic HTTP(S) URL"
}
```

`text` is UTF-8, at most 16 KiB, contains no control characters, and uses the same exact-one URL
semantics as `memex_agent::extract_url`. Browser checks are advisory; the authenticated server and
Mac agent both revalidate. The request body is never copied into a URL/query, response, log,
analytics event, browser storage, job status, or canonical Store record. The Mac queue persists only
the extracted canonical URL and its digest; surrounding prose is discarded before local enqueue.

Exact key/exact request replay returns the same local job identity. The same key with a
different request is an idempotency conflict and MUST NOT enqueue. A successful response has exact
keys and no title/body/full URL:

```json
{
  "schema_version": 1,
  "job_id": "job_opaque",
  "state": "queued",
  "redacted_url": {
    "host": "article.example",
    "path_class": "nested",
    "query_present": true
  }
}
```

The Mac surface validates the body, extracts one canonical URL, and calls the owner-only UDS. It
does not write queue SQLite, use a cloud mailbox, or activate a relay/encryption transport.
`GET /memex/capture/status/<job_id>` returns only the opaque ID plus the truthful coarse state
`queued|fetching|processing|needs_user|failed|stored`.

Queue states are `queued`, `leased`, `artifact_ready`, `needs_user`, `retry_wait`, `succeeded`,
`failed`, and `cancelled`. Stages are `extract`, `resolve`, `fetch`, `validate`, `translate`, and
`commit`. A producer namespace plus stable producer job ID is unique. Exact request-digest replay
returns the original job; the same producer ID with a different digest records a conflict and is
quarantined from execution. The CLI retries one ambiguous socket failure with the same generated
request ID and prints that opaque ID; if both attempts fail, the operator can reuse it explicitly
with `--request-id` without exposing or persisting the surrounding prose. Job-stage and attempt
transitions are single SQLite transactions. Retryable public-fetch failures use a five-attempt,
capped exponential budget; permanent policy/media/encoding failures end `unsupported_content`.

## Admission and frozen-artifact rules

Store commit成功后，capture或relay consumer可把exact `IngestReceipt`交给
`enqueue_ingest_translations`和`enqueue_ingest_summaries`。同一个durable hook只有在两个幂等
enqueue都成功后才ack；任一hook失败或skip不得回滚已成功的source ingest，receipt replay必须
重放exact revision并得到相同job。consumer不得在Store前、staged CAS上或Store lock内调用
模型，也不得直接写operational SQLite或AnalysisRecord。完整合同见
`interfaces/memex.async_translation.md`。

Loopback intake与owner metadata edit在Store commit后若即时queue访问失败，仍返回唯一明确的
Store成功结果，避免client把已成功写入误判为可安全重试。daemon worker以Store current revision
为唯一权威执行有界分页reconciliation，幂等补齐translation与summary job；修复前投影保持
`not_requested`，不得伪造成功。

Admission results are `article`, `needs_preview`, `interactive_login_required`,
`challenge_detected`, and `unsupported_content`. HTTP 401/403/407, password/OTP/login forms,
App-QR login markers (including the Jike class), and CAPTCHA/challenge markers stop before staging
CAS or Store. Generic Readability success is not article admission: unknown generic HTML is
`needs_preview`.

Preview approval is permitted only for a frozen `needs_preview` artifact and binds its SHA-256 and
the admission policy version. Login, challenge, and unsupported jobs cannot be approved. A later
extension recapture is a new producer job, never approval of login-page bytes. Store commit retries
read the exact staged bytes and frozen capture identity/timestamp; they never refetch.

Public fetch accepts only credential-free absolute HTTP:80 or HTTPS:443 URLs, GET, at most five
fully revalidated redirects, no HTTPS downgrade, no implicit proxy/cookie/referrer/auth/client
certificate, valid rustls WebPKI TLS, bounded DNS/connect/header/overall time, at most 96 accepted
headers/64 KiB aggregate header bytes, at most 8 MiB encoded and decoded bytes, and at most 100:1
decompression expansion. Only successful UTF-8 `text/html` or `application/xhtml+xml` is eligible.
Every resolved address in each hop must be ordinary global unicast; mixed public/special answers,
IPv4-mapped IPv6, NAT64, 6to4, documentation, private, loopback, link-local, CGNAT, multicast,
unspecified, reserved, and other IANA special-purpose ranges fail the hop. A hop has at most 64
resolved addresses. DNS and decompression each have one bounded worker; if blocking system work
outlives its deadline the attempt fails with `fetch_worker_failed`. That worker is reused, never
replaced, so no thread is abandoned or accumulated; work behind it fails fast (`*_busy`) until the
blocked call returns. Any non-busy capture-worker error (including `fetch_worker_failed` or a
panic) restarts only the capture worker loop after a short delay and records
`capture_worker_restarted reason_code=<sanitized code>` on stderr; it no longer terminates the
daemon, so IPC, Intake and other in-flight work continue. The leased job stays durable and
recovers on lease expiry exactly as after a process restart. SQLite busy/locked keeps its quiet retry.

## macOS persistence and secret boundary

`memex daemon install` is macOS-only and installs a user LaunchAgent at
`~/Library/LaunchAgents/com.lotsmemex.agent.plist` (label prefix set at build time), with config at
`~/Library/Application Support/LotsMemeX/config.json`. It never installs a root LaunchDaemon. The
plist contains only the validated executable, `daemon run`, and config path; it contains no secret
or environment-variable payload. Non-macOS install/uninstall/status fail closed. Device keys and
relay secrets are represented only by opaque Keychain references through the `Keychain` seam; no
secret belongs in SQLite, argv, environment, plist, status, or logs.

The config may additionally contain a strict `translation` object with one canonical explicit-port
IPv4 loopback origin, pinned model ID, `sha256:<lowercase-hex>` model digest, and 1–60,000 ms request
timeout. That single reviewed credentialless model authority serves the independent summary and
translation workers; it contains no credential or arbitrary provider URL. When absent, capture
remains compatible and post-Store jobs stay durable but no model worker runs.

`memex direct install --lan-address <private-ipv4> --lan-hostname <bonjour.local> [--port 43191]`
installs that daemon plus a second owner LaunchAgent at
`~/Library/LaunchAgents/com.lotsmemex.surface.plist`. The surface plist contains only the validated
executable, standard owner Library path, `internal` ceiling, stable high port, private bind address,
and Bonjour hostname. It contains no token, credential, URL payload, or environment variable.
`memex direct status` reports only installed/running booleans; `memex direct uninstall` stops and
removes both plists/config while leaving queue, CAS, and canonical Store data intact.
This Bonjour/private-LAN surface is a development smoke transport only, not the remote-iPhone
product acceptance transport. Remote pairing, signaling, encryption, and relay code remain
unimplemented until a separately selected mature reusable protocol defines that boundary.
The inherited `tiny_http` parser does not provide transport-bound header allocation or slow-body
deadlines, so this smoke listener assumes a trusted owner LAN and must never be exposed as an
Internet or adversarial-LAN endpoint. Handler-level Host/Origin/framing/body checks are defense in
depth, not a claim that this development transport has a hardened parser boundary.

## Frozen future producer seams (not implemented runtime)

### Native messaging browser producer

- Trigger only from an explicit user click in the active top-level HTTP(S) tab.
- Emit one bounded, UTF-8, inert DOM snapshot; never browser profile data, cookies, local/session
  storage, form fields, password values, or arbitrary local files.
- The daemon revalidates producer identity, URL policy, artifact integrity, and admission. A browser
  login snapshot does not inherit approval from an earlier URL-fetch job.

### Superseded outbound relay proposal (not runtime)

- This proposal is frozen and not part of the direct owner-LAN runtime.
- Mailbox records are bounded opaque IDs plus versioned nonce/ciphertext/authentication tag and an
  opaque Keychain reference. Plaintext URL/content and device keys are unavailable to the relay.
- Authentication/decryption occurs locally before queue admission. The relay never calls Store and
  is not a persistence substitute. Crypto, pairing, rotation, replay windows, and revocation require
  a separately reviewed implementation; this foundation intentionally supplies no fake cloud
  persistence.

### Batch discovery

- Discovery is one page, same site, at most 200 candidates, with no recursion or automatic
  pagination.
- The strictly sorted immutable candidate-ID set has one SHA-256 digest over its canonical JSON
  string-array serialization. Approval supplies strictly sorted selected IDs bound to that digest;
  each selection maps one-to-one to a unique stable child producer job ID derived from the
  canonical discovery-page URL, candidate-set digest, and selected candidate ID.
- Future execution is globally limited to two concurrent jobs and one per origin, with durable rate
  limiting and resume. This foundation validates the contract shape only and does not discover or
  fetch batch candidates.

### Offline Dashboard export (L2c)

`memex_surface::export_dashboard_snapshot` is a read-only, source-first
projection for the current paired/direct-intake authority. It accepts an
explicit Store library root, private output path, and
`DashboardSourceAuthority` (repository, commit, and tree); it does not
hard-code a Dashboard origin. It uses Store's existing
`with_consistent_snapshot` recovery/lock boundary to collect current item
revisions, owner metadata, Store projections, Analysis inputs, summary state,
and translation bindings, then releases the Store lock before rendering and
atomic output. Analysis/Store fingerprints are checked again before the final
rename.

The export contains the exact Dashboard v2 source manifest shape, deterministic
HTML/CSS/JS/resources, local supported PNGs, and an expected
`dashboard.memex.server-bundle.v3` import receipt. `Private` and `Restricted`
resources and unsupported/remote resources fail closed. The explicit
`DashboardExportConfig.source_first` option suppresses only ready translated
body projections for that export; the manifest and hidden authority marker
retain the real `ready` state. `queued`, `running`,
`failed`, stale, or invalid translation bindings are `pending`; an eligible
English item without a job is also `pending`; only verified ineligible
classification is `not_required`; an exact validated current-revision pair is
`ready`. Pending/not-required reader modes remain source-first and do not emit
translated body bytes. Existing exports are immutable validation-only replays.

The export path has no HTTP, publish, push, deploy, protected-readback,
Keychain, relay, queue, scheduler, daemon, or CLI caller. Mailbox/export
primitives remain dormant and do not authorize cloud activation. Provenance is
historical `96b`/`6660591` for mailbox concepts and Dashboard wire authority
`f1eba4c`; no deployment or readback proof is implied.

## Non-authority and remaining acceptance

This contract does not authorize live browser access, private profiles/data, deployment, provider
control-plane changes, fake relay persistence, batch execution, or use of Amp as product runtime.
Real macOS LaunchAgent lifecycle, Keychain, codesigning/notarization, browser native-messaging
packaging, relay cryptography, and batch rate-limit behavior require separate real-device review.

## Public WeChat URL capture

CLI and scoped mobile URL jobs use the same existing enqueue/status identity
and permission contract. For exact public `mp.weixin.qq.com/s/<id>` URLs,
capture freezes admitted HTML and all required article images before Store
commit. A retry/restart retains these verified bytes and downloads only missing
resources. Text is deterministically extracted, never model-rewritten. With no
destination supplied, the new unfiled item is visible through Inbox. No new
mobile request field, pairing or gateway configuration is required.

Admission policy is `capture-admission-v4-weixin-v2` only for this URL family;
other sources retain v4. The whole page is checked for credential controls,
editable regions and verification walls; only the article region
(`#activity-name` and a visible `#js_content`) is classified, so the page
shell's hidden QR dialogs and share buttons do not reject the article (v1
classified the whole page). A verification shell is `challenge_detected` /
`needs_user`, not an approvable generic preview. Authentication and prohibited
controls continue to fail closed. Missing/ambiguous body or images are not
substituted with navigation content. Images use the item-cover contract's
strict CDN policy; up to 96 distinct body image URLs and 64 MiB total downloaded
image bytes are allowed. Unsupported cover rasters are not reported as success.

Existing items are not silently rewritten or relocated: a different logical
capture finding an exact existing source/canonical URL stops at
`weixin_source_conflict` (`needs_user`). Final checks/commit are serialized
with cancellation and other capture commits on the same queue. The original
Store receipt resumes post-commit cover work without duplicating the article.
Cover updates use a persisted request identity and fixed revision/cover CAS;
an already present cover is preserved. Failed cover work never reports a fully
successful capture. Additional fixed status codes are `weixin_content_invalid`
(`failed`) and `weixin_commit_retry` (bounded retry or exhausted failure).
Status responses expose only codes, never upstream HTML/errors/URLs. Existing
post-Store derivative-intent behavior is unchanged; capture itself calls no model.

The final source-index check never waits for its mutex or rebuilds the index
inside the queue write transaction. A busy/stale index rolls that transaction
back, then refreshes and renews the lease outside it before bounded re-entry
(at most three checks per lease). Continued contention uses the existing
`polite_wait` / `retry_wait` scheduling outcome (at least one second), refunding
this lease's business-attempt count atomically rather than exhausting it.
The original job, request identity, frozen bytes, and final source/permission
checks remain in force. Leased jobs remain non-cancellable; a released
`retry_wait` job can be cancelled through the existing API.
