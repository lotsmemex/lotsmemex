# Chrome capture converter V1

Status: backend-owned wire contract; implementation/fixture availability must be
reported separately. An old server returning 404 is **unavailable**, not success.
Writing this contract does not enable a daemon, credentials, network fetch or models.
The reported local snapshot deployment is recorded separately below.

Owners: C-Staff owns this contract; during the approved 48-hour handoff the frontend
writer owns product implementation, including backend intake/Queue/translator/Analysis
and the complete `browser/chrome/**` package in its own checkout.
User approved c1 and requested comments/human-like loading (frontend human #488,
2026-11-01; controller verified). The 2 MiB, 8–25 second and bounded-scroll
numbers are engineering choices, not quoted user requirements.

## Authentication and capability discovery

Keep the existing IPv4-loopback intake Host, exact configured Origin, secret,
nonce, body bounds, no-cookie/no-Authorization and no-redirect contract from
`memex.capture_transport.md`. Each attempt uses a fresh nonce. Retrying a write
keeps its durable identity and complete payload unchanged. Secrets never enter
page/content-script payloads, links, receipts, logs or DOM.

`POST /api/v1/capabilities`, JSON `{ "schema_version": 1 }`, returns:

```json
{
  "schema_version": 1,
  "features": ["url-jobs-v1", "comments-v1", "quality-check-v1"],
  "comments_max_bytes": 2097152
}
```

Features describe implemented request/result protocols, not provider readiness.
Do not send new fields to a server without its matching feature. Snapshot V1
without any new fields remains supported. No discovery request enqueues work.

## Snapshot extraction, admission and video preservation

### Long-form marker exemption and policy identity

Current `memex-capture::classify_html` exempts the **text markers** login/qr/jike
from login-wall rejection only when `article_has_text` is true AND
`visible_text_chars >= 3000`. The count is non-whitespace Unicode characters from
the inspector's nonsuppressed text over the submitted HTML, not bytes, DOM-rendered
layout visibility, or a count restricted to the article subtree. At least some
text must be in an article; long text with no article text receives no exemption.
The threshold is inclusive. Short pages retain the existing marker-based rejection.

Credential controls (including password inputs) reject regardless of length.
Challenge markers, prohibited interactive elements, active contenteditable states,
structural limits and empty-content rejection remain independent gates. This is a
bounded false-positive reduction for article text, not proof that a page is public
or permission to bypass authentication. Raw `<main>` remains generic HTML requiring
preview when otherwise eligible; main alone does not satisfy article_has_text.

**Version decision: bump to `capture-admission-v4`.** This changes accept/reject
semantics for identical bytes, and preview approval binds both artifact hash and
policy version. Reusing v3 would conflate different admission policies. The local
product writer now emits `capture-admission-v4`; installation still requires the
candidate verification/release boundary. Record the deployed binary/source
identity alongside v3 during this transition; never relabel existing decisions or
receipts as v4. Once the bump lands, v3 preview approval is not v4 approval: recompute
the decision, and obtain new approval if preview is still required. Existing stored
items/Evidence are not retroactively rejected, rewritten or reimported by the bump.

For the exact public Amp podcast episode URL shapes, an explicit unique `main`
containing one `h1` and one `.transcript` has a source-specific article view.
Admission first retains full-page credential, editable and challenge checks,
then inspects the entire unsanitized main region including its attributes and
notices outside the transcript. Only after admission does translation select
the heading and transcript. Raw HTML and its hash remain unchanged Evidence.
Credential forms/inputs anywhere, ambiguous/missing body, explicit hidden body or
ancestor, and ordinary body login/challenge/interactive gates still reject. This
does not make arbitrary `main` elements into articles or bypass HTTP auth status.
Cover identity is selected only inside a unique main (or legacy article) region;
site navigation videos do not supply it, while multiple video IDs inside the region
remain an error. Admission rejection is deterministic: the episode worker returns
`needs_user` at `validate` with a fixed admission-class reason, not five automatic
`podcast_retry` attempts. Existing accepted request identities are never rewritten.
An explicit original-ID manual retry's first attempt may re-probe a previously
missing cover when no artifact or cover edit was prepared. Frozen RSS/page bytes
remain reused; automatic later attempts do not repeatedly probe a negative result.
This is not a guarantee if the first attempt fails before reaching cover selection.
Successful historical jobs are not re-enqueued or silently repaired by this change.

