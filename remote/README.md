# Remote transport

This directory is a narrow adapter around the existing owner daemon. The Mac and iPhone make outbound WebSocket connections to a content-blind relay; the relay routes opaque application ciphertext and has no account, mailbox, push, or persistence subsystem. The signed native client provides end-to-end encryption against an active relay operator. The Safari diagnostic encrypts frames only after trusted HTTPS code delivery and therefore trusts its TLS ingress/static-code provider against active substitution; it is not a product E2EE client.

The legacy capture/library bridge RPC allowlist is:

- `memex.capture.enqueue`
- `memex.capture.status`
- `memex.item.read`
- `memex.library.list`
- `memex.library.item.read`

The Mac bridge translates those calls to framed owner-only UDS commands. It does not open the queue database or Store. Remote status and item reads use remote-specific daemon operations: status requires producer `remote_iphone`; item read additionally requires the exact succeeded job/result-item association before Store is called. The owner daemon remains responsible for URL admission, DNS/fetch policy, capture coordination, durable idempotency, and Store reads.

## Scoped mobile gateway — synthetic local slice

This is a separate opt-in `createLibraryRouter` + `SessionGrants` assembly of the
existing `BridgeRunner` / `MacSecureSession` / relay. The installed legacy bridge
does **not** acquire these methods or permissions. Do not expose its old library
methods as a fallback when the new daemon or gateway is unavailable.

Pairing proves phone identity; it does not grant library ownership. A Mac-owned,
in-memory grant binds the exact pairing session and generation, `read` (required),
optional `edit`, a `public|internal|private|restricted` ceiling and absolute expiry.
No remote RPC can create or change a grant. Restart loses grants and pairing in
this fixture, requiring new local authorization; there is no persistent ACL/schema
migration or Keychain access. `set` replaces the grant identity even when values
are identical; `revoke` removes it. Every request checks the current grant before
dispatch and rechecks it, expiry and phone trust immediately before encrypting
and queuing the response. A changed or revoked grant suppresses the entire
in-flight result and closes the channel. Already-sent results cannot be recalled.

The request envelope is the existing `{id, method, params}`; responses are
`{id,result}` or `{id,error:{code,message}}`. Only these five methods are allowed:

- `memex.session.info`, params `{}`: returns `scopes`, `max_sensitivity`,
  `expires_at_ms`, `generation_ready:false`, `chat:"unavailable"`,
  `translation:"unavailable"`. These are server values, never request authority.
- `memex.library.query`, params `{query,offset,limit,expected_view}`: query and
  `QueryPage` follow [library_query](../interfaces/memex.library_query.md).
  No client ceiling is accepted. Counts and facets exclude above-ceiling items.
  Generation/view are ceiling-specific: normal hidden-only edits do not change
  low-ceiling tokens or invalidate its pages. Complete Store verification still
  fails closed; no timing/noninterference guarantee is made for corrupt storage.
  On `stale_query_view`, discard the old window and restart at offset zero.
- `memex.library.item.get`, params `{item_id}`: returns `item_id`, opaque
  `revision_id`, `title`, `manual_tags`, `source_url`, `sensitivity`,
  `coordinate_space:"revision_markdown_utf8_v1"`, `content_sha256`, `total_bytes`,
  `markdown` (first at most 16 KiB on a legal UTF8 boundary), and `next_byte`
  (null at the end). Empty documents return empty text and null. This reads
  original Markdown, not generated summary/translation. Type/date come from the
  query row; do not invent missing dates or generated results.