Existing regression: `login_words_in_a_long_article_are_content_but_short_pages_stay_walls`
checks long-article login text, short login wall, long non-article text and password
controls. It does not by itself prove the exact 2999/3000 boundary, qr/jike variants,
or old-version approval rejection after a bump; retain these as acceptance cases.

### Explicit current-tab main fallback

`extractArticleSnapshot` first considers `article,[role='article']` candidates,
excluding identified comment regions. Only when that candidate set is empty does
it consider outermost `main,[role='main']` regions. An existing but unsuitable or
ambiguous article candidate does not trigger main fallback. Sanitization, size/node
bounds, minimum text and candidate ambiguity checks apply to either selection.
The chosen main region's sanitized children are serialized inside `<article>`.
Thus the backend receives a connector-produced article snapshot; its raw-main
preview rule is unchanged. This is not browser background navigation, an iframe
content read, or a claim that the original page contained an article element.

### YouTube links, not downloaded video

During sanitization a recognized HTTPS YouTube embed iframe becomes an inert
`<p><a href="https://www.youtube.com/watch?v=ID">YouTube: URL</a></p>` in place.
The extractor accepts youtube.com/youtube-nocookie.com (with optional www prefix)
and m.youtube.com, with exact `/embed/ID` path and ID matching `[A-Za-z0-9_-]{6,20}`.
It discards embed query/fragment in the watch URL; other iframe content is removed,
not executed or fetched. This preserves a reference, not video bytes or a transcript.

The selected snapshot exposes deduplicated `videos: string[]`, at most 16 entries.
This is an extension-internal field, not a new capture-manifest field. The manifest
uses the existing remote attachment shape for each retained URL:

```json
{"url":"https://www.youtube.com/watch?v=tEse6n4hEJQ","media_type":"text/html","title":"YouTube"}
```

Because Readability can discard a lone link paragraph, `prepare_capture` checks
remote attachments after translation/sort/dedup. For each URL starting with
`https://www.youtube.com/watch?v=` that is not already a literal substring of
content_markdown, it prepends `[YouTube: URL](URL)` followed by a blank line. Other
attachments do not get this treatment. The existing body is retained; already
present URLs are not prepended again. Ordering follows the sorted attachments,
not a promise of video DOM order. This prefix/subsequence check is not a video-ID
validation or global URL deduplicator. Raw HTML Evidence stays unchanged; the
inserted link is part of derived Markdown and remote attachments remain references.

Regression: `youtube_attachments_lead_the_body_once_and_other_attachments_do_not`.
Both named backend tests were inspected as source here, not independently rerun.

## Reported local snapshot-only intake deployment

The product writer reports daemon `local-db-signed-540c143203d2` serving the existing
local MemeXDB with intake bound to `127.0.0.1:47821`, exact Origin
`chrome-extension://dmccdibgheklkgclcddbbhllfdihkppl`, secret file
`<MemeXDB>/Runtime/intake-secret` (0600), and snapshots at
`<MemeXDB>/Intake/snapshots`. These are one local installation's values, not
portable defaults or permission to disclose/read the secret.

This daemon assembles plain snapshot intake only: no URL-job backend or quality
model is attached. Do not infer URL batch support or model checking from this
deployment. The intake can still advertise comments/quality-check **protocols**;
that is not quality-provider availability. No quality execution is enabled here.
Frontend reports backend tests 344 passed / 0 failed / 0 ignored and uncommitted
implementation. This spec update verifies source seams only, not that running
binary/config, file permissions, endpoint health, or the reported suite result.

## Explicit selected-reference browser snapshots

This explicit browser path is separate from the URL-job routes below. It reuses
the existing `web-snapshot-v1` binary capture, admission, exact-source reuse and
Store commit contracts, not a new backend queue/schema or permission to install
a daemon. Capability discovery must read the configured intake at runtime; an
old deployment record is not evidence that the feature is available now.

The picker separates body citations / Learn more from Related posts, retaining
bounded occurrences while deduplicating candidates. Only the explicit click
“打开并抓取已选链接” freezes up to 200 selected HTTPS URLs, their destination and
the intake identity that supplied the directory view. The browser requests
optional permissions for those exact origins inside that click gesture. Denied
permission queues nothing. Required host permissions remain unchanged; the new
manifest declaration is optional HTTPS access, not an all-sites runtime grant.
The first click may show Chrome's selected-sites “read and change your data”
dialog; previously granted origins may not prompt again. Reloading this source
does not install a backend or add new required site permissions.