- `memex.library.context.read`, params exactly
  `{item_id,expected_revision_id,start_byte,end_byte}`: returns the existing
  [ContextSlice](../interfaces/memex.item_read.md#revision-bound-context-local-trial).
  The response's `input` object holds revision hash and Evidence IDs; it is not a
  request parameter. `content_markdown` is the text, and `input.revision_id` is the
  revision. The strict half-open byte range is at most 16 KiB. If the known-valid
  start is the prior end/next_byte and end splits a code point, clients may reduce
  end by at most three bytes on `invalid_context_range` only. Hashes cover different
  objects: never substitute content_sha256 for revision_sha256 or use translation
  offsets for original text.
- `memex.library.metadata.update`, params
  `{item_id,expected_revision_id,title,manual_tags}`: requires `edit`; returns
  `{schema_version,item_id,previous_revision_id,revision_id,revision_number}`.
  The Store checks effective sensitivity **inside the writer lock before CAS**.
  Missing and above-ceiling items both return `library_resource_not_found`, before
  revision mismatch is disclosed. A stale revision returns `stale_item_revision`
  without writing. Only title/manual tags change, not sensitivity or body.

The gateway supplies the ceiling on owner UDS `library_query`, `context_read`,
`scoped_item_read` and `scoped_metadata_update`. These local commands are **not**
independent network authorization; only the authenticated gateway may assemble
them for a phone. No naked owner item_read/update, generated-result or enqueue fallback
is reachable from this router. Existing Store verification, journal recovery,
durability and full-query-snapshot semantics stay intact.

Remote edits do not explicitly enqueue generation. The existing daemon reconciler
may record pending analysis jobs, but this fixture has no configured translation
provider and makes no model request. Provider readiness is not inferred from a
model selection or successful pairing.

**Write ambiguity:** authorization linearizes at dispatch. Revocation after
dispatch suppresses the reply but cannot undo a durable Store edit. A timeout or
disconnect on a write is outcome **unknown**, not failure or proof of rollback.
If owner UDS fails after dispatch while the WebSocket stays connected, the
gateway returns `error.code="unknown"`; only known pre-mutation Store rejection
codes (CAS, validation, missing/denied, unchanged, revision limit) remain definite.
Reconnect/read back before deciding, never auto-resend metadata edits.
Metadata has no idempotency-key facility. CAS prevents a stale retry overwriting
another revision but is not an exactly-once receipt. Session grant changes do
not retroactively erase data already displayed by a client.

### Scoped link capture and translated reading (synthetic slice)

No generic chat/QA or production provider readiness is implied. Capture needs
an explicit server-owned `capture` scope **and** read ceiling at least `internal`,
because existing capture source/envelope sensitivity remains Internal. Read/edit
alone never authorizes it; even `capture` with Public returns `permission_denied`
before enqueue and creates no job. This does not promote any existing grant.

- `memex.capture.enqueue {request_key,url}` accepts an HTTPS URL and a key of
  `remote_mobile_` plus 32 base64url characters. Returns `{job_id,state,item_id}`.
  States: queued, leased, artifact_ready, retry_wait, needs_user, succeeded,
  failed, cancelled. Item ID is null until the canonical Store commit succeeds.
- `memex.capture.status {job_id}` returns the same three fields, not URL/body,
  preview metadata or raw diagnostics. Both operations recheck effective read
  sensitivity of a stored result. Missing/foreign jobs return
  `remote_resource_not_found`; ungranted capture returns `permission_denied`.
- Ownership comes only from the authenticated channel's session ID, pairing
  generation, phone device ID and identity key. The gateway hashes that tuple
  for the durable Queue producer namespace. Clients cannot send it or authority.
  The existing queue transaction serializes lookup/insertion: same key+URL and
  same pairing returns the same job across reconnect/daemon restart; changed URL
  returns `request_identity_conflict`. Another pairing (including re-pairing)
  cannot read the job **or reuse that key**, even with a new valid capture grant.
  It receives no original receipt and creates no replacement job. The opaque
  random key collision rejection is observable; keys must be unpredictable.
  No migration of legacy `remote_iphone` jobs into scoped ownership is performed.
- Lost capture receipt is `unknown`. Retry only the **identical key and URL**
  under the original pairing; never mint a replacement key automatically. This
  is durable enqueue replay, not cancellation or rollback. Revocation stops new
  dispatch/replies; work already accepted may still finish after revocation.
- `memex.library.translation.status {item_id}` returns
  `{state,reason,revision_id}` for the **current** authorized revision. State is
  `not_requested|awaiting_enqueue|queued|running|succeeded|failed|unavailable`;
  reason is nullable. Absent jobs with a durable current-revision capture hook
  still pending (including deferred retries) are `awaiting_enqueue` with reason
  `capture_hook_pending`. This is not an Analysis job or model execution yet;
  clients keep querying rather than infer absence after a fixed timeout.
  Otherwise absent jobs are not_requested, not invented skipped. Reads never enqueue.
  Success requires the current-revision, hash/provenance-validated title and
  document Analysis pair, each readable at the effective ceiling. Corrupt or
  invalid projections return unavailable, leaving original text readable.
- `memex.library.translation.read {item_id,expected_revision_id,start_byte}`
  returns those status fields plus `title,content_markdown,start_byte,end_byte,
  next_byte,total_bytes,content_sha256,coordinate_space,title_analysis_id,
  document_analysis_id`. Each page is at most 16KiB, its end rounded down to a
  UTF8 boundary; continue at next_byte until null. A non-boundary/out-of-range
  start is `invalid_context_range`; revision mismatch is `stale_item_revision`;
  absent/unverified translation is `translation_unavailable`. Ceiling is checked
  before revision errors. The coordinate space is `analysis_translation_utf8_v1`,
  **not** source revision bytes. The content hash hashes translated Markdown;
  Analysis IDs identify provenance, not an original-text offset mapping.

Grant freshness/expiry/trust is checked before dispatch and before encrypted
reply for all four added methods, including status. Local owner UDS is still a
trusted assembly boundary, not a separate remote authentication protocol.
Store commit precedes durable post-Store Analysis enqueue via the existing hook.
There is no new production generation worker or automatic credential resolution.

Use `startMobileFixture({capture:true,mobileRoot})` for the explicit synthetic
Internal read/capture/edit session. Default fixture remains Public read/edit.
It injects only deterministic `capture.example.invalid` HTML and synthetic model
outputs into the real fetch/admission/Store/Analysis pipeline. Other hosts never
reach DNS/network. `/article` succeeds, `/fail-translation` preserves an original
with failed translation, `/chinese` has no translation job. Existing daemon
reconciliation may enqueue after edits; that is not a guarantee of generation.
The helper can restart only its own daemon/root and re-pair for persistence and
ownership tests. The example's resume mode requires its private-temp synthetic
receipt and exact root-bound config; no real library/config input is supported.
Browser/static routes do not expose this receipt. No provider, Keychain, installed
service, public relay or real library is used by this supply.

### Reproducible isolated supply

```sh
TMPDIR=/private/tmp cargo build -p memex-agent --example mobile_fixture
node --test remote/test/library-gateway.test.js
node remote/test/mobile-fixture.js /absolute/path/to/frontend/mobile/web
```

The Rust example accepts no real library/config/socket input: it creates a new
temporary canonical Store with four synthetic records (two public, one internal,
one private), starts its own daemon with translation/intake disabled, and reports
its owned root/PID/socket. The Node helper adds a fresh memory-only pairing and
one-hour read/edit public grant. Its relay binds only `127.0.0.1` on an ephemeral
port; SIGINT/SIGTERM closes the owned bridge/relay/daemon and removes only that
fixture root. Relay shutdown stops accepting and closes its HTTP/preconnect and
WebSocket connections without requiring the browser to close first.
Abnormal process death may leave the disposable root; no installed
service or real database is used or cleaned. Build the example before tests;
there is no silent stub/old-daemon fallback.

With the optional frontend directory, an explicit static manifest serves
`/mobile/index.html`, its assets, and existing `/remote/v1/` modules from one
loopback origin. The exact paired page is **/mobile/index.html**, not /mobile/;
the existing protocol requires a canonical non-trailing-slash path. Config at
`/remote/v1/config.mjs` contains this page URL, actual relay URL and ALLOW_LOCAL.
Files are captured at startup; restart **only this fixture** after frontend edits.
The local receipt contains synthetic bootstrap material and must not be published
or passed via URL/query. No pairing/receipt/Store directory is served by HTTP.

This is a local integration harness, not a deployed phone entry. Keep the current
public Quick Tunnel static/synthetic-only. The browser still trusts delivered JS;
authenticated encrypted frames do not make it E2EE against an active code provider.
Real data, persistent grant UX, transport ingress and real-write acceptance remain
separate approval gates; no Tailscale/Serve/Funnel/DNS/Access configuration occurs here.

## Install-free Safari review canary

The relay serves the client at `/` and WebSockets at `/relay/<session>`, so one temporary accountless Cloudflare Quick Tunnel can carry both clients outbound without opening a public Mac port. This is one uninterrupted controlled-public diagnostic canary over one random hostname, with no SLA, account, Worker, D1, DNS, key, Access policy, mailbox, or persistent Cloudflare resource. Do not use it as a production transport or durability claim. A new Quick Tunnel hostname is a new browser origin, so browser identity, trust, and outbox state do not follow it.

The expected review sequence is:

1. Choose a loopback port and start `cloudflared tunnel --url http://127.0.0.1:<port>` separately. This repository does not install or run it. Record the generated `https://<random>.trycloudflare.com/` root as `PUBLIC`.
2. Start the relay on loopback with the explicit diagnostic switch and that exact root. The switch is the only mode that co-hosts temporary static assets with the relay; it derives one exact accepted browser `Origin`. Do not add a path, query, or fragment:

   ```sh
   HOST=127.0.0.1 PORT=8787 \
     MEMEX_REMOTE_DIAGNOSTIC_SAME_ORIGIN=1 \
     MEMEX_DIAGNOSTIC_PUBLIC_URL="$PUBLIC" \
     MEMEX_RELAY_ADMISSION_ROOT="$ROOT" npm run remote:relay
   ```

3. Open `$PUBLIC` in the iPhone Safari tab that will own the browser identity. Ensure no Mac bridge is
   already running, then start fresh pairing through the same public WSS origin:

   ```sh
   MEMEX_RELAY_URL="wss://${PUBLIC#https://}relay" \
     MEMEX_REMOTE_DIAGNOSTIC_SAME_ORIGIN=1 \
     MEMEX_RELAY_ADMISSION_ROOT="$ROOT" npm run remote:pair
   ```

   If this prints `bridge_already_running`, stop the current `remote:mac`/`remote:pair` process with Ctrl-C first, then rerun `remote:pair`. The process-lifetime singleton is acquired before pairing rotation, is owner-only and symlink/hardlink safe, and is released by the kernel after a crash. The command opens one 560×560 owner-private PNG in a dedicated system Quick Look window; it does not print a terminal QR or a pairing value, and closes/removes the image after trust commits, the five-minute admission expires, or the pairing process exits.

4. In that existing Safari page, press **在本页扫描 Mac 配对码** and point its in-page camera at the Quick Look window. The QR contains a non-navigable one-time code rather than an HTTP URL. A bundled, pinned pure-JavaScript decoder processes camera frames locally in the authenticated page; the code is never navigated, fetched, sent to the auth provider, or placed in an HTTP request. A normal reload keeps automatic paired-state recovery.

Keep `MEMEX_RELAY_TRUST_PROXY` unset for this Quick Tunnel. Both clients may appear from the same direct local tunnel source and fit within the default source bounds; client-supplied forwarding headers remain untrusted.

The Safari client requires iOS/iPadOS 18.4 or later. Apple shipped X25519 WebCrypto in [Safari 18.4](https://developer.apple.com/documentation/safari-release-notes/safari-18_4-release-notes), and WebKit documents the same release in [WebKit Features in Safari 18.4](https://webkit.org/blog/16574/webkit-features-in-safari-18-4/). Before accepting a pairing, the client executes Ed25519, ephemeral X25519, HKDF-SHA256, AES-256-GCM-with-AAD, and a close/reopen IndexedDB round trip that reuses the stable non-extractable Ed25519 private `CryptoKey`. X25519 keys remain connection-ephemeral and are never persisted. Any missing operation or required persistence property fails closed; there is no JavaScript/WASM crypto, plaintext, server-key, or `localStorage` fallback.

The browser pairing code contains the existing independent 32-byte bootstrap secret and 32-byte browser relay admission capability; it is not truncated into a human code. It is accepted only by the already-loaded page after exact prefix, size, base64url, transport-pin, Mac-identity, and five-minute expiry validation. Bootstrap material remains memory-only; IndexedDB receives only the trusted record after the Mac has durably consumed the one-time secret, and legacy untrusted records are deleted. Incoming URL query or fragment navigation fails before the app module, Web Lock, IndexedDB, camera, or socket starts.

The page loads no third-party network asset, analytics, or service worker. Its Apache-licensed QR decoder is pinned, bundled, and served through the same authenticated no-store route. All HTML/module/CSS responses set `Cache-Control: no-store`, strict CSP, `Referrer-Policy: no-referrer`, and `x-amp-review-widget: off`. Pairing independently pins the exact web-client URL and exact WSS `/relay` root; page query parameters are rejected. The browser admission capability authenticates `browser` client mode and the relay then requires one exact explicitly configured HTTPS Origin. These controls do not prevent the HTTPS/static-code provider from actively substituting the client before frame encryption begins. Native iOS uses a separately derived authenticated `native` mode and remains originless; the Mac uses `bridge` mode.

Within that one unchanged browser origin, one exclusive Web Lock serializes the active tab and one pending capture is transactionally retained in IndexedDB, bound to the exact Mac device ID and pinned identity key. Outbox creation and reconfirmation use compare-and-set transactions before becoming sendable in memory. Same-Mac session/generation replacement on that origin resumes with its stable request key. A different Mac or a legacy unbound record cannot automatically enqueue, poll, or read; the user must discard it or explicitly create a fresh request key for the current Mac. “Stop checking locally” never claims to cancel the Mac job. `needs_user` is shown as “Requires Mac approval” and is not remotely approvable in this slice.

Stable production remote use requires the signed native client (preferred), or a separately trusted stable app origin with an explicit static-code delivery trust decision. The random-hostname Safari diagnostic is not a substitute.

## Stable-origin deployment

A hosted browser deployment fixes the authenticated page at one exact URL (for example
`https://web.example.com/app`) and the transport at `wss://relay.example.com/relay`. The hosting
Function must serve the HTML and every module/CSS asset only after the verified provider subject
resolves server-side to the single owner; the URL slug is never authorization. The live-library route: it reuses the same persisted paired-browser identity and E2EE
relay/Mac/owner-UDS channel to read the canonical Mac Store without a second database or public Mac
origin. The embedded snapshot is dormant compatibility input and is not the root route's current view.
With an identity provider, an unauthenticated exact app-page GET redirects to the ordinary login page. Pairing
codes are not URLs and are accepted only inside an already-authenticated app page, so Camera/Code
Scanner never creates a second cookie/PKCE context. An existing owner session that reaches login or a
stale callback is redirected back to the app without starting or consuming another PKCE transaction.
Assets and wrong/encoded/extra slugs remain unauthorized or not found according to auth-first ordering.
Cloudflare Access mode preserves its existing provider contract rather than inventing a login flow.

Authenticate and open the exact app page first. Only after it loads should the current bridge be
stopped and a fresh five-minute in-page pairing code generated:

```sh
MEMEX_WEB_CLIENT_URL=https://web.example.com/app \
  MEMEX_RELAY_URL=wss://relay.example.com/relay \
  MEMEX_RELAY_ADMISSION_ROOT="$ROOT" npm run remote:pair
```

The product relay is a separate loopback service configured with the exact browser origin and serves
no static client:

```sh
HOST=127.0.0.1 PORT=8787 \
  MEMEX_BROWSER_ORIGIN=https://web.example.com \
  MEMEX_RELAY_ADMISSION_ROOT="$ROOT" npm run remote:relay
```

Moving from a temporary hostname creates a different IndexedDB/Web Lock origin. Do not copy keys,
trust, or outbox records between origins. First explicitly stop/discard any old-origin pending operation,
then authenticate at the stable page and create a fresh pairing. Fresh pairing is also the revocation
operation: it rotates the routing generation/session and clears previous phone trust under the bridge
singleton. Provider enrollment, DNS, named tunnel, managed-service installation, and live acceptance
are deployment-specific. The phone-access setup (mobile gateway, named tunnel, pairing) and the
separate relay/bridge/tunnel services are documented in [`service/README.md`](service/README.md).

## Loopback protocol review

Install the locked Node dependencies once:

```sh
npm ci --ignore-scripts
```

Create one 32-byte base64url deployment admission root and provide the same value to the relay and Mac bridge. Keep it out of shell history and logs; an injected secret environment is expected for review/hosting. Then start the development relay on loopback:

```sh
HOST=127.0.0.1 PORT=8787 \
  MEMEX_REMOTE_DIAGNOSTIC_SAME_ORIGIN=1 \
  MEMEX_DIAGNOSTIC_PUBLIC_URL=http://127.0.0.1:8787/ \
  MEMEX_RELAY_ADMISSION_ROOT="$ROOT" npm run remote:relay
```

With the memex owner daemon running, start a fresh pairing window in another terminal:

```sh
MEMEX_RELAY_URL=ws://127.0.0.1:8787/relay \
  MEMEX_REMOTE_DIAGNOSTIC_SAME_ORIGIN=1 \
  MEMEX_RELAY_ADMISSION_ROOT="$ROOT" npm run remote:pair
```

The command rotates the routing session, clears prior phone trust, opens a small owner-private five-minute pairing image in a dedicated system Quick Look window, and remains connected as the Mac bridge. The existing browser page scans that image in place; the system Camera must not navigate it. For a later trusted reconnect, run:

```sh
MEMEX_RELAY_URL=ws://127.0.0.1:8787/relay \
  MEMEX_RELAY_ADMISSION_ROOT="$ROOT" npm run remote:mac
```

Set `MEMEX_AGENT_SOCKET` only when reviewing against a temporary owner UDS instead of the default `~/Library/Application Support/LotsMemeX/agent.sock`. Mac identity, routing session/generation, active bootstrap secret, and pinned phone trust live in Keychain. The helper source at `remote/mac/keychain-helper.swift` uses native Security APIs and receives state over stdin, so private identity material is not placed in process arguments. The cross-process generation lock is a persistent non-secret owner-only file; it rejects symlinks/hard links, never steals or unlinks a live lock, and the kernel releases it automatically when its owning process exits.

Upgrading from the original unauthenticated-bootstrap MVP preserves stable Mac and phone identities but revokes its pairing trust and routing session. Scan one fresh QR to establish trust with the authenticated bootstrap protocol.

The signed native target is the preferred product E2EE client and protects command/result plaintext from an active relay operator. It is not needed for the reduced-threat Safari diagnostic canary. Its pending Keychain record has the same exact-Mac binding and confirmation-required migration behavior. If reviewing that target separately, stop the current bridge and run the explicit `npm run remote:pair:native` mode; its QR carries a native-only admission capability and is not accepted as a browser capability.

## Verification

```sh
node --test remote/test/*.test.js
cd ios && swift run MemeXProtocolFixtureCheck ../remote/fixtures/protocol-v1.json
cargo test -p memex-agent remote_ --locked -- --test-threads=1
cargo test -p memex-agent --locked -- --test-threads=1
cargo check --workspace --locked
```

The synthetic offline E2E starts an ephemeral relay and owner UDS and uses no live article, private body, provider, or network capture.

## Security boundary

- Relay logs contain readiness/port information only, never session IDs, payloads, URLs, or result bodies.
- Mac and phone status/error paths use bounded codes and do not log URLs or result bodies.
- Every fresh in-page pairing code has an independent 32-byte bootstrap secret. The phone proves possession with a domain-separated transcript HMAC before trust is saved; that proof is also bound into HKDF. The Mac erases the secret atomically with trust commit. If the encrypted ready frame is lost, only that exact saved phone identity can complete signed recovery without retaining the one-time secret.
- Relay admission uses separate deployment-root-derived, per-session/per-role/client-mode capabilities in the WebSocket subprotocol offer. Browser mode additionally requires the exact configured HTTPS Origin; native and bridge modes reject Origin. The relay echoes only `memex.remote.v1`; capabilities never enter URL/query parameters or application ciphertext derivation.
- Stable browser pairing requires separate exact `MEMEX_WEB_CLIENT_URL` and `MEMEX_RELAY_URL` values. Same-origin static hosting exists only under the explicit diagnostic switch; production relay mode serves no browser assets.
- In-page code bootstrap pins the stable Mac Ed25519 identity. Each connection uses ephemeral X25519, HKDF-SHA256 directional keys, AES-256-GCM, metadata AAD, and monotonic replay-rejecting counters.
- The signed native client keeps that frame encryption boundary against an active relay operator. The Safari diagnostic has the narrower boundary described above: it trusts HTTPS/static-code delivery, then encrypts frames; it must never be described as E2EE against an active code-serving relay/TLS operator.
- A fresh pairing rotates the session generation under an interprocess lock and invalidates in-flight and established old-phone sessions before another RPC can reach the owner daemon.
- Owner-UDS responses are fenced under that same cross-process authority: exact generation/session/phone trust is revalidated and ciphertext is synchronously queued before the lock is released, so rotation either suppresses an old response or linearizes after it.
- Native phone outbox state is written to Keychain before enqueue transmission. The Safari diagnostic publishes sendable outbox state only after its origin-scoped IndexedDB CAS commits and permits only one active tab through Web Locks. Retries reuse the same `request_key`; this is durable idempotency within the stated storage/origin boundary, not an exactly-once delivery claim.

## Required public TLS ingress controls

Production deployment is outside this branch. A reachable relay must be behind `wss://` TLS ingress with TLS 1.2+ and must preserve Upgrade/subprotocol headers while enforcing its own bounded handshake rate, concurrent connections per source, maximum header size, and finite idle timeout. Disable request logging or redact the complete request path/query and `Sec-WebSocket-Protocol` header: the path carries the routing session and the protocol offer carries admission capability. Never place capability or bootstrap material in URL/query fields.

The relay itself bounds total/per-source connections, rooms, frames/s, bytes/s, peer buffering, frame size, and idle room lifetime. It uses the direct socket address by default. Set `MEMEX_RELAY_TRUST_PROXY=1` only when the relay is reachable exclusively through a trusted proxy that removes client-supplied forwarding headers and writes its own `X-Forwarded-For`; otherwise forwarded headers are deliberately ignored.

The pinned Remodex attribution and exact upstream Apache-2.0 license copy are in [`NOTICE.md`](NOTICE.md) and [`LICENSE-remodex-apache-2.0.txt`](LICENSE-remodex-apache-2.0.txt).

The Bonjour/LAN surface from the canonical base is unrelated development smoke and is not exposed by this adapter.