### Navigation versus source reading

This is ordinary, user-authorized **visible Chrome navigation**, one new tab at
a time, not hidden/background-tab crawling. Chrome may already follow redirects,
load subresources and use its normal browser session before inspection. The
extension never reads/exports cookies, clicks through login/challenges, logs in,
scrolls or recursively discovers more targets. The pre-click notice must disclose
that it **does not provide DNS, connection-IP or private-network protection**.
The URL filter rejects IP literals (including parser-normalized unusual IPv4),
localhost/local/internal and other special hostnames, credentials, non-HTTPS,
fragments and nondefault ports. It is syntax filtering, not a public-DNS proof.

Only the original frozen source may be read/committed. Comparison uses URL
parsing (scheme/hostname case normalized) and removes one trailing pathname `/`;
path case, query order/values, fragments (including empty `?` / `#` separators)
and encoding remain significant. A
changed final URL is a visible stopped result with no automatic retry or capture
of the destination. This does not claim Chrome prevented the redirect request.
The extractor's optional `expectedSourceUrl` guard runs synchronously inside its
serialized function before any DOM access. Omitting it preserves old single-page
behavior. A mismatch returns an explicit `page_changed` failure, not empty HTML.
Content reads are pinned to the inspected document ID to avoid tab/frame reuse.

### Durable identity, bounded execution and cancellation

Reuse the extension's existing `batchJobs` single writer. Freeze the non-secret
endpoint/registration binding along with the target; verify it at submission,
execution and before POST. Unbound/changed-intake entries stop rather than adopt
the current account. No fallback to Inbox or re-resolution of a folder name.
Binary bytes go into extension-owned IndexedDB before dispatch intent is saved;
then POST uses the fixed `external_capture_id`. Unknown replies retain the exact
bytes and identity, never auto-resend or recapture. Explicit “再次核对” replays
those original bytes to the same intake with a fresh transport nonce. Missing
bytes, content/identity conflicts or lost capabilities stop, not a new identity.

Only visits not yet started are cancellable. Persist the in-progress marker and
disable cancellation before opening the tab. A worker interruption leaves the
entry awaiting explicit checking, not eligible for automatic opening. Chrome may
leave the created visible tab behind; the progress view instructs the user to
close it before checking an interrupted visit. Normal completion closes the
extension-created tab. No AI/model calls, automatic recursion, cookie export or
new target discovery are authorized by this click.

### Snapshot bytes versus daemon fetching

The intake `web` branch consumes submitted HTML and verified raster-image bytes
through `prepare_binary → prepare_capture → install_snapshot → commit_web_capture`;
it does **not** fetch source URL content or fill missing images. Translation
localizes only matching verified blobs; missing bytes leave remote image links.
This is network-free snapshot admission, not inheritance of a DNS firewall.
Independent daemon URL/resource fetches continue to validate every redirect hop,
resolve and reject non-global addresses (`address_not_public`), and pin checked
addresses through the existing fetch transport. They are not a snapshot fallback.
Real reference capture / library writes require separate execution authorization;
synthetic tests and document completion are not deployed/real-capture evidence.

## One URL, one durable job

The legacy URL-job batch UI fans out a bounded user-selected list into individual
server jobs; no second batch queue, implicit crawling, background-tab opening or
hidden follow-up URLs. The separate explicit visible-navigation snapshot path
above does not change this URL-job behavior or the single-page snapshot protocol.
All following routes are authenticated POST JSON with `schema_version: 1`:

| Route | Additional request fields |
| --- | --- |
| `/api/v1/url-jobs/enqueue` | `request_key`, `url`, `include_comments`, `quality_check` |
| `/api/v1/url-jobs/status` | `job_id` |
| `/api/v1/url-jobs/retry` | `job_id` |
| `/api/v1/url-jobs/cancel` | `job_id` |

`request_key` is `chrome_capture_` plus 32 unpredictable base64url characters.
URL must be canonical HTTPS without credentials or fragment, through the existing
URL/admission/SSRF policy. `include_comments` is boolean; `quality_check` is
`off` or `configured`. Neither value is inferred from the mobile capture scope.
The server binds jobs to its authenticated extension registration; callers may
not supply a producer, principal, sensitivity ceiling, model ID or filesystem path.

The existing Queue transaction binds the full URL **and options** to the key.
Exact replay returns the original job, including after restart. Same key with
different URL/options returns `request_identity_conflict`, without another job.
Foreign registrations cannot read/retry/cancel a job or obtain its receipt by
replaying its key. No legacy/mobile job ownership is silently adopted.

Receipt shape (nullable values remain explicit):

```json
{
  "schema_version": 1,
  "job_id": "job_...",
  "state": "queued",
  "stage": "extract",
  "reason": null,
  "retry_at_ms": null,
  "item_id": null,
  "revision_id": null,
  "can_retry": false,
  "can_cancel": true,
  "comments": { "state": "not_requested", "reason": null },
  "quality_check": { "state": "not_requested", "reason": null, "analysis_id": null }
}
```

Capture states remain `queued|leased|artifact_ready|retry_wait|needs_user|succeeded|
failed|cancelled`; stages remain the existing Queue stages. Success means durable
Store commit, not successful model checking or translation. Error responses use
`{schema_version:1,error:CODE}` with bounded normalized codes, no raw exceptions,
URLs, HTML or provider response text. `unknown`/disconnect means reconcile with
the same key and options; do not generate a new key. Retry/cancel are explicit,
and the server rechecks eligibility even if a previously returned flag was true.
No cancellation promise for a leased/committing job; no automatic resubmit after
`request_identity_conflict`, `resource_not_found` or `permission_denied`.
Comments status states are `not_requested|pending|captured|none_found|in_frame|
too_large|unavailable`; `pending` never implies comments exist. Snapshot success
keeps its existing receipt fields and additionally returns the same `comments`
and `quality_check` objects when either feature was explicitly requested.

## Comments remain separate from the article

New optional `comments` on the existing capture-v1 JSON/binary manifest:

```json
{
  "state": "captured",
  "html": {
    "media_type": "text/html",
    "byte_size": 123,
    "sha256": "64-lowercase-hex"
  }
}
```

For JSON, `html` also includes UTF8 `body`. For binary, append the comment bytes
**after all image bytes** and include them in exact outer Content-Length. The
existing 36 MiB total budget still applies. At most one comment artifact, at most
2 MiB; validate exact size/hash/UTF8 and inert DOM independently on Mac. For
`none_found|in_frame|too_large`, `html` must be null and no bytes follow images.
Omission means `not_requested`. Unknown fields/states, missing bytes, excess bytes
or contradictory state/body fail before any Store write.

The verified comment artifact uses a distinct Evidence locator; do not concatenate
it into article Markdown or treat it as another primary HTML document. Its raw
bytes retain their own hash/backend reference under the original sensitivity.
The comments observation itself is captured provenance, including negative states;
changing it under the same capture identity is a payload conflict, not replay.
Comments are not inputs to model checking or automatic article translation.

For URL jobs, `include_comments:true` only inspects the fetched HTML. No browser
automation, cookie reuse, iframe fetch or lazy-loading claim. `none_found` means
none found in that snapshot, not that the site has no comments. `in_frame` means
an inaccessible frame was observed; it never claims its content was captured.
One-click DOM extraction may use the explicitly approved bounded current-tab
scroll, restore its position and report actual observation; Mac does not infer
that this happened from a checkbox.

## Optional model checking is distinct from classification and translation

Snapshot manifests may also request `quality_check:"off"|"configured"` (default
off). The server resolves a dedicated `capture_quality` purpose, not a hidden
classification/translation fallback or a model supplied by webpage text.
No existing user configuration is rewritten to select a model automatically.

Pipeline: verified input → existing admission → deterministic site/generic
translator → optional bounded check → Store → derived Analysis provenance.
Model output cannot override challenge/login/preview admission, replace raw
Evidence, invent source facts or certify completeness. No model call when the
configured route is unavailable; capture original translator output and report
`unavailable`, never `passed`/`corrected`.

Quality states are `not_requested|pending|running|passed|corrected|needs_review|failed|
unavailable`. A failed/invalid model result leaves the original deterministic
draft intact. Only mechanically validated formatting changes may be applied;
unverifiable content/metadata suggestions remain `needs_review`, not silently
accepted corrections. The derived record binds source capture/bytes, translator
ID/version, pre/post draft hashes, model/adapter descriptor, prompt ID/version/hash
and the current stored revision. `analysis_id` is returned only after that record
is durable; a queue flag alone does not establish successful checking.

`pending / awaiting_capture` means configured checking has not started while
capture is queued, delayed or waiting for the user; it is not provider failure.
`running` means the check is in progress. A failed/cancelled capture cannot run
the check and returns `unavailable / capture_not_committed`; a missing route is
`unavailable / provider_unavailable`. The capture state still controls retry and
cancel; pending quality alone never resumes a challenge or cancelled capture.
The first mechanical correction policy allows only CRLF normalization and final
newline formatting. Interior whitespace, Markdown structure or metadata changes
remain suggestions for review. Frozen check outcomes survive exact replay;
replay does not silently rerun a newly configured provider.

Omitted snapshot checking is canonical `off` for request identity. Changing
configured/off under an already accepted identity is a conflict, including
JSON/binary replays. Historical unchecked Store receipts cannot acquire a new
configured check by replay. Exact off replay validates Store's complete input
fingerprint before adopting any new option record.

Checks freeze prompt/policy and model descriptors as executed, not current-build
constants. `catalog/capture-checks` contains the frozen outcome and a durable
post-commit intent bound to the final Store input fingerprint. Store success with
temporary Analysis failure returns `pending / analysis_persistence_pending`;
the daemon reconciles without another fetch/model call. If the stored revision
has since changed, terminate as `unavailable / capture_revision_changed` rather
than attach the result to a different revision. A competing unchecked winner
with another input terminates the losing reservation as
`unavailable / capture_identity_conflict`, with no Analysis; its exact off replay
remains possible. Frozen losing data is retained, not treated as the winner's check.

The first verifier injects public-shaped synthetic HTML and model outputs. Real
execution needs the existing provider/Keychain HOLD resolved for this precise
purpose/input route. The user's model-checking request already covers this
function; absence of a per-call budget is not a new per-call approval gate. New
private-page disclosure, new credentials/ACLs or another purpose remain separate.

## Polite fetching, not anti-bot circumvention

Reuse the existing fetcher, public-address checks, pinned resolutions, redirect
validation, bounded body/decompression and admission. Same origin serial, small
global concurrency (one worker is sufficient initially), persisted per-origin
spacing with 8–25 second jitter. Replays do not reset backoff or fetch again.
For 429/503 use validated Retry-After (seconds or date) plus existing exponential
retry policy; expose actual scheduled `retry_at_ms`, not a UI-invented countdown.
Honor long server delays rather than clamp them to an earlier request time.

Origin reservations apply to redirect hops too. Local lease contention is a
per-job `polite_wait`, not shared server backoff; otherwise crossing redirects
can keep delaying each other indefinitely. Scheduling waits do not consume the
network retry budget. A Chrome fetch attempt has a 180-second bounded deadline
and a 240-second lease, allowing paced redirects; the interactive five-second
UDS contract is unchanged. At most 25 seconds is spent waiting inline for one
hop; longer waits become a persisted retry time. No attempt count or interval
guarantees avoidance of a site's challenge.

Challenges/login requirements become `needs_user` with normalized reason such as
`challenge_detected` or `interactive_login_required`; never repeatedly hammer the
page. The UI can offer to open it in the user's browser for an explicit snapshot.
No fingerprint spoofing, CAPTCHA solving, rotating proxies, cookies/login reuse
or promised avoidance of blocking. This is respectful scheduling, not stealth.

## Acceptance and remaining delivery gates

Use disposable Store/Queue, injected resolver/HTTP/model, fixed synthetic secrets,
real loopback HTTP authentication and real CLI/Store. Cover JSON/binary snapshots,
comments separation/bounds/hash/replay, mixed batch outcomes, options conflicts,
restart/retry/cancel eligibility, challenge/backoff without real waiting, model
failure/fabrication rejection, and durable provenance. Exercise existing mobile
tests after shared Queue/intake changes. Independently review authorization,
persistent replay and source-vs-generated provenance before combined acceptance.

Browser extension Origin/PNA behavior, real site extraction, real provider output,
installed service/cloud changes and production deployment are separate evidence
and authorization gates. A synthetic receipt does not clear them.

## Explicit public-site trial

The subsequently authorized 23882 Chrome Linkresearcher health-page trial uses
`examples/chrome_trial.rs`: a newly created private root, explicit maximum-six URL
allowlist, exact extension Origin, new local credential and one worker. No installed
config or service is read/replaced. URL jobs require quality=off, snapshot routes
are disabled for both transports, and redirect DNS/HTTP is restricted to the
selected host/origin. Normal system DNS resolution may use the existing resolver's
DoH fallback; this is not a claim that every packet goes to the page origin.
Capabilities remain protocol discovery, not permission to enable real model calls.
Results and failures are retained separately from MemeXDB; no duplicate import.

### Linkresearcher public article representation

The source adapter maps exact credential-free HTTPS
`www.linkresearcher.com/theses/<lowercase-uuid>` URLs (no query or fragment) to
the same origin's `/api/theses/<uuid>`. It sends no cookies, signatures or login
state. DNS/SSRF pinning, body/decompression limits, origin pacing and 429/503
backoff still apply; API redirects are rejected rather than followed. JSON is
accepted only for this recognized representation, not arbitrary webpage jobs.

The pure `linkresearcher.article` v1 translator requires matching response `id`,
title and blob acquisition URL. It extracts the public page's `wholeHtmlContent`
field, falling back to `content` only when the former is absent/empty. It does not
use abstracts, related articles or comments as article text. Original response
JSON remains checksum-bound Evidence in the spool, with its API source URL;
the item/canonical URL remains the user's public article URL. HTML conversion
is a derived document, not a fabricated raw HTML response. Images remain remote
references here; this adapter does not claim downloaded image preservation.

Admission v3 checks the selected HTML body for controls, challenges and visible
article content before Store. Metadata/link/script/style attributes alone no
longer imply login, and do not supply missing visible article text. Literal
`contenteditable=false` is not editable; all active editable states and independent
credential controls still reject. `qr` must be a separate token, not characters
inside a random image identifier. Existing v1/v2 preview approval does not approve
a v3 decision. API failure or unsupported
shape does not silently switch to browser sessions or other sources. Full
template coverage and original first-trial rejection causes remain unproven.

The translator removes only known inert leaf editor decorations (`↵`/`X` spans),
marking actual removal as `1-editor-cleanup-v1`; it never removes their surrounding
article container. The isolated trial can retain public response bodies and only
content-type/content-encoding/status metadata for rejected-input diagnosis. It
does not record authentication or Set-Cookie headers; production rejection paths
remain unchanged.

## Amp podcast enrichment increment (not yet advertised)

Use the same durable URL job for capture or existing-item enrichment; a client
match result is display-only and never substitutes for enqueue/commit identity
checks. The new feature must remain unadvertised until durable no-generation,
cross-request deduplication, target authorization and recovery are verified.
No second batch queue. `generation_policy: "none"` will bind request identity;
omission must preserve old request serialization. It cannot coexist with
quality_check=configured and cannot be implemented by quality_check=off alone.

The authenticated intake `POST /api/v1/capabilities` response `features` is the
plugin gate, not remote session.info or item_cover_v1. Require all three strings:
`url-jobs-v1`, `url-jobs-no-generation-v1`, `url-jobs-enrich-v1` before enabling
podcast bulk submission. The no-generation feature guarantees durable
`generation_policy:"none"`; it alone does NOT guarantee episode deduplication,
enrichment or archive placement. The enrich feature remains absent until those
contracts pass end-to-end validation. Unknown/missing capabilities disable bulk
submission, never fall back to legacy enqueue. Each request still carries
`generation_policy:"none"` and `quality_check:"off"`.

### Active UI destination (new contract; not yet advertised)

Authenticated `POST /api/v1/active-group` accepts exactly `{"schema_version":1}`
with the same Origin/secret/nonce boundary as capabilities. Capability
`url-jobs-active-group-v1` is advertised only with the installed state writer and
reader. Response: `{schema_version:1, group_id, path, root_group_id, writable,
updated_at}`. IDs are strings or null; `path` is current root-to-node names;
`updated_at` is a server UTC timestamp or null before any selection. No item data
or counts. Resolve current visibility, ownership, ancestors and permissions on
every read, never return saved stale names. The read ceiling is Restricted.

UDS capability `ui_active_group_v1` gates the Mac writer independently of HTTP
`url-jobs-active-group-v1`; these names are intentionally not interchangeable.
UDS `ui_active_group_set` takes `{request_id, group_id, max_sensitivity}` and
acknowledges `{schema_version:1, request_id, group_id, updated_at}`. Non-null IDs
must identify active visible owner_local nodes. `null` clears selection when the
user explicitly selects Inbox/All/an independent global search view. Filtering
inside the current folder is not navigation and does not clear that selection.
State is one local-owner selection;
last successfully accepted write wins in server serialization order, not client
time. Persist request identity: exact replay returns its original acknowledgement
without making the old selection latest again; changed parameters conflict.
This UI state does not move items or grant filing authority. Read-only nodes may
be recorded but have `writable:false`. Missing/inactive/foreign/invisible current
selection returns null IDs, empty path and writable:false, with no stale names.

Mobile gateway method `memex.library.ui.active_group.set` requires an active
`edit` grant; it supplies effective max_sensitivity and namespaces request_id by
the authenticated principal. No caller-selected owner or client timestamp.
Local-owner UDS is the Mac writer. Both clients send only explicit navigation
selections, not initial cached rendering or background refreshes.

The extension snapshots the exact selected group ID into the initial enqueue;
job execution and unknown-outcome reconciliation never resolve current UI state
again. Null, read-only, virtual or invalid selections stop an
unchecked-Inbox submission with an explanation; never silently fall back to
Inbox. Entity roots and nested folders are supported only by the separately
gated destination contract below. UI selection never authorizes removing memberships.

### Explicit destination modes (new contract; not yet advertised)

Require `url-jobs-destination-v1` in addition to the existing no-generation and
enrichment gates for this increment. Optional `destination_mode` is either
`inbox` or `selected_group`, persisted and bound into request identity. Omission
preserves legacy serialization, digest, routing and replay semantics; historical
no-target jobs must not be reinterpreted as podcast enrichment.

Both modes require `generation_policy:"none"`, `quality_check:"off"` and
`include_comments:false`. `inbox` forbids target and removal IDs. Inbox means
global unfiled, never a folder named Inbox. Recognized Amp episode URLs use the
same durable deduplication/enrichment worker: new items receive no membership;
existing items retain all memberships and are described as “已存在，位置未变”.
Ambiguous duplicates reject; trash is never restored. Other URLs remain ordinary
capture jobs, not podcast jobs merely because generation is disabled.

`selected_group` requires an exact `target_group_id`. Active visible writable
owner_local entity roots and folders at any depth are legal. Admission and worker
commit validate the complete ancestor chain. Default empty removal IDs only add
membership; other locations remain intact. Explicit removal IDs retain the old
0–2 same-root root/direct-child restrictions; clients must not synthesize them.
Selected destination is frozen at first enqueue. Changing the current UI selection
cannot redirect a job; exact unknown replay uses the original key and payload.
Existing durable identity is reconciled before new-request admission, without
granting the worker additional authority.

### Podcast enqueue DTO and exact filing scope

The direct-child target requirement, mandatory podcast target and no-target
ordinary-capture routing below describe **legacy requests omitting
`destination_mode`**. Explicit modes use the preceding destination contract;
shared authentication, replay and explicit removal constraints still apply.

Authenticated `POST /api/v1/groups` accepts exactly `{"schema_version":1}`
under the same Origin/secret/nonce rules as capabilities. Advertised by
`url-jobs-group-picker-v1`. Returns `{schema_version:1, groups_version, nodes}`;
each node has only `group_id`, `name`, `kind`, `parent_id`, `root_group_id`,
`writable`. No items, memberships or counts are returned. Nodes are active,
owner_local and visible at the enqueue ceiling (Restricted). `writable` also
requires a visible writable owner_local root. `groups_version` is the SHA256
hex of the sorted public node projection, not a Store mutation CAS token.
Clients select a writable direct child collection of a writable root as target;
removal choices are that root or its writable direct child collections, excluding
the target. Enqueue revalidates current state; a picker snapshot grants no write.

For a previously unaccepted request key, unknown/nonwritable/invalid-hierarchy
target or removal groups reject admission with HTTP 400 `invalid_request`;
ambiguous existing episodes reject with HTTP 409 `podcast_duplicate_conflict`.
That admission rejection creates neither a job nor a request alias. It does not
prove that a different key for the episode has never been accepted.

Persisted request identity and registration are checked BEFORE current
group/duplicate admission. Slow Store admission runs without a queue writer
lock. The final queue transaction rechecks identity before applying either the
admission result or an insertion, so a concurrent acceptance wins over a stale
rejection. An accepted key with exact
original semantic parameters returns its original job receipt even if admission
would now fail. Changed parameters yield `request_identity_conflict`; a different
registration receives `resource_not_found`, never the old receipt. Neither
identity/auth rejection proves the original write did not happen. Unknown-outcome
clients retain the original key and parameters, not a new ID or repaired payload.
Reconciliation is receipt access under the existing registration, not permission
to write an invalid target: worker commit still checks current Store permissions.
New keys (including new aliases for an existing episode) must pass current
admission before any queue insertion. Store/read failures remain unknown or
unavailable, not evidence of nonacceptance.

The production daemon prepares a shared in-memory exact-URL index before HTTP
readiness. It follows Store's generation/head-stamp freshness contract, retains
no bodies, and verifies matching canonical metadata again. After Store writes,
only changed heads need metadata reads; trash and group authority remain live.
Final episode commit does not rebuild or wait for this index while holding the
queue writer lock: a stale/busy index aborts that transaction, refreshes outside
it, then retries the original lease/identity checks (bounded to three attempts
before the ordinary durable retry state). Cold startup or external Store writes
can still delay readiness/reads; this is not a universal latency guarantee.

The ordinary enqueue payload adds `target_group_id: string` and
`remove_from_group_ids: string[]` (default empty). Podcast mode requires the
target, `include_comments:false`, `quality_check:"off"`, and
`generation_policy:"none"`. No item_id is accepted. Optional `source_hints` are
untrusted display hints, not adopted as evidence; the client may omit them.
Missing target means ordinary capture, not implicit filing by a folder name.

The removal array has **at most two entries before deduplication**. The server
then sorts/deduplicates it for semantic request identity. A target in the removal
array is rejected. All IDs are exact persisted identities, never names. Target
must be a writable direct child collection of a visible writable root. Sources
must be that same root or its direct child collections. The worker includes only
actual memberships among those explicit sources in the atomic filing operation;
all other locations, including nested same-name collections, remain unchanged.
CAS binds item revision and each participating node version.

The authenticated backend checks existing source/canonical URLs before enqueue;
different request keys with the same producer, exact URL and semantic options
share one durable job. Changed options conflict instead of silently changing the
original job. Unknown-outcome reconciliation reuses the original key and full
original payload; never retrofit fields onto an old key. The worker verifies
the URL against the fixed RSS GUID identity and rechecks existing items inside
the serialized commit section. Multiple existing copies are not re-imported;
only a unique already-filed copy may be enriched, otherwise the job stops with a
conflict. Duplicate deletion is a separate explicitly authorized CAS operation.

Podcast receipts additionally expose `enrichment` with `state` (`pending`,
`complete`, or `partial`), `cover` (`stored`, `preserved`, or `unavailable`),
`metadata_conflicts` (field names only), and `archived`. Completed enrichment also
includes `placement`: `selected_group` means the frozen explicit target was filed;
`existing_locations_preserved` means Inbox mode retained existing memberships
(display “已存在，位置未变”); `unfiled` means the resulting item has no membership.
`archived` is false for both Inbox outcomes. Placement is a job receipt, not a
live membership query; later owner moves do not rewrite historical receipts.
Partial cover/metadata
results do not claim full completion. Other URL jobs retain their prior shape.

### Deterministic server RSS extraction

`memex_intake::url_jobs::podcast::episode_from_feed` accepts the exact bytes
obtained by the trusted worker from `https://ampcode.com/podcast.rss` and an exact
requested HTTPS Amp URL. It does not fetch, write Store, accept client metadata
as evidence, or call a model. Caller retains those RSS bytes and verifies source
transport separately. Parser limits: 2 MiB, 20,000 XML nodes, 1,024 items; no DTD
or external entities. Duplicate links/GUIDs, ambiguous scalar fields, invalid
dates/numbers and missing requested episode fail closed with fixed error codes.

EpisodeMetadata contains episode_identity, url, guid, season, episode,
rss_sha256, source_start_byte/source_end_byte (original RSS UTF-8 byte range),
published_date, published_instant, duration_seconds, duration_raw. Identity is
SHA256 of the JSON tuple (exact feed URL, trimmed GUID), not title/body hash.
Publisher changed raw data yields a new RSS digest, not a new episode identity.

Publication date is PartialDate(day) from the RFC2822 pubDate's original offset
calendar date, with original raw and RFC3339 instant retained; it is not converted
to UTC/Asia-Taipei calendar day. Missing date remains null; invalid is an error.
Duration is positive u32 seconds, accepting integer seconds, mm:ss or hh:mm:ss;
non-leading colon components are exactly two digits <60. Overflow, signs,
fractions, zero and malformed values are errors, not guessed values. Missing
duration remains null. Persisted source evidence and conflict-preserving metadata
application are separate steps, not implied by extraction success.

Cover storage/read contract: `memex.item_cover_v1.md`. Source verification,
network limits, URL job application, target membership and installed daemon
acceptance remain separate outstanding gates; neither parser nor cover Store
tests enable this increment on their own.
