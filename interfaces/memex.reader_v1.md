# Long documents and paired editions — reader_v1

Backend CLI/Store/owner-UDS contract. Frontend wiring and production daemon
installation are separate delivery steps; capability discovery is authoritative.
No model requests, OCR, remote metadata fetches or new network scopes are implicit.

## Import one paired book

```sh
memex file convert ORIGINAL.pdf --out original-conversion
memex file convert TRANSLATION.pdf --out translation-conversion
memex book prepare --original original-conversion --translation translation-conversion \
  --original-language en --translation-language zh --title 'Book title' --out paired
memex book import --bundle paired --library LIBRARY
```

`prepare` is local and network-denied. Original PDFs, raw engine output, previous
converted Markdown, converter receipts, chapter-aware editions and manifest remain
in the checksummed bundle. Existing output is never overwritten. PDF outlines and
explicit numbered parts provide structure; ambiguous numbering/overlapping outline
pages preserve the complete side as unstructured text without counterpart links.
Unstructured documents do not automatically invoke Luna.

`import` rechecks all preparation hashes and both converter provenance chains
before any Store write. It creates one private document through ordinary ingest;
exact bundle retries return the same receipt/item. Re-preparing the same PDF pair
with changed metadata/timestamps causes an immutable-input conflict, not a second
item. Retain/reuse the prepared bundle for retries.

The translation is the canonical readable/searchable Markdown for compatibility
with existing readers. Both editions have independent immutable Evidence. All
source files are copied into `Library/reader-blobs/<sha256>` before the ingest
journal references them. An interrupted preparation may leave unreferenced blobs;
do not garbage-collect them without a separate verified retention operation.
Full backup/restore must include `reader-blobs` and `catalog/reader-state.json` along
with the existing canonical Library. No SQLite hot copy, socket or Keychain export.

No new ItemRevision schema or owner-edit activity is introduced. Existing readers
can read the translation; they do not acquire bilingual controls automatically.
Direct Markdown edits invalidate the paired manifest if canonical content changes;
the reader fails with `stale_book_manifest`, rather than displaying stale translation
as current. Evidence and historical revisions remain intact.

### Correct extraction without creating a duplicate

```sh
memex book import --bundle corrected-paired --library LIBRARY \
  --update-item ITEM --expected-revision REVISION
```

This explicit CAS refresh validates the prepared bundle, the same source PDF pair,
unchanged original edition, and identical translation chapter keys/titles/levels.
It uses the existing FileReconversion transaction: a new revision and correction
Evidence, not another item or an overwrite of historical Evidence. A stale expected
revision rejects; reconcile the current revision before another write.

The immutable original manifest remains available. For a verified reconversion
matching the current canonical hash, readers rebuild translation byte ranges from
the same headings and return the correction's Evidence ID. Original-side content
and chapter identities do not change. Old revision-bound anchors remain stale;
they are not silently relocated. Compatible reconversion readers must be installed
before applying the refresh; new `reader_v1` transport availability is separate.

PDF adapter version 2 honors document-scoped StructTree `ActualText` before reflow.
Unsupported cross-page, unresolved-page or covered nested marked-content scopes
reject conversion rather than silently duplicating replacement text or leaking
corrupt glyphs. This is not arbitrary tagged-PDF support or a full-book accuracy
guarantee. Adapter 1 receipts remain readable but their hashes do not prove that
their extracted text is correct.

### Paragraph restoration and format audit

Adapter 3 identifies itself as `memex/pdf-layout` version 1, retaining the pinned
converter environment and `pdfplumber` version in its receipt. It carries corrected
ActualText alongside line coordinates instead of flattening every printed page to
one paragraph. Indentation, line gaps and font transitions provide paragraph hints.
Cross-page continuation additionally requires matching font/margins and text near
both physical page edges; explicit outline chapters are processed separately.
Bottom-centred numbers are removed only with a three-page sequential pagination
series. Isolated years and uncertain mixed-font pages remain in the reading text.
These heuristics do not certify arbitrary multi-column, table or OCR layouts.

`memex file audit-format INPUT.md --out AUDIT` audits Markdown from any converter
(PDF, Word, web), never changing the source, item, Evidence or model settings.
Optional `--allow-model --database AGENT_DB --model SAVED_MODEL_ID` explicitly sends
at most four nonoverlapping UTF-8 windows, each at most 4096 bytes, in one request
with low effort and a 1500-token completion limit. Select `luna` when configured.
The first/middle/end and longest-block windows are sampled, not the entire book.
No model retry occurs; an existing output directory refuses reuse. A durable
dispatch marker records uncertain outcomes so interruption is not mistaken for
an unbilled request. Only validated enum findings, hashes, offsets, configuration
identity and token usage enter receipts; no excerpts or credentials are logged.

For a bounded follow-up, `--reference SOURCE_LINES.json` accepts caller-supplied
PDF line geometry bound to the input Markdown hash. The object has `input_sha256`,
`source_sha256` and `samples: [{id, lines: [{page, x0, top, size, text}]}]`.
IDs must exactly match the selected windows; each reference is limited to 128
lines and 4096 UTF-8 text bytes (at most 16 KiB additional source text per call).
Source hashes are recorded, not independently authenticated by this option.
The model can identify suspect zero-based `reference_lines`; only validated
indexes, never their text, appear in reports. Partial references are not full
source-layout coverage and do not turn a sample finding into certification.

Model findings are a review signal, not proof of PDF geometry or original wording.
The report remains `quality=not_certified` even if all sampled windows have no
detected issue. This shared auditor is not yet a Word/web formatting transformer
or an automatic whole-library quality job. PDF repair remains deterministic; model
suggestions cannot silently rewrite prose, delete content or alter source anchors.

### Reusable Low dial mechanical tasks — CLI v1

The user requires reusable CLI/backend operations, not book-specific scripts or
frontend-owned model calls. Low dial is a user-configurable cost/task route for
mechanical work such as sampled format inspection and sparse bilingual matching.
Flash, Luna and Haiku are examples of selectable models, not hardcoded model IDs
or a claim that all their provider APIs are currently supported. Low dial does not
mean unconditionally sending `reasoning_effort=low`: provider request parameters
must follow the selected model's supported contract. Existing `audit-format
--model` remains a legacy per-call route forcing low effort. The shared route is
selected with `--low-dial`; it uses the existing OpenAI-compatible adapter, not
native Anthropic/Gemini APIs. No model readiness claim follows from configuration.

```sh
memex low-dial show --database AGENT_DB
memex low-dial configure --database AGENT_DB --model SAVED_MODEL_ID \
  --expected-revision 0 --effort omit --max-input-bytes 65536 --max-completion-tokens 8192
memex file audit-format INPUT.md --out AUDIT --allow-model --low-dial --database AGENT_DB
memex file align-samples --original ORIGINAL.md --translation TRANSLATION.md --out ALIGNMENT
memex book align --library LIBRARY --item ITEM --revision REVISION \
  --chapter chapter-2 --max-sensitivity private --out ALIGNMENT
```

The independent `memex_low_dial_v1` table stores one CAS-versioned profile. Use
the revision returned by `show` for subsequent updates. Configuration never
rewrites existing chat/translation selections or probes credentials. `omit`
actually omits `reasoning_effort`; `none`, `minimal`, `low` send those exact values.
The input budget includes the serialized input and system prompt in UTF-8 bytes,
not an estimated token count; limits are 1024–262144 bytes and 64–16000 completion
tokens (2026-11 owner-subscription amendment; previously 1024–65536 and 64–4096; CLI
defaults 65536/8192, previously 32768/1024). They are runaway guards, not cost caps. Request usage is provider-reported, not independently metered or a price.

Both alignment commands default to offline JSON preview: counts, sample ranges,
identity hashes and zero anchors/requests. Preview does not create `--out`.
Adding `--allow-model --database AGENT_DB` permits one bounded request using the
saved profile. Default sampling is three distributed positions (configurable
1–7); each offers up to three original and seven nearby translation paragraphs,
each excerpt at most 512 UTF-8 bytes. Candidate positions are guesses, not matches.
Overlapping windows reference a deduplicated paragraph pool: each candidate's
text is sent once, and each sample explicitly lists the IDs it may select.
New requests also enumerate legal contiguous groups and ask the model to choose
one option index per side, rather than construct arbitrary ID lists. The backend
resolves choices locally and applies the same candidate/range checks. Old completed
v1 receipts retain their original prompt/input identity and remain reusable without
a new request; a failed old receipt is not retried by the protocol change.
Preview reports actual `sample_text_bytes` and `repeated_text_bytes_avoided`;
these are byte counts, not token estimates. Byte offsets stay local to the index,
not in the model's candidate pool. No candidate is added merely to save tokens.
Only validated model-selected contiguous 1–3 paragraph spans become anchors;
uncertain, unmatched, crossing and overlapping selections do not become links.

Model execution writes `intent.json`, `dispatch.json`, then either `result.json`
or `failure.json`; successful alignment also writes `alignment.json`. When a
provider response is received, `response.json` records typed usage and whether its
text is syntactically JSON **before** task validation; invalid results can still
be billed. Failures include a fixed stage/code, not raw parser diagnostics.
Candidate-validation failures also include a fixed reason (count, outside
candidates, noncontiguous, or IDs attached to unmatched/uncertain), numeric sample
ID, at most four returned IDs per side, and the bounded allowed IDs. Unknown
response fields are rejected without echoing their values into these diagnostics.
Missing provider usage remains unknown. Receipts
contain IDs/enums, hashes, offsets and typed usage, never sampled text. Reusing
the same output directory with the same bound inputs/profile/route/prompt replays
a revalidated completed result without contacting Keychain or the provider.
Incomplete/mismatched/malformed attempts refuse reuse; no silent resend. Changing
the output directory explicitly permits a new attempt, so callers must retain
and reuse the operation's directory to prevent duplicate dispatch. These files
are local derived caches, not cryptographically authenticated provider receipts.

The index binds both chapter-body hashes and sampler identity. `book align` also
binds item/revision, full-edition hashes and each chapter's start offset; it reads
only the requested paired chapter through permission-checked 32-KiB chunks.
Anchor offsets are **chapter-relative UTF-8 bytes**. They are navigation hints,
never proof that whole spans translate each other or valid cross-language citation
coordinates. No source text, canonical item, Evidence or reading state is changed.

Keep extraction, checksums, chapter partitioning, bounds checks and cache lookup
deterministic. Use a model only for bounded judgment that those steps cannot make.
Each model-enabled operation must bound supplied text, request count and completion
size, retain actual token usage when reported, and return compact validated IDs or
enums instead of rewritten source text. Report absent usage as unknown. Do not
silently retry, upgrade to a more expensive model, alter saved configuration or
start whole-library processing. Selecting a route alone does not authorize source
disclosure or an inference call. Frontends consume the same CLI/backend results.

For bilingual reading, retain both editions unchanged and build a derived sparse
correspondence index: start with a few distributed samples within a paired chapter,
offer nearby target candidates, and eventually refine only uncertain or inconsistent
intervals within an explicit budget. V1 does not automatically refine or interpolate.
Support many-to-many spans and unmatched
content; bind cached results to both edition hashes and the task/model configuration.
Prefer processing the requested chapter to eagerly processing the whole book.
Between verified anchors, progress estimates may provide approximate navigation,
never precise semantic citations or highlight coordinates. The acceptance target
is useful low-cost approximate reading synchronization, not exhaustive paragraph
equivalence. Validate a bounded chapter trial before extending coverage. Frontend
anchor navigation is separate; no daemon/frontend alignment capability is
advertised by the offline index commands.

### Explicit whole-book alignment and index installation

`book align-all` takes `--library`, `--item`, `--revision`, `--max-sensitivity`
and `--out`, with the same `--samples` setting. Without `--allow-model` it previews
every explicitly paired chapter without making requests or writing an index.
Model execution additionally requires `--database` and an explicit `--max-requests`
at least as large as the chapter-task count. Tasks run sequentially against a frozen
model/profile snapshot. Per-chapter output directories are hashes of chapter keys;
completed caches revalidate without dispatch, incomplete attempts do not resend.
Reports separate elapsed time, accepted requests/tokens, unknown usage and failed
chapters. Failed task usage remains in its receipt and must be included in costs.
Any failed chapter prevents creation of a new complete `book-alignment.json`.

`book save-alignment --library LIBRARY --bundle book-alignment.json
--expected-version missing --max-sensitivity private` validates and atomically
installs the derived index. Later changes require the current version hash;
same-content replay is safe. `reader ... alignment ITEM --revision REVISION`
returns that version and index. Store validation checks current access/revision,
all paired chapter identities/order, canonical/edition/chapter hashes, UTF-8 bounds
and noncrossing anchors. Both texts and Evidence remain unchanged. All chapter
tasks completing is not a semantic accuracy certificate: unmatched/uncertain
samples can leave chapters without anchors and must not be reported as full
paragraph coverage.

### Chapter paragraph repair/alignment — v1 contract

This section records the owner's post-v0.1.26 requirement and supersedes sparse
navigation for the new paragraph UI. The v1 implementation was released with the
v0.1.28 clients; installed capability discovery remains authoritative. Later
extensions below are not implied by that release. Existing `reader_v1` and old sparse
indexes do not imply availability. Never convert sparse anchors, paragraph counts,
byte fractions or scroll fractions into asserted paragraph correspondences.

#### Whole-book consent and legacy AI-map repair — additive freeze

This freezes the corrected frontend proposal artifact bf4a1ffa-e128-4778-9a4f-d0931cff81cd
revision 1 for synthetic implementation, NOT installed capability or permission to
run real model jobs. Frontend design approval and actual target/data-use/cost consent
remain required. Generic repair/release permission does not itself authorize these
new persistent feature contracts or paid runs. Old operations/schema remain intact.

Capability reader_chapter_repair_batch_v1 adds align_batch_preview/start/status/stop
(full op names retain the align_batch_ prefix). Gateway names have the existing
memex.library.reader. prefix; the session field reader_repair_batch is independently
available|unavailable. Preview/start/stop require edit authority; status requires read.
Freeze and enforce principal/library binding, current grant/trust and ceiling at
admission, before each external dispatch and at commit, not just when previewing.
A read-only caller cannot spend, stop another principal's work, or learn its private
job details. Persisted permission snapshots are not a substitute for rechecking
live authorization. The current gateway SessionGrants is an in-memory authority;
its before/after-RPC assertCurrent checks do not reach later Rust queue execution.
Existing content_format::authorize checks Store visibility with a ceiling snapshot,
not live remote session expiry/revocation/trust. Neither proves this new requirement.

For this synthetic implementation, inject an explicit live-authority verifier into
the execution seam and test admission, each external dispatch and commit separately.
Bind the check to the authenticated origin, principal/library, grant/session epoch,
required scope, effective ceiling, target and operation stage. Missing verifier,
unavailable authority, expiry, revocation, epoch mismatch or trust loss must deny the
dependent action, never default to local_owner or an earlier allow result. Test
overrides are fixture-only, not request flags or a production allow-all fallback.
Persisting a non-secret authority reference for reconciliation is allowed; persisting
an allow decision does not extend permission. A remote-origin job remains remote
across restart, even when its request arrived through the owner's UDS.

Until an authenticated gateway-to-daemon live-authority seam is implemented and
verified, production remote discovery for BOTH new capabilities stays unavailable
and their new gateway operations must fail closed before preparing/accepting work.
Installed Rust code alone cannot make session.info advertise these features. The
existing read/translation/format capabilities are not changed by this temporary gate;
their background revocation coverage is a separate unresolved audit item, not deemed
proven by their Store-level authorization tests. Independent Store map and local
synthetic state-machine work may proceed. Actual local-owner authority is a distinct
trusted invocation contract, not a remote fallback or a string supplied by a client.

The later production seam must specify authenticated caller/channel ownership,
replay-resistant check identity, expiry/revocation and disconnect behavior, bounded
failure handling, and the authorization ordering relative to dispatch and Store CAS.
A successful remote check followed by an unfenced commit is not proof of atomic
revocation. Until that boundary is frozen, authority/channel loss prevents subsequent
model calls and new commits; reconnection does not automatically resume spending.
An already dispatched call may incur cost and an already committed receipt remains
valid evidence: retain its actual/unknown usage and reconcile without resending or
claiming cancellation. Loss of authority must not expose the response or new private
job details through a revoked session. Do not hold Store/queue writer locks across
network authority checks or introduce an unauthenticated loopback control endpoint.

Protocol design frozen for isolated synthetic implementation, NOT activation approval:
use the production candidate in delivery artifact d8dbdb10-3c7e-4d0d-ace9-4db6242622b4
revision 49, constrained by the following rules. The daemon may hold
an ephemeral grant/channel-epoch registry, updated/revoked by the authenticated
gateway over a separately authenticated, replay-resistant local control channel.
Restart starts with an empty registry. Registry entries are short-lived authority
mirrors, not durable authorization snapshots. Bind publisher/channel epoch and
library/principal/session/grant epoch; stale updates cannot resurrect a revoked
entry or a disconnected publisher. An ordinary phone request or owner-data RPC
must not be able to register grants or claim a local-owner origin.

Define revocation ordering at the daemon's receipt/processing of authenticated
revocation or observed channel loss, under the same local authority fence used by
dispatch and Store publication. The remote button click is not this boundary.
Freeze maximum lease=5000ms, renewal cadence=1000ms, no expiry grace. This does NOT
add a five-minute or other whole-batch deadline; the original grant lifetime is not
extended. A daemon-issued single-use challenge records sleep-inclusive monotonic t0.
The gateway returns R=min(5000ms, original grant's actual remaining lifetime), R>0;
the daemon deadline is t0+R, not response-arrival+R. At now>=deadline, reject.
Challenge expiry is never extended; expired or already-consumed challenges reject.
Keep renewal state bounded, at most one outstanding challenge per grant/channel.
Both ends require a verified sleep-inclusive continuous clock; on Mac the proposed
mach_continuous_time adapter must be exercised, not replaced by unverified default
Instant or Date.now. Initial wall-clock conversion is conservative; rollback cannot
increase remaining lifetime and observed forward expiry revokes. Check time after
wake before actions. Unavailable clock guarantees fail closed. Delayed or replayed
renewals, suspension and channel replacement must not extend authority.
The gateway renews only while the actual current grant/channel/trust remains valid;
a heartbeat of an otherwise live gateway is not enough. Reconnection/new epoch does
not reauthorize persisted jobs or resume spending by itself.

Control authentication design: use a dedicated owner-only UDS plus peer-UID checks
AND a library-scoped 256-bit random key, proposed for OS Keychain storage with access
limited to the approved publisher and daemon. UID or a generic Node executable is
not authenticated publisher identity; do not grant a broad interpreter Keychain ACL.
If the current publisher cannot be constrained appropriately, production stays HOLD
until a bounded signed-helper/identity design is approved. This does not claim
protection against a compromised authorized publisher or another actor already able
to extract that key. Do not reuse model-provider, pairing or another product's keys.

Mutual authentication binds fresh daemon-boot/publisher nonces, channel epoch and
key ID. HMAC-SHA256 covers direction/domain, unambiguous lengths and exact raw payload
bytes, including operation, all identities, scopes/ceiling, sequence, challenge and
remaining lifetime; verify with constant-time comparison before trusting contents.
Frames are at most 64KiB with strict schemas and strictly increasing per-direction
sequence; malformed, reflected or replayed frames fail closed. Secret bytes never
enter control payloads, durable queues, logs or ordinary configuration. Keep revoked
epoch tombstones for the daemon boot; do not recycle an epoch after disconnect.
Rotation first revokes old registrations/closes channels; no dual-key grace that
continues old jobs. Provisioning, actual ACLs and rotation/cleanup remain unapproved.

The fence must cover the check and the local irreversible-action boundary, not just
return an allowed ceiling before a later transaction. A delayed, reusable allow
token or queued send can outlive revocation. Persist one attempt intent, prepare the
connection outside the fence without sending application-request bytes, then recheck
authority/time/epoch/job/stop/config and consume a one-use permit with the first actual
nonblocking request-byte handoff inside the fence. Enqueueing a future or accepting
bytes into a hidden client/TLS buffer is not evidence of this boundary. A zero-byte
WouldBlock requires readiness wait outside the fence and a fresh check, not reuse of
an old allow. Any partial handoff counts as dispatched; no automatic redirect/retry
or duplicate body send. Already-dispatched work may complete and incur cost; a crash
across intent/handoff/receipt remains conservatively unknown, never auto-resend.

Store commit/revocation uses the same local order and a Store-internal final CAS gate,
not merely an outer verifier returning a ceiling. Prepare/validate immutable content
outside the fence; use a uniform Store/queue/authority lock order, with revocation
taking only the authority fence. Publication of map or current revision has one
defined irreversible visibility point. Fence acquisition/final gate rechecks expiry;
do not hold it across provider/network waits. Filesystem publication may block, so
5000ms is an authorization-lease bound, NOT a hard real-time button-to-revocation or
filesystem completion guarantee. Post-publication sync/receipt uncertainty is not
permission to overwrite/rollback or repeat the job. Receipt reconciliation remains
internal, zero-dispatch and does not publish an uncommitted result; outward status
requires current caller access. Real transport and Store crash/race tests, including
zero/partial handoff and revoke-before/after-publication, must prove these boundaries.

Production HOLD remains until control-channel authentication material provisioning,
ownership/permissions, rotation/revocation and cleanup are explicitly approved, and
the exact lease/disconnect/epoch/fence protocol has synthetic crash/race/replay/expiry
evidence. No new key installation, persistent ACL, listener or production registration
is authorized by accepting this direction. Continue independent synthetic wire and
own-revision progression acceptance; existing production features and the cold-build
release are outside this gate. Existing remote-job revocation audit remains open.

align_batch_preview accepts {item_id,revision_id}, or exclusively {item_id,batch_id}
to observe preparation. Return {batch_id,state,quote,error_code}, with preparation
states preparing|ready|failed. Preparation is local, no inference or Library mutation;
use the existing narrowly scoped Credentials::status rule only when necessary.
Keep preparation off the five-second RPC path. Store its bounded result in the
additive reader_repair_batches table; preview does not reserve the item's writer.
Enumerate only authorized, explicitly paired chapters whose valid alignment is not
aligned. Corrupt/unreadable artifacts are errors, not missing eligible work. Freeze
ordered chapter keys, both sides' hashes/bytes, alignment artifacts, input revision,
policy/prompt, Low profile/settings/route and credential generation. No future arrival
joins the list. An empty selection has no usable spending quote.

quote contains {batch_id,expires_at,item_id,revision_id,chapters,route,budget,
estimated_cost}; chapters rows are {chapter,counterpart,original_sha256,original_bytes,
translation_sha256,translation_bytes,expected_alignment_version}. Use the existing route and translation budget
field shapes and Unix-millisecond expiry, 15 minutes after preparation completes.
max_requests is at most 2N, with unchanged per-request Low input/completion ceilings;
total input/output ceilings cover ALL possible requests including prompts and both
repair stages. A first-input byte estimate is not the total cap. Use checked sums;
never truncate chapter lists or budgets to fit a response. estimated_cost=null means
unknown money, not free. New retries, larger profiles or fallback models are excluded.
Reserve/check remaining parent AND child budgets durably before every dispatch.
The current repair Record has no credential-generation guard; the new paths must
actually persist/check it, not assume reusing the old engine already enforces it.

align_batch_start takes {item_id,batch_id,request_id,accept_cost:true,
accept_model_processing:true}, returns {batch}, and consumes a ready unexpired quote
exactly once in its principal/library namespace. Persist the client request ID before
sending. Exact authorized replay returns the existing batch before expiry checks;
conflicting ID reuse or a second ID consuming the same quote rejects without calls.
One confirmation covers the frozen chapter set, provider data use, possible formatting
and proofreading changes and total cost ceilings, not arbitrary later content.

align_batch_status accepts {item_id,batch_id,offset,limit}, limit=1..100, and returns
{batch,offset,total,next_offset,chapters}. Chapter order is immutable; each row carries
chapter,job_id,state,error_code,result_revision_id,alignment_version,alignment_status.
batch contains batch_id,request_id,state,stop_reason,done,total,current_job_id,usage;
states queued|running|stopping|stopped|succeeded|failed|outcome_unknown. Counts and
nullable usage follow the existing format batch aggregation rules, including failed
and unknown cost; done counts succeeded chapters including explicitly partial results.
Pages are observations of progress, not a multi-page atomic snapshot.
align_batch_stop takes {item_id,batch_id}, is idempotent, returns {batch}, and is
serialized against child admission. stopping prohibits admitting the next chapter;
already admitted work may finish and incur cost. stopped is not claimed while a child
can still commit. Complete chapters stay committed; there is no whole-book rollback.

Reuse chapter execution, not an unbound frontend loop. Distinguish job_id from the
idempotency request_id. Derive a bounded, domain-separated child request ID from the
parent identity and ordinal; appending -cN to a valid 128-byte ID can overflow and is
not acceptable. Persist parent/child linkage and admission atomically. An active batch
reserves the item across ALL existing repair/translation/format/edit admission paths;
only its bound child bypasses its own reservation. Never leave a gap between chapters.
Check parent ownership and install its child in one IMMEDIATE queue transaction;
no external request accepts a bypass flag. Reuse an internal claim/execution seam,
not a public request that rejects its own reservation or a step that recursively
takes an execution lock already held by the parent.
Check the actual frozen settings/profile/generation before every call; injecting an
old profile into admission does not bypass drift rejection.

Advance only through this batch's committed result_revision_id, verifying untouched
chapter keys/content and existing-artifact bindings. Inherited effective alignment
versions can change with revision even when the underlying artifact did not: validate
the frozen source artifact and allowed own-receipt chain, then compute current CAS.
Do not compare an old derived version blindly or accept an unrelated latest revision.
External revision, alignment, permission or configuration drift stops further work.
Failure/unknown stops without resend. Partial successful results remain explicitly
partial; "batch succeeded" means jobs completed, not all paragraphs aligned. Fresh
preview may list a partial chapter again, but must disclose repeated work and new cost;
it must not claim every previously completed chapter is automatically excluded.

Restart first reconciles durable Store receipts/commit intents, child IDs and usage.
No new model dispatch is permitted by recovery of a batch or its child. An unfinished
batch stops with interrupted; only an actually uncertain dispatch is outcome_unknown,
not every never-started chapter. Preserve successful receipts and usage. A fresh
preview/confirmation is needed for further work after reconciliation; it does not
silently resend unknown jobs. An unreconciled active/unknown chapter cannot become
new eligible work merely because a fresh preview still finds it unaligned.
Generic reader_repair recovery/step must not autonomously requeue or execute
batch-owned children. Persist them separately or in states the old executor cannot
claim; an added ownership field ignored by old serde readers is insufficient.
Enforce this at crash boundaries; ignoring the parent table alone is insufficient.

Capability reader_translation_map_repair_v1 independently adds session field
reader_translation_map_repair and ops translate_map_preview/request/status/stop/read.
Preview/request/stop require edit; status/read require read and current/historical access.
Preview takes {item_id,revision_id,chapter,translation_version}, freezing the exact
eligible source, stored AI projection identity/digest, effective view, source Evidence,
Low settings/profile/credential generation, policy and bounded plan. Return
{eligible,reason,quote,map_version}; an existing valid generated or retrospective map
is a zero-call reuse, not a new quote. map_version is null when only the existing
generation-time map is present (reason=recorded_map_available); read that map through
translate_alignment instead. No counterpart substitution or formatting.
Quote follows the translation quote expiry/route/budget conventions with at most two
Low calls, not Purpose::Translation or a force-regenerate option. Unexamined blocks
stay unresolved; malformed responses do not authorize another call.
Reuse only the pure matching algorithm. Existing matching::pools reads the human
translation edition and repair::apply can change canonical text: neither is the
AI projection input/publication path. Bind both actual texts independently and
publish with a map-only receipt. Do not fabricate a human counterpart Plan.

translate_map_request takes {item_id,revision_id,chapter,translation_version,quote_id,
request_id,accept_cost:true,accept_model_processing:true,expected_map_version:"missing"}.
Return promptly as {job}, with operation=chapter_translation_map, common durable
job/usage/error fields and nullable map_version; stages queued|matching|committing|done.
Job states add stopping|stopped to the existing common state set. translate_map_status
takes {item_id,job_id} and returns {job}. translate_map_stop takes {item_id,job_id},
returns {job}, is idempotent and prevents further dispatch; an already sent call or
commit may finish with its cost retained. Reconcile receipts before declaring stopped.
Exact replay, quote
single-consumption, per-item exclusion, per-dispatch authorization/configuration
guards and commit CAS apply. Unknown calls are never automatically repeated; safe
receipt recovery precedes any new attempt. Existing translation discovery is unchanged.

translate_map_read takes {item_id,revision_id,chapter,translation_version,
expected_version,offset,limit}, limit=1..256; expected_version="any" is permitted only
at offset=0. Later pages echo the returned effective map version. Return the existing
alignment Page shape plus translation_version,basis:"retrospective_model_alignment_v1",
certified:false, with translation_chapter=null and quality="not_certified". Use pairs,
not a second unbounded unresolved array. Pairs partition every byte on both bodies
exactly once using the existing matched/one-sided/unresolved relations, complete
adjacent same-kind blocks, UTF-8 bounds and monotone noncrossing/nonoverlapping spans.
No usable correspondence has status=none, some unresolved means partial, and aligned
is not a semantic certificate. Missing map returns version="missing",status=missing,
total=0,pairs=[],next_offset=null at offset=0; do not permanently negative-cache it.
Changed map CAS rejects stale_chapter_alignment; changed projection rejects
stale_reader_translation; current revision drift retains stale_item_revision.
Bad proof is an error, never missing/fallback. Reading requires no quote or credentials.

Persist an immutable, schema-versioned chapter_translation_map artifact separately.
Bind original item/chapter/source Evidence, stored projection identity AND digest,
both full-body hashes/bytes, algorithm/route/profile/prompt provenance and the complete
pair partition. Never persist provider prose as diagnostic evidence. Stored version
hashes the complete serialized map; inherited effective versions additionally bind
current/source revisions using the existing metadata-view scheme. A changed effective
translation version alone does not change its immutable source identity. Inherit only
through the existing validated per-step unchanged-chapter/projection chain, including
historical protection; changed-then-restored or corrupt intermediate state never skips.
Publish atomically under current revision/projection/map CAS without altering source,
translation body/version/provenance, item revision or old receipts. Exact same-job
publication replay returns its receipt; a different installed map rejects the CAS,
never overwrites it. A corrupt current map or eligible historical proof is an error,
not permission to search past it for an older valid map. Old
translate_alignment remains missing for a legacy projection; clients discover this
separate map only via the new capability/read, labelled AI事后推断，未经人工核对.

Rollback retains all new artifacts/tables and old Library history; no deletion is
authorized. Older readers may ignore the new map, but older executors may still claim
ordinary queued child jobs. Therefore binary rollback requires verified quiescence
and durable reconciliation of all batch/map work, including queued and committing
children; otherwise retain a compatible executor. Do not merely ignore the new table
or assume an unknown payload field fences an older binary. Synthetic acceptance must
exercise stop/admission races, crashes before/after dispatch and commit, old-executor
behavior, ID/quote replay, config/grant/revision drift, partial retries, inheritance,
map pagination/version discovery, corrupt proofs and legacy zero-write/zero-dispatch
reads. Feature approval, durable-format review and verified rollout remain separate
from this synthetic implementation freeze.

The original and translation retain their language roles, authorship and immutable
source Evidence. Formatting repair may change either edition, not just canonical
Chinese. The existing translation-only reconversion path must be extended and its
recovery validation tested before applying English repairs to a served Library.
Changed formatting creates a new item revision through the owner-edit/reconversion
CAS journal, with new edition/manifest Evidence; old revisions and Evidence remain.
Changing only the derived alignment does not create a content revision. Unselected
chapters' text and identities must remain unchanged; shifted absolute byte positions
are recomputed. Existing revision-bound annotations remain stale, never relocated.

#### Deterministic formatting and bounded model work

The reusable CLI operations will be `book repair-chapter` (offline plan by default;
`--allow-model` enables the saved Low dial route) and `book apply-chapter-repair`
(explicit plan/application CAS). The same core operation backs the daemon job;
the frontend does not spawn a CLI or own a second implementation.

Inspect only the selected original chapter and its declared counterpart. Preserve
page/line geometry and traceable source ranges when available. A missing terminal
punctuation mark plus a lowercase next line is a candidate boundary, not sufficient
evidence to join paragraphs. Physical page-edge continuation, indentation, headings,
quotation/list boundaries and font/layout evidence also constrain repair. Visible
hyphens are not automatically removed; only corroborated line-wrap hyphenation can
change them. Repeated header/footer removal needs corroborating source evidence.
An isolated numeric line/year must not disappear. Missing layout evidence leaves
the boundary unresolved, rather than treating all Markdown blank lines as page ends.

Footnotes/translator notes become independent `note` blocks, with their markers
retained; an inline circled numeral alone does not justify moving/deleting a sentence.
No model rewrites prose. It selects boundary actions and paragraph IDs from bounded
candidate sets; the backend constructs and validates the result. Formatting
operations retain a source-range/edit map and text-conservation checks, including
an explicit accounting of removed furniture and approved soft-hyphen joins.

Use the saved Low dial profile (Luna for the user's selected current route), never
silently upgrade the model. Reuse successful bound work; no eager whole-book run,
whole-book prompt, automatic retry or neighboring-chapter inference. A job admits
at most two requests, each bounded by the saved profile's input-byte and completion
limits. Budget exhaustion or ambiguous matches leave explicit unresolved ranges;
they do not authorize more calls. Receipts record actual/unknown usage and elapsed
time separately. One chapter may exceed a prompt budget: `partial` is valid.

#### Read: paragraph correspondence, not citation equivalence

New daemon capability: `reader_paragraph_alignment_v1`. UDS request:

```json
{"command":"reader","max_sensitivity":"private","request":{
  "op":"alignment","item_id":"item_...","revision_id":"revision_...",
  "chapter":"chapter-1","offset":0,"limit":128,"expected_version":"any"
}}
```

Gateway method: `memex.library.reader.alignment`; params are the exact request
fields above without `op`. The read grant and sensitivity ceiling apply. Offset is
a nonnegative safe integer measured in **pair rows**, not bytes; limit is 1–256.
The first page uses `expected_version: "any"`; subsequent pages must use its returned
version. A changed index rejects with `stale_chapter_alignment`, preventing mixed
pages. No implicit model request or data write occurs on read.

Success lives in `response.reader` (gateway `result`):

```json
{
  "schema_version":1,"item_id":"item_...","revision_id":"revision_...",
  "version":"64-hex-or-missing",
  "original_chapter":"chapter-1","translation_chapter":"chapter-1",
  "original_sha256":"64-hex-chapter-body","translation_sha256":"64-hex-chapter-body",
  "original_bytes":160,"translation_bytes":140,
  "status":"partial","quality":"not_certified",
  "offset":0,"total":3,"next_offset":null,
  "pairs":[
    {"o":[0,120],"t":[0,90],"kind":"paragraph","relation":"matched"},
    {"o":null,"t":[90,140],"kind":"note","relation":"translation_only"},
    {"o":[120,160],"t":null,"kind":"paragraph","relation":"unresolved"}
  ]
}
```

The example illustrates a complete range partition; hashes are placeholders. Both
SHA256 values hash complete **chapter bodies**, exactly the bytes reconstructed from
`chunk`; they are not full-edition hashes. Stored identity additionally binds
canonical/edition hashes, chapter keys/starts, revision, algorithm and input profile.
Ranges are half-open UTF-8 byte intervals relative to their own chapter, including
any attached Markdown separators. They never use canonical Chinese coordinates for
English text. `translation_chapter`, its hash and byte count are null for a chapter
without a counterpart. Pairs never carry source text; fetch it through `chunk`.

Rows across all pages partition every byte on each available side exactly once,
monotonically without overlap, omission, split codepoints or crossing chapters.
Each non-null range is one or more complete adjacent blocks of the same kind:
`paragraph`, `heading` or `note`. This supports 1:1, 1:n and n:1 without forcing
footnotes into body paragraphs. Empty ranges and two null ranges are forbidden.
`matched` has both sides. `original_only`/`translation_only` assert an identified
one-sided block. `unresolved` has exactly one side: it means **unknown matching**,
not an assertion that the other edition omits the text. Unresolved intervals on
both sides appear as separate rows; they must not be displayed as an asserted pair.

`aligned` means complete structural coverage, at least one matched paragraph, and
no unresolved rows; `partial` means at least one matched paragraph and unresolved
rows remain; `none` means no matched paragraph. These do not certify translation
accuracy; quality remains `not_certified`. With no stored paragraph index, return
`version: "missing"`, `status: "none"`, empty pairs/total 0, no next offset, and the
current chapter identities. Coverage is then explicitly absent, not fabricated.
Here absence means no eligible index after the verified chapter inheritance below.
Old sparse indexes may be retained but do not fill this new result.

The frontend defaults to paragraph-interleaved reading. Parallel is optional and
scrolls to the corresponding **matched row boundary**, not proportionally through
text. One-sided/unresolved rows do not drive the opposite pane. Notes remain
accessible as notes, and cannot silently disappear from reading or coverage.

#### Verified revision inheritance of unchanged chapter artifacts

An item has a stable item_id; changing its title does not create another item.
Paragraph indexes and completed AI chapter translations may remain usable after
verified metadata-only revisions or the chapter-preserving transitions below.
This is a read-view binding, not a migration of
their immutable files or an assertion that a historical job ran on the new revision.
All ordinary reader requests still require the exact current revision.

Resolve the current revision's artifact first. Only actual file absence permits
walking the same item's fully validated ancestor chain, nearest first. An existing
valid index with status=none is authoritative. A present but malformed, invalid,
unsupported or unauthorized artifact is not absence: reject rather than searching
past it. A broken chain or denied historical access also rejects; do not skip it.

Before resolving any inherited view, validate the complete current owner-edit
chain, including each transition's Evidence. Every traversed parent is the same
item's input_ids[0], bound by its revision digest, and must independently pass the
existing full owner-edit transition validation. A revision hash match or activity
name alone is insufficient. Enforce current item/trash/access checks and historical
revision/Evidence protection, including the highest applicable source sensitivity;
never downgrade historical protection. A parent above the caller's sensitivity
ceiling rejects, not missing. Cached validation must retain the same guarantees.

The existing metadata_edit, bibliographic_metadata_edit, model_metadata_suggestion,
and bibliographic_lookup_apply transitions retain their whole-document rule:
canonical body bytes/hash, source URLs, Evidence identities/order, attachments, sensitivity,
and the complete projected editions/manifests and chapter identities/ranges/hashes
must be unchanged (apart from the revision binding). Compare both real editions,
not only the requested chapter or equal body hashes.

Additionally allow owner_content_format_v1, owner_content_format_restore_v1,
owner_reader_chapter_edit_v1 and reader_chapter_repair_v1, but only for a chapter
whose binding survives EVERY intermediate revision. Reconstruct each historical
document from its own validated revision/Evidence; do not copy the current source
Evidence identity into a historical projection. On every step require the same
original chapter key and exact UTF-8 bytes (SHA256 and byte count). A paragraph
index additionally requires unchanged counterpart presence and counterpart chapter
key, SHA256 and bytes. An AI translation requires that the original chapter still
has no human counterpart and remains an eligible source language. Other chapters,
whole-edition hashes, chapter start offsets and canonical hashes may differ on
these four transitions; chapter-relative artifact coordinates must not be rebased
by proportional offsets or guessed text matching.

A missing chapter key, changed counterpart binding, changed bytes on either bound
side, or loss of AI-source eligibility is a hard inheritance boundary. Return the
ordinary missing index/no completed translation; do not skip that revision to find
an older equal body. Thus changed-then-restored text cannot resurrect pre-change
artifacts. Unsupported transitions, including local_file_reconversion_v1 and
owner_markdown_edit_v1, also stop inheritance even if final bytes match. Invalid
historical documents, missing/corrupt Evidence, malformed artifacts and denied
access remain errors, never signals to skip a revision. Never reuse across items.

Validate an eligible historical paragraph index against its original full identity
and block boundaries, then bind its read-view identity to the current chapter and
validate again. Validate an AI projection, including any recorded map, against
its own original revision/source_evidence_id and exact source chapter bytes before
binding public metadata to the current revision. Retain all existing validation;
legacy unmapped translations remain unmapped, not reconstructed. Current valid
status=none still suppresses inheritance. This exception changes read eligibility,
not artifact storage, historical provenance, repair receipts or restore CAS.

A directly stored current artifact keeps its existing complete serialized-artifact
SHA256 version. An inherited view returns current revision_id and an opaque version:
SHA256 of compact UTF-8 JSON serialization of this ordered string array:
`["memex.reader.metadata-view.v1",kind,item_id,current_revision_id,source_revision_id,source_artifact_version]`,
where kind is `paragraph_alignment` or `chapter_translation`, and
source_artifact_version is the original complete serialized-artifact SHA256, NOT a
body hash or a previously rebound view version. Serialization uses serde_json's
compact string-array encoding. Thus both a new current revision and a changed
source artifact invalidate old view versions, even when body bytes remain equal.
No new wire field is required; clients echo the returned opaque version unchanged.
Body hashes, byte counts, matching rows and generation provenance remain original.

Paragraph read/pagination, repair plan construction, align_request admission,
repair commit and index save CAS MUST use this same effective-index resolution and
version under their existing snapshot/write locks. A read yielding an inherited
version must not be compared against current-path missing at admission or commit.
Existing exact replay semantics remain intact. Translation metadata, preview cache
lookup and translate_chunk likewise share effective projection/version resolution;
an eligible completed inherited translation is a zero-dispatch cache hit without a
credential probe. Stale page/chunk versions still reject, never restart silently.

Resolution performs no artifact writes, revision creation, model request or receipt
rewrite. Old jobs retain their actual input/result revisions and original usage.
Only an explicitly admitted new operation may publish a current artifact. Historical
reads stay bound to their requested historical revision; format restore CAS and
source citation coordinates are not relaxed by this exception.

Regression acceptance uses synthetic data: title-only and consecutive allowed
metadata changes preserve both artifacts; each newly allowed transition preserves
artifacts for an unchanged chapter, including changed earlier chapters that move
its edition offset. A changed bound chapter/one side, changed-then-restored text,
reconversion, whole-Markdown edits, same-title different keys and cross-item equal
hashes do not inherit. Current none/corrupt artifacts and eligible corrupt historical
proofs do not fall back; historical protection is enforced; inherited
read/plan/admission/save CAS agree; old page/chunk versions reject. Translation
metadata/map/chunk reads need no credentials or quote, and preview cache hits
dispatch zero requests. No artifact/receipt rewrite or automatic model call occurs.
Do not claim sensitivity-mutation coverage unless the Store supports and tests it.

#### Explicit job admission and observation

New daemon capability: `reader_chapter_repair_v1`. `session.info` adds independent
`reader_alignment` and `reader_repair` fields, each `available|unavailable` from
those actual daemon capabilities; existing `reader` continues to mean TOC/chunk.
Capability means code availability, not model readiness or permission to dispatch.

Write request (gateway `memex.library.reader.align_request`, existing **edit**
grant required; never admitted under read-only scope):

```json
{"command":"reader","max_sensitivity":"private","request":{
  "op":"align_request","item_id":"item_...","revision_id":"revision_...",
  "chapter":"chapter-1","request_id":"stable-owner-generated-id",
  "expected_alignment_version":"missing"
}}
```

`request_id` is persisted before sending, 1–128 ASCII letters/digits/underscore/
hyphen. Reuse the same ID and payload after unknown transport outcomes; conflicting
reuse rejects. A completed matching job/cache returns without another dispatch.
An incomplete dispatch, malformed reply or uncertain outcome of a single-chapter job is
retried automatically at most 2 times (backoff 30 s, 60 s; each retry adds exactly one
request to the job budget); after that it ends `failed`/`outcome_unknown` and needs a new
explicit user request (2026-11 owner-subscription amendment: the owner's route is a
subscription, not per-request billing). Only one active repair job per item is admitted; another returns
`reader_job_in_progress` without model calls. Admission validates current access,
revision, alignment version and a frozen saved profile, then returns promptly;
it never waits for inference inside the five-second reader RPC.

Read observation (gateway `memex.library.reader.align_status`, read grant):
`{"op":"align_status","item_id":"item_...","job_id":"job_..."}`. Both
admission and observation return `response.reader: {"job": {...}}`, with:

- `job_id`, `request_id`, `item_id`, `revision_id` (input revision), `chapter`;
- `state`: `queued|running|succeeded|failed|outcome_unknown`;
- `stage`: `queued|formatting|matching|committing|done`;
- `result_revision_id`, `alignment_version`, `alignment_status`: null until a
  successful commit, then the committed identities/status;
- `error_code`: null or a fixed sanitized code; no text, titles, URLs or raw errors;
- `budget: {max_requests,max_input_bytes_per_request,max_completion_tokens_per_request}`;
- `usage: {requests_dispatched,reported_total_tokens,unknown_usage_requests}` and
  `elapsed_ms`. Usage counts failures too; missing usage is unknown, never zero cost.

Prepare both corrected chapter bodies and the index before committing. Commit
revalidates permission, current revision and alignment CAS under the writer lock.
Do not hold that lock during extraction or model calls. No partial visible edit
on stale/failure; a successful content+index transition must recover consistently
after interruption. Changed revision makes all old reader requests stale; the
frontend reloads TOC/chunks and the new index. If formatting was unchanged,
`result_revision_id` equals the input revision. Only then may the frontend say the
chapter was reorganized; admission alone is not success.

New fixed errors: `invalid_reader_alignment_request`, `stale_chapter_alignment`,
`reader_job_in_progress`, `reader_job_not_found`, `no_low_dial_provider`,
`reader_repair_budget_exceeded`, `reader_repair_failed`; retain existing
`request_identity_conflict`, `stale_item_revision`, `provider_outcome_unknown`,
permission and resource errors. Unknown fields reject. Fixtures must cover missing,
matched 1:n/n:1, note, original-only appendix, unresolved ranges, stale revision/index,
job replay, failed/unknown dispatch and changed-revision completion. Synthetic job
fixtures must never invoke a provider or write the real Library.

### Proofreading, missing translations and chapter edits — extension v1

Frozen implementation contract, NOT installed capability. Source requirement:
owner request
(2026-11-04): proofread typos/garbling while organizing a chapter; translate an
English-only part, never an already paired part; enable book Markdown editing.
The "of he District" example is the frontend's report, not independently verified
source wording. No rule or fixture may special-case that book or sentence.

The old whitespace Edit/Plan validator stays unchanged. Text corrections and owner
edits need separately versioned, activity-bound proofs; merely appending a manifest
or weakening non-whitespace conservation cannot grant content-write authority.
Keep canonical/edition/history integrity checks and journal durability barriers.
Implementation belongs to the backend writer; this file freezes the shared seam.

#### Common wire, identity and consent

All new operations use the existing reader envelope and response.reader; gateway
methods are `memex.library.reader.<op>` with exactly the same fields except op.
IDs, chapter keys and request IDs follow the existing reader/job validators. Unknown
fields reject. SHA256 fields are lowercase 64-hex. Source ranges use half-open,
chapter-relative UTF-8 bytes, never canonical Chinese offsets for English.
All writes require edit scope. Session admission is checked before each gateway
dispatch; current resource permission/sensitivity/trash is rechecked before Store
mutation and receipt replay. Response delivery rechecks the current session too.
No new login, credential access path, provider fallback or global settings write.

Session authorization linearizes at gateway dispatch, as in remote/README.md's
"Write ambiguity" contract, not at Store commit. Dispatch means the authenticated
gateway starts one owner-UDS command after validating the current grant, generation,
phone trust, scope and ceiling; this is not mere frame arrival or upload creation.
No asynchronous admission queue may use a stale grant without revalidation before
starting that command. Socket connection/write and Store/SQLite lock waits may
follow dispatch; they do not move the authorization point to the later mutation.
Revocation/expiry after this point blocks new requests and response delivery, but
does NOT cancel that already-dispatched command, even if it is still waiting for a
lock. It may commit only after current resource checks and revision/hash CAS under
the writer lock. This version does not promise a cross-process revocable grant or
an atomic cancellation fence. Existing accepted durable jobs keep their bounded
consent; session loss grants neither a new job nor an automatic paid retry.

A denied pre-dispatch request has no Store effect. After dispatch, timeout, lost
connection or suppressed response is unknown to the client, not proof of rollback;
do not deliver even a receipt through a revoked channel. After fresh authorization
to the same principal/resource, use the original identity to retrieve/replay its
receipt without reapplying a committed write. Resource denial still blocks replay.

##### Relay room replacement and encrypted-channel lifetime

Connection stability changes do not change pairing identity, grant scopes, spending
consent or the dispatch authorization point above. A relay room/socket is transport,
not an authenticated application channel. The relay remains blind to library RPCs.

The local Mac-to-relay leg may use an explicitly configured loopback endpoint while
public pairing/page/WSS pins remain unchanged. Never publish the internal endpoint
in pairing material or silently downgrade a public WSS connection to remote ws.
Use the same session/role/mode admission capability, Origin rules and resource limits
on the local leg; loopback is not authentication. Under trustProxy, a fixed local
rate-limit source is permitted ONLY for the explicitly configured, verified local
Mac ingress after role=mac/mode=bridge admission. It is an accounting bucket, not
an unlimited exemption. Never select it from client forwarding headers, Host, query
or a claimed role. A socket appearing loopback because it arrived through the public
reverse proxy is insufficient. If local Mac ingress cannot be distinguished from
public proxied ingress, do not enable the exception. Keep public trusted-proxy
validation and all total/per-source/frame/byte/backpressure limits in force.

Freeze the local ingress as a separate loopback-only HTTP/WebSocket listener,
bound with port=0 and sharing the same relay room registry and aggregate budgets
with the public listener. Only this listener accepts the authenticated mac/bridge
local bucket; reject phone/browser/native admission there. Do not proxy or expose
it through public ingress. The public listener keeps its trustProxy rules, including
rejecting missing forwarding source when trustProxy requires it. The ephemeral
port is internal runtime configuration, never a hardcoded public/default endpoint.
Shutdown and failed startup must close both owned listeners without losing counts.

Phone departure must not by itself tear down the Mac transport. A same-role
replacement installs the newly admitted socket as that room's current role and
closes only the superseded role socket as the routing operation. Late messages,
close events or send-error callbacks belonging to superseded sockets must neither
clear current membership nor close the replacement/other current leg. Count each
socket exactly once, including rejected joins and replacements. Do not forward or
buffer frames for a future peer or replay a buffered RPC across a replacement.

Keeping the Mac transport does NOT preserve its old phone encryption channel.
Phone departure/replacement invalidates the associated pending handshake/channel
and queued response delivery. A newly connected phone must complete a fresh
authenticated handshake before any application RPC, even if pairing identity and
generation are unchanged. Use fresh ephemeral material/nonces/directional keys;
never reset counters while reusing a key. Bootstrap remains one-time, trusted
reconnect still proves the pinned phone identity, and revoked/expired grants never
become valid because a socket reconnected. Relay peer/reset notifications may only
invalidate transport availability; they cannot grant trust, reset a durable pairing
generation, install keys or prove handshake success. Any new control frame must be
bounded and separated from encrypted application frames, with no caller-supplied
authority or library data.

Freeze the invalidation signal as a relay-to-current-Mac WebSocket Ping control
frame whose payload is exactly the ASCII bytes `memex_peer_reset_v1`. It contains
no JSON, session ID, identity, counter or reason fields. Only BridgeRunner's ping
handler for its current relay socket recognizes the exact byte sequence. Ordinary
heartbeat pings/pongs and application data containing that string are not resets.
Phone-generated Ping is handled locally by the relay (ordinary Pong); never forward
it as a Mac control frame. No new phone protocol field is introduced.

For a genuine current-phone departure or replacement, queue the reset Ping to the
current Mac before forwarding any new phone handshake/application frames. Late
close/error callbacks from the superseded phone must not send a second reset that
invalidates its successor. If the reset cannot be queued, fail closed rather than
continue routing new-phone frames into the old channel. The notification proves
no authority: a relay able to inject it can interrupt availability, not gain access.

On receipt, invalidate pending/channel and advance the local connection epoch
before processing new-peer data. Fence receiveChain work captured before reset,
queued replies and asynchronous errors as well as already-running RPC callbacks;
do not merely set channel=null while old queued frames can later establish or use
it. Reset is idempotent when no channel/pending work exists. Never reset durable
pairing/grants or release accounting for still-executing owner commands.

For this increment Mac departure/replacement explicitly closes the phone with
4002 (Mac offline) or 4005 (peer replaced/reset), respectively, for a fresh
reconnect/handshake; do not promise seamless continuation or carry old in-flight
RPCs forward. There is no future-peer buffering or cross-replacement replay.
Fresh channels fence pending work by channel instance/epoch as well as current
socket, pairing generation, trust and grant. In particular, a late old RPC response
or error must not be encrypted under new keys, delivered on a new channel, consume
its counters, or close it through an old asynchronous error callback. A successful
new handshake does not resurrect suppressed old responses.

Already dispatched owner writes/jobs may finish under their original authorization
and Store/CAS guards. Disconnect is outcome_unknown, not cancellation or failure.
Keep durable operation IDs and reconcile status/receipts after fresh authorization;
do not automatically resend writes or create replacement request IDs. A transport
RPC id is not a durable idempotency key. Operations without durable replay support
require authorized readback, not blind resend; unknown paid attempts are never
automatically retried. Pure, side-effect-free reads may retry finitely after a new
handshake with fresh authority and current revision/version checks. Read scope alone
does not establish purity: preview/quote creation and write-shaped operations are
excluded from automatic transport replay. Preserve drafts and honest unknown UI.
Read concurrency/backpressure changes must keep bounded actual in-flight owner
work; timing out a client request does not release an executing owner command's
accounting or justify unbounded replacement requests. Do not hide contention by
increasing the established reader timeout or weakening authorization checks.

Synthetic acceptance covers phone departure with Mac retained, both role
replacements and delayed old close/send/RPC callbacks, a fresh reconnect with old
ciphertext rejected, grant revoke/expiry and pairing rotation during queued replies,
an owner write committed before response loss with no automatic resend, read-only
bounded recovery and queue saturation. Exercise local authenticated admission,
forged forwarded/local-source headers from public ingress, invalid role/mode and
rate-limit accounting. Test the exact reset Ping, ordinary heartbeat Ping, spoofed
phone Ping/application text, reset-before-new-hello ordering, stale receiveChain
work and duplicate old-socket close callbacks. No real credentials, library content or production restart
is authorized by this contract freeze; implementation, two-client integration and
any separately authorized deployment remain the owning threads' responsibilities.

Independent daemon features map to session.info availability fields:

| Feature | Session field | Meaning |
| --- | --- | --- |
| reader_chapter_proofread_v1 | reader_proofread | Opt-in correction plus correction-list reads |
| reader_chapter_translation_v1 | reader_translation | Missing English chapter translation, preview/request/status/chunk |
| reader_chapter_translation_metadata_v1 | reader_translation_metadata | Credential-free discovery of a completed chapter projection, with translate_chunk reads |
| reader_chapter_translation_alignment_v1 | reader_translation_alignment | Read-only paging of recorded source-to-AI-output block correspondence |
| reader_chapter_edit_v1 | reader_edit | Side-specific staged Markdown save |

Values remain available/unavailable; absent is unavailable. Old clients retain the
old behavior. Code availability is not inference readiness or spending consent.
The new actions must name their effect before the existing confirmation: correcting
reading text, or translating this chapter with the displayed route/budget. A user
confirmation covers that selected scope; do not add a dialog for every correction.
Old format-only confirmations do not enable proofreading. This spec is not a real
chapter selection or authorization for an agent to run private/paid acceptance.

Use the existing durable job vocabulary and request-id replay rules. New jobs add
`operation: repair|chapter_translation`; old jobs without it mean repair. Keep the
item-wide active-job exclusion across repair and translation; chapter-edit commit
checks that exclusion too. Staged uploads do not reserve an active-job slot, so an
abandoned editor cannot block the item forever. Concurrent edit uploads may proceed;
only one changed-revision commit wins CAS. No lock is held during upload/model I/O.
An identical committed request returns its original receipt after current access
checks, even if its old revision/quote has since expired; it never reapplies data.
Same ID with different payload rejects. Failed/unknown work never automatically
resends or silently obtains a new request ID. Historical jobs remain inspectable
under current access but do not authorize reading stale generated text as current.

New-job receipts retain safe `failure_stage` and `failure_reason` enums, nullable
on old receipts. Keep stage=done at terminal for compatibility. Record per-dispatch
nullable input/output/total usage; aggregate input/output totals are null when any
component is missing, not invented from total usage. Existing reported_total_tokens
continues to sum reported totals; unknown_usage_requests and failure costs remain.
No raw provider response, excerpt, title or credential in diagnostic logs.

### Matching reply envelope normalization

Approve a new versioned parser policy for reader repair matching, reusing the
bounded JSON-envelope behavior from content-format normalization. This is not a
diagnosis of the historical matching failure: its original response was not saved,
so fences/extra keys are unproven causes. No real retry/model charge is authorized
by this parser change; a new attempt still requires a new explicit user request.

Accept bare JSON or exactly one whole untagged/json Markdown fence with surrounding
whitespace. Reject explanatory text, multiple JSON values/objects, multiple/nested
fences, unsupported fence tags and truncation. Parse the complete JSON, detecting
duplicate decoded keys at EVERY depth, including inert extra-field subtrees, before
discarding unknown fields. Extra fields cannot supply IDs, defaults, authority,
relations or publication permissions. Known fields remain required and strictly
typed: missing/null/wrong-type groups/o/t/relation reject the whole response; no
string-to-number coercion or unknown-enum fallback. Do not alter the matching
group budget, adjacent-block/kind/monotonicity checks or Store partition proofs.

Retain existing semantic-group rejection behavior, distinct from JSON/schema
failure. A well-typed but semantically invalid group consumes no IDs; other valid
claims may still produce partial results, with rejected claims/gaps unresolved.
Do not copy content-format's all-edits-or-nothing rule onto this different contract.
All rejected/no-usable-match outcomes keep their existing rejection and cannot be
reported aligned; an empty groups array retains its existing none/unresolved
semantics rather than manufacturing a match. Test both schema-invalid mixed groups
(whole-response rejection) and semantic-invalid mixed groups (existing filtering).

Unknown fields remain subject to allocation limits before being discarded:
raw UTF-8 reply at most 16MiB, each array/object at most 65,536 members, total at
most 1,048,576 JSON Value nodes, with existing serde depth and any stricter transport/
profile limits retained. Exact limits are inclusive. Never raise provider output
budgets for tolerance. Use typed budget failure state, not error-text matching.
New fixed failure_reason subcodes are alignment_output_json (envelope, syntax,
duplicates), alignment_output_schema (known-field shape/types/enums), and
alignment_output_size (raw/collection/node budget). Preserve public error_code,
failure_stage vocabulary, existing semantic rejection codes and observed usage;
do not log provider prose, raw parser messages, titles or credentials.

Bind parser policy with prompt/plan/request identity at admission and before any
dispatch/interpretation. Absence means the legacy policy, never current by default.
Old queued/pending jobs cannot silently acquire the new policy or spend new calls;
fail/pause them under a fixed stale-policy reason without dispatch. Failed/unknown
jobs remain inspectable with their original usage, not automatically retried.
Reconcile existing durable receipts and accepted committing plans according to
their frozen identity and existing recovery contract without model calls or new
interpretation; never turn receipt recovery into permission to dispatch. Exact
request replay preserves the old job/receipt instead of creating a current-policy
job. Any new user retry gets a new explicit identity, not an automatic retry ID.

The shared matching helper is also used by synthetic batch/map work: freeze/check
the policy in those callers' persisted plan/quote/job identities too, not just by
passing today's constant when reading an old job. Those new production capabilities
remain HOLD. Reusing the strict envelope helper must preserve content-format's
existing null/binding/duplicate/size behavior and tests, not widen its semantics.
Synthetic verification covers fences/text/truncation, nested escaped duplicate
keys and extras, null/wrong types/enums, exact/over budgets, mixed/all-invalid
groups and Store proofs, plus actual persisted legacy jobs across reopen/step/
replay/recovery with zero new dispatch and unchanged usage. Parser unit tests alone
do not establish that old jobs cannot run after daemon restart.

Frozen JSON keys (additive; no historical receipt rewrite):

```json
{
  "operation":"repair",
  "failure_stage":null,
  "failure_reason":null,
  "usage":{
    "requests_dispatched":2,
    "reported_total_tokens":50,
    "unknown_usage_requests":0,
    "dispatches":[
      {"input_tokens":null,"output_tokens":7,"total_tokens":37},
      {"input_tokens":11,"output_tokens":2,"total_tokens":13}
    ],
    "input_tokens":null,
    "output_tokens":9
  }
}
```

`operation`, `failure_stage` and `failure_reason` are job-level fields. Missing old
operation means repair; missing failure fields mean null, not an inferred cause.
Failure fields are null when inapplicable, including a partial success with rejected
claims; internal diagnostics may preserve that rejection without calling success
a failed job. Terminal stage remains done, not replaced with failure_stage.

`usage.dispatches` is an ordered array of this job's dispatch attempts, one entry
per requests_dispatched, including failed/unknown attempts. All three entry keys
are required on new entries, each a nonnegative safe integer or null. Preserve an
all-null entry for unknown usage; do not omit it or convert it to zeros. Update the
same entry when its response/reconciliation becomes known; replay/cache reads never
append entries. Old failed jobs and their costs remain separate on explicit retry.

`usage.input_tokens` and `usage.output_tokens` are independently summed ONLY when
that field is known for every dispatch. A missing component makes only its own sum
null: the example's input sum is unknown but output sum is 9. Do not calculate input
as total minus output, or total as input plus output. reported_total_tokens still
sums provider-reported total_tokens only; unknown_usage_requests still counts
dispatches without known total usage, so known totals with unknown splits do not
increase it. These fields report tokens, never a monetary charge.

A new job with verified zero dispatches has dispatches=[], input_tokens=0 and
output_tokens=0. An old record without per-dispatch evidence instead reads as
dispatches=null, input_tokens=null, output_tokens=null; older wire responses may
omit these new fields and clients normalize them to null. Do not fabricate entries
from old aggregate totals, even if only one old dispatch was recorded. Preserve
all three existing aggregate fields unchanged for those historical jobs.

#### Proofreading: constrained edits, not a rewritten chapter

The later "Unified content formatting v1" section supersedes this section as the
new user-facing entry point (human #3796). This narrower compatibility contract
does not gain garbage deletion or batch powers. Implementing a second proofreading
UI/capability is not required; absent reader_chapter_proofread_v1 still rejects true.

`align_request` gains optional `proofread: boolean`, default false. This flag is
part of request identity and requires reader_chapter_proofread_v1 when true. The
explicitly confirmed true request permits conservative reading-text corrections
in the selected original chapter and its existing counterpart only. It does not
authorize translation, prose polishing, modernization, factual correction, new
sentences, deletions of notes, or changes to another chapter.

Allowed kinds: `typo`, `extraction_garble`, `punctuation`. Short replacements must
stay inside one existing paragraph/heading/note block, without changing that kind,
links, code, note markers or chapter boundaries. At most 64 non-overlapping edits
per job; each replaces a nonempty source range of at most 64 UTF-8 bytes with at
most 64 bytes, without newlines/NUL. This is a bounded first release, not permission
to split a sentence rewrite into many adjacent corrections. Larger corruption,
uncertain wording and missing source text remain unresolved.

Each correction has a stable id, side, chapter, old/new ranges and span hashes,
both chapter hashes, input/result revision, kind and evidence basis:

- `source_verified`: immutable input Evidence plus a resolvable source locator
  supports the replacement; the backend validates that link, not a model assertion.
- `context_inferred`: a bounded typo hypothesis, explicitly labelled AI-inferred,
  not verified original wording. Do not infer changes to names, numbers, dates,
  negations, quotations' meaning or author/translator style from plausibility.
  Ambiguity stays unresolved; a model confidence score alone is not certification.

The private revision proof stores exact before/after strings and supporting ranges,
bound to the selected immutable source. Context-inferred evidence identifies the
local context actually considered and model/prompt identity; it must not pretend
to be a source-page verification. Original PDFs/Evidence remain immutable; corrected
reading text gets new revision/Evidence, with source-vs-correction provenance kept.
The user can inspect each applied correction without reopening or resending a job.

Budget: reuse the frozen saved Low dial profile and existing max_requests=2 for
formatting/matching/proofreading COMBINED, not two calls for each task. Both prompts,
input text and completion must fit the saved limits. No automatic model upgrade,
profile expansion, whole-book scan or retry. Track exactly which ranges on each
side were actually checked; sampled coverage cannot be called full-chapter proof.

`align_status`/admission job adds nullable `proofreading` (null for old jobs/false):

```json
{"status":"partial","applied_count":2,"unresolved_count":1,
 "checked_bytes":{"original":400,"translation":300},
 "total_bytes":{"original":800,"translation":700},
 "changes_version":"64-hex","quality":"not_certified"}
```

Status: pending|complete|partial|failed|outcome_unknown. Counts/version are null
before commit; a definite failure with no commit has applied_count=0, not a success
message. Unknown commit outcome retains null until reconciled. complete means all
available selected-side ranges checked with no unresolved candidates, NOT guaranteed
error-free text. partial is a valid committed bounded result; count only committed
edits, excluding suggestions and whitespace changes. Report alignment_status and
proofreading.status independently. "已校对 n 处" means applied_count only; partial
must additionally show that some content was not checked or remains uncertain.
checked_bytes counts the union of checked input ranges, never duplicate prompt
context; absent translation uses null for its byte counts. unresolved_count counts
identified unresolved candidates, not unknown errors in unchecked text. A complete
zero-edit result still has a changes_version for its empty, hash-bound receipt.

`proofread_changes` (read scope) params:
`{item_id,job_id,expected_version,offset,limit}`; expected_version is the returned
changes_version, offset counts edits, limit=1..64. Result:
`{item_id,job_id,input_revision_id,result_revision_id,version,total,offset,next_offset,changes}`.
Each change is `{id,side,chapter,kind,basis,old_range,new_range,before,after,
old_sha256,new_sha256,source_evidence_ids}`. Text is returned only through this
authorized UI read, never copied to diagnostics/public proof. Navigation uses its
result revision; a later revision disables stale jumps instead of guessing offsets.

Prepare corrections, final bodies, proofs and matching index together. Validate the
complete output byte accounting and updated UTF-8 ranges before one CAS/journal
commit. A malformed response, permission failure, stale CAS or unknown dispatch
must not publish a half-corrected chapter. Budget-bounded/uncertain but valid output
may commit as partial; network failure is not relabelled partial to conceal it.
No text change creates no new content revision. Index/receipt changes alone may
still be saved. Legacy whitespace plans never acquire these additional powers.

#### Missing English chapter translation: durable AI projection

Eligibility is checked on the server: current original edition language is en
(including en-*), selected original chapter exists and counterpart is null.
Existing counterpart means `reader_translation_not_needed`, with zero dispatch,
even if that counterpart is short/empty-looking. Never call legacy whole-item
translation_request: it has different scope and may schedule other purposes.

Use saved Purpose::Translation, not Low dial. The user's existing preference is
Astra/low, but resolve the current saved route and expose it; do not hardcode or
change it. No implicit glossary, title, summary, classification or alignment calls.
Source supplied to the provider is limited to this chapter, including its notes.

`translate_preview` (read scope) params: `{item_id,revision_id,chapter}`.
It performs no inference, network credential probe or content write. For an eligible
uncached chapter only, the daemon may use the existing database/provider-scoped,
noninteractive local Credentials::status to bind the actual credential generation.
That implementation reads/parses a Keychain record containing the key internally;
this is NOT a promise of zero secret access. It must not call resolve, make a
provider/models/auth request, export a key or masked_key, log the status object,
modify credentials, prompt to unlock Keychain, or read another database's records.
Only the required non-secret state/generation enters quote planning; generation is
not a substitute for provider readiness. Missing, inaccessible, corrupt or
scope-mismatched credentials yield no usable quote and a sanitized unavailable
reason; never fabricate generation=0 or substitute settings_revision. Ineligible
chapters and completed-result reuse do not need a credential lookup. No generation
mirror/migration is required by this version. Return
`{eligible,reason,quote,translation}`. translation is null or the current completed
projection metadata below; an existing result makes quote=null and avoids spending.
Ineligible results also have quote=null. Otherwise quote contains:
`{quote_id,expires_at,item_id,revision_id,chapter,source_sha256,target_language,
settings_revision,route,budget,source_bytes,planned_requests,estimated_cost}`.
`route` is `{provider_id,model_id,reasoning_effort}`: provider configuration ID,
exact upstream model ID and nullable configured effort, no secret. expires_at is
Unix epoch milliseconds; target_language=zh-CN. estimated_cost is null unless
supported by a known price, then `{amount,currency,kind:"estimate"}` (decimal string,
ISO currency code). Token counts or Low must not be presented as a currency price.

The backend plans complete block batches and binds them/profile/prompt/settings/
credential generation to quote_id. Quote expires after 15 minutes. Per request
input including prompts is at most min(adapter limit,32768 bytes), completion at
most min(adapter limit,8192 tokens); at most 8 planned requests per chapter job, plus at
most 2 automatic retries (see below).
`budget` is `{max_requests,max_input_bytes_per_request,max_completion_tokens_per_request,
max_total_input_bytes,max_total_completion_tokens}`. max_requests=planned_requests
counts the actual bounded plan (1..8), not always 8; the two total ceilings are the
respective per-request ceiling times max_requests. All auxiliary dispatches count
(none are required); malformed-output splitting cannot exceed the confirmed plan.
If the complete chapter cannot fit, preview returns eligible=false with
reader_translation_budget_exceeded before billing; never silently omit its tail.
UI shows route and planned/max usage at the existing cost confirmation. All budgets
are ceilings, not expected consumption or an authorization granted by this spec.

`translate_request` (edit scope) exact params:
`{item_id,revision_id,chapter,request_id,quote_id,accept_cost:true,expected_translation_version:"missing"}`.
One step (2026-11 amendment, capability `one_step_model_jobs_v1`): `quote_id` and
`accept_cost` may be omitted; the server then issues the quote itself (same planning,
budget and source/settings/credential checks) and admits it in the same call. Old
clients that send `quote_id`+`accept_cost:true` are unchanged; `accept_cost` is no longer
a gate. An invalid reply or uncertain dispatch is retried automatically at most 2 times
with backoff, resuming after the validated batches; then the job ends failed/unknown.
New jobs require matching unexpired quote and unchanged source/settings/credential
generation; drift rejects without dispatch. Recheck permission, no counterpart and
source hash at dispatch and commit. Reusing an available complete projection returns
a succeeded/reused receipt without a new call, even under a new user request ID;
there is no force-regenerate option in this version. A fresh explicit retry after
failure may reuse validated completed batches with the same source/route/prompt;
beyond the 2 bounded automatic retries, unknown/incomplete dispatches are not resent.
Preserve their old costs.

`translate_status` (read scope): `{item_id,job_id}`. Response is `{job}` with the
common job fields, operation=chapter_translation, same state/error/budget/usage
semantics, and nullable `translation`. Alignment fields are null/not applicable.
Stages are queued|translating|committing|done. Expose completed_blocks/total_blocks
without leaking text. Publish only a full validated chapter, not a truncated success.
On failure/unknown, retain safe completed batches privately for explicit recovery;
no partial result is advertised as the complete translated chapter.

The result is a durable, versioned Analysis/projection with immutable output and
source provenance, NOT a transient cache, new Item, or chapter inserted into the
human translation edition. It leaves item revision, original and human edition
bytes, author/translator metadata and counterpart=null unchanged. Label it AI译文
with model/prompt provenance, never credit it to the human translator. Projection
metadata: `{version,item_id,revision_id,original_chapter,source_sha256,
target_language:"zh-CN",sha256,bytes,provenance:"machine_translation",route}`.
Canonical/human-translation search is unchanged by this projection in v1.

For a directly stored current projection, `metadata.version` is the server-computed SHA256 of its serialized complete stored
projection (schema version, source identity, provenance including route/prompt/plan,
body digest and Markdown), not the SHA256 of body text or of the public metadata
object. Clients treat it as opaque and echo the returned value; do not reconstruct
it from the public fields. `metadata.sha256` separately hashes the exact complete
generated UTF-8 body, and `metadata.bytes` is that body's byte count.
For an inherited unchanged-chapter view, version and revision_id follow the binding
rule above; the original stored projection and model provenance remain unchanged.

##### Cross-device completed-translation discovery

Source: frontend human #3994 / M-034rnDPNvtjB44ocioMBqG,
2026-11-04T14:37:48.593Z: a Front matter translation completed on Mac was not
visible when opening the same chapter on the phone. The required behavior is to
discover and read an existing server projection, NOT automatically translate again.

Add chapter-scoped `translate_metadata`, not per-chapter fields in the full TOC.
Gateway method `memex.library.reader.translate_metadata` requires read scope;
owner UDS uses the ordinary reader envelope:

```json
{"command":"reader","max_sensitivity":"private","request":{
  "op":"translate_metadata","item_id":"item_fixture",
  "revision_id":"revision_fixture","chapter":"front-matter"
}}
```

Gateway params are exactly `{item_id,revision_id,chapter}`. Return the ordinary
reader payload `{item_id,revision_id,chapter,translation}`; translation is null
or the complete existing projection Metadata defined above, with its opaque
version, current revision binding, body SHA256/bytes and original route/provenance.
No quote, job, eligible flag, readiness claim or body text is returned. Null means
no validated completed projection is available for this exact current chapter,
not a failed job and not authorization to generate one. In-flight/partial worker
output is not a completed projection. Use the existing source validation/errors
for nonexistent chapters, unsupported language or an existing human counterpart;
do not turn invalid or unauthorized requests into a successful null result.

Implement this as the same permission-checked Store effective-projection metadata
read used by translate_chunk, independent of preview/job planning. It must perform
ZERO Credentials::status/resolve/Keychain calls, provider/network requests, quote
creation, job creation, usage changes or projection writes, for BOTH hits and misses.
No saved provider configuration is needed to read a previously completed result;
missing/rotated credentials, model readiness and changed current settings do not
invalidate that result. Retain and label its actual historical model provenance.
Normal Store snapshot/integrity recovery guarantees are not weakened by this read.

Enforce the exact current item/revision/chapter and current plus historical
revision/Evidence permissions. Reuse the verified unchanged-chapter inheritance
resolver and domain-separated view version defined above (version encoding
originally frozen as f8d0ecdf); current corrupt/denied artifacts must not fall back
or become null. Per-step chapter-change and unsupported-transition barriers remain;
equal final body hashes alone never grant reuse across revisions/items.
Do not expose filesystem paths, credentials, provider diagnostics or chapter text
through discovery errors or logs.

Advertise reader_translation_metadata only when this route AND matching
translate_chunk reads actually work through the installed daemon/gateway. Missing
capability means unavailable, not proof that the chapter lacks a translation.
It is independent of provider readiness and permission to start a new translation.
Opening an eligible chapter on either device, or explicitly refreshing/reopening
it, uses this read without needing a remembered version or another confirmation.
If translation is present, fetch translate_chunk with its returned version,
validate the complete UTF-8 body SHA256/byte count, and show it as AI译文. Hide the
new-translation action for that completed result. Do not use translate_preview as
an automatic fallback: on a miss it can probe credentials and create a quote.
On an older server retain only the existing explicit-click preview behavior.

On stale_item_revision/stale_reader_translation, discard only the stale projection
read state, refresh current chapter identity/metadata and retry reads within the
existing bounded refresh policy; preserve conversation/editor drafts and selected
chapter. Permission failure must not display stale cached private text. Discovery
errors must not be relabelled missing and trigger a new translation request.
This is on-open/read refresh, not a claim of background push synchronization.
If another device completes after an earlier null result, reopening/refreshing
must discover it; do not persist a permanent negative cache keyed only by revision.

Synthetic acceptance: client A completes translation, fresh client B without local
version/job/quote state discovers and reads it; no new quote/job/provider request
and usage unchanged. Both hit and miss succeed with credential access wired to
fail if called, including absent/changed provider settings. Test title-only
and unchanged-chapter inheritance, stale-version refresh, per-step chapter-change
and unsupported-transition barriers, corrupt artifacts,
unauthorized access, missing capability, null followed by later publication at the
same revision, and retention of drafts/selected chapter. No real-library/model call
is authorized by this contract freeze; actual fixture wiring and release remain
separate verification/delivery steps.

##### Recorded AI translation block correspondence

Additional human source: frontend #4000 / M-034rnI9vSxEVNDuni1djJX,
2026-11-04T14:40:55.833Z: the existing translation appeared, but unlike other
chapters it lacked the Chinese/English alignment controls. Add paragraph-style
interleaving and matched-block parallel scrolling for AI projections without an
additional inference request. This is block-generation correspondence, NOT a
second model's semantic alignment or certified translation accuracy.

For newly published translations, derive the map from the frozen plan's ordered
source blocks and their validated, ID-matched complete outputs. Publish body and
map together in the immutable projection; do not expose a complete new-format
projection with a half-installed map. Each pair uses the existing row shape
`{o:[start,end],t:[start,end],kind,relation:"matched"}`. o is relative to the original
chapter, t to the generated projection body, never to a human translation edition.
kind is the source planner block kind paragraph|heading|note. One generated block
may contain several rendered paragraphs: keep its actual range as one row, do not
invent subparagraph pairs or claim the kind certifies generated Markdown structure.

Require exactly one output per planned block ID in the original order and complete
coverage of all planned batches. Both ranges are nonempty, half-open UTF-8 byte
intervals, monotone and disjoint, partitioning their respective complete bodies
exactly once. Original separators remain attached as in the frozen plan. During
assembly, assign each generated joining separator to the preceding output range
(no separator after the last block); do not change the existing generated bytes
merely to fit a map. Source ranges/kinds must agree with the validated source plan,
and generated ranges must agree with actual output concatenation and full hash.
Invalid/missing/duplicate/out-of-order blocks or ranges reject new publication,
never trigger extra inference or publish a partial correspondence as complete.

The stored projection schema must distinguish mapped projections from legacy ones.
Its complete serialized version binds the map as well as identity/provenance/body;
unchanged-chapter inherited views then use the same source-artifact/current-revision
binding defined above. Preserve legacy complete-artifact version computation and
deserialization: adding an absent optional field must not silently change a legacy
version. Do not modify existing immutable projections, historical jobs or receipts
to retrofit a map. Legacy translations remain readable and discoverable; if no
recorded map exists, return missing. This increment does not reconstruct legacy
maps from body splitting, length ratios, or job progress counters, or automatically
retranslate them. Thus an already-created Front matter may remain readable but
without synchronized paragraph controls; do not claim this limitation is solved.

New read-scope gateway method `memex.library.reader.translate_alignment`, owner
UDS reader op=translate_alignment. Params:
`{item_id,revision_id,chapter,expected_version,offset,limit}`,
expected_version is the exact value from translate_metadata.translation.version,
not a body hash and not "any"/"missing"; offset is a row offset, limit=1..256.
Return `{item_id,revision_id,version,original_chapter,translation_chapter,
original_sha256,translation_sha256,original_bytes,translation_bytes,status,
quality,basis,offset,total,next_offset,pairs}`. version is that same effective
projection version; translation_chapter=null (there is no human counterpart);
original hashes/bytes describe the complete source chapter and translation hashes/
bytes the complete AI body. quality="not_certified", basis="translation_blocks".
status=aligned for a complete recorded map. Legacy absence returns status=missing,
total=0, pairs=[], next_offset=null and requires offset=0, while retaining the real
projection version/hashes/byte counts. Missing projection itself is the existing
reader_translation_unavailable error, not an empty alignment for fabricated text.
Malformed recorded maps reject rather than becoming missing. Use existing bounded
offset/limit validation and stale_reader_translation for a projection-version race.

Discovery, mapping and chunk reads share effective projection resolution, exact
current identity and current/historical protection. They require no configuration,
credentials, quotes, jobs, billing or model request and perform no lazy map writes.
The new capability means the mapping read is implemented; an individual legacy
projection can still report missing. Neither the capability nor a translation
success means every historical projection has a map. Keep translate_status's
human-edition alignment fields null; do not route an AI projection through the
existing human-counterpart align_request or change counterpart=null.

Clients use the recorded pairs for interleaving and row-boundary scroll sync,
with original reader.chunk bytes and generated translate_chunk bytes. They must
not reuse human-translation offsets, proportional scrolling or treat AI bytes as
original citation coordinates. With missing mapping keep the AI text readable,
explain the absent correspondence and do not show enabled synchronized controls.
When mapping capability is absent, keep the old readable AI view. A version race
refreshes metadata/map/body coherently within the bounded read policy and preserves
drafts; it never calls preview/request automatically or substitutes another chapter.

Synthetic acceptance adds unequal UTF-8 source/output sizes, multiple batches,
headings/notes, generated internal blank lines, separator accounting, full byte
coverage and map paging; malformed map publication/read rejection; legacy missing
with unchanged body/version; inherited title-change map version and stale rejection;
two fresh clients rendering the same recorded correspondence with no extra dispatch.
End-to-end interleaved/parallel UI must be inspected on both clients. Legacy missing
and new mapped success are separate acceptance cases, neither real paid requests
nor retroactive claims about the user's already-generated translation.

`translate_chunk` (read scope) params:
`{item_id,revision_id,chapter,expected_version,offset,limit}`.
expected_version must equal the returned `metadata.version`; a body sha256 is not
an accepted substitute. Mismatch returns stale_reader_translation. Equal body
bytes with different source/provenance still bind distinct projection versions.
offset is a chapter-relative byte offset, limit=4..32768. Result includes projection metadata plus
`{start,end,markdown,next_offset}`; ranges are relative to generated text, UTF-8
safe and hash-verifiable. Reading/switching modes never starts work. Display beside
original as generated translation; recorded block correspondence is available only
through translate_alignment above, never fabricated source citation coordinates.
An original-side revision change makes old output stale;
the sole reuse exception is the fully validated unchanged-chapter inheritance above.
Otherwise retain it for history but do not silently reuse/retranslate under the new revision.
The UI hides the translation action when a counterpart or completed AI result exists.

#### Owner Markdown edits: one chapter, one real edition

Enable for side=original|translation and an actual chapter in that side. Generated
AI projections are not editable through this operation. The editor uses chapter
bytes from chunk, not the entire canonical item. It may change prose as an explicit
owner edit, not as an AI/source-verified correction. No implicit model request.
Keep item title, authorship, chapter key/title/level/order and counterpart metadata;
Markdown headings inside the edited body do not redefine manifest chapter identity.

Use bounded staging on BOTH UDS and gateway rather than enlarging encrypted frames:

- `edit_begin` (edit scope): `{item_id,revision_id,side,chapter,expected_chapter_sha256,
  request_id,bytes,sha256}`. bytes/sha256 describe the complete proposed UTF-8 chapter.
  Return `{edit_id,received_bytes,max_chunk_bytes:8192,state,receipt}`. State is
  uploading|committed; receipt=null until committed. Exact begin replay resumes the
  same staging identity or returns its committed receipt; payload conflict rejects.
- `edit_chunk` (edit scope): `{item_id,edit_id,offset,markdown}`. At most 8192 UTF-8
  bytes, sequential whole-codepoint chunks. Return `{edit_id,received_bytes}`.
  Identical already-acknowledged range replay succeeds; differing overlap/gap fails.
- `edit_commit` (edit scope): `{item_id,edit_id}`. Verify length/full hash/UTF-8,
  then current item revision and selected chapter hash under the writer lock.
  Return `{item_id,request_id,revision_id,side,chapter,chapter_sha256,changed}`.

Stage is durably bound to requester/authorized resource, not a client filesystem
path. A restarted/expired session cannot access another principal's upload. Each
step is a new admission under the Common rules: successful edit_begin/edit_chunk
does not authorize a future edit_commit after session expiry/revocation. An already
dispatched edit_commit is not cancelled by session loss while awaiting the writer
lock. Fresh authorization for the same principal/resource may resume the stage or
recover the original receipt; possession of edit_id alone confers no authority.
No long-lived writer lock while uploading. Bound total
chapter input to 16MiB (existing editor limit); enforce resulting edition/canonical
limits before committing. Reject empty/whitespace-only or NUL content, as with the
current Markdown editor. Respect existing frame/deadline/concurrency limits. Old
staging is uncommitted user input, not automatically disposable Library garbage.

Commit only this chapter through new owner-edit activity with immutable source,
new edition/manifest Evidence and recovery checks. Preserve the other side and all
unselected chapter bytes/identities; recompute subsequent offsets. Editing the
translation updates canonical accordingly; editing original leaves canonical text
unchanged but still creates a new item revision. An identical body is a no-op and
returns changed=false/current revision. Journal recovery and replay must return the
same receipt after a lost response, never another revision.

Changed content invalidates affected paragraph correspondence (read returns missing
until explicitly rebuilt). No automatic repair/translation fee; old annotations and
generated translations remain explicitly revision-bound/stale. Do not wipe or
silently save unrelated title/AI/editor drafts. On stale_item_revision preserve the
draft and offer reload/compare, never overwrite current content or silently rebase.

New sanitized errors: invalid_reader_proofread_request, invalid_reader_correction,
stale_reader_corrections, invalid_reader_translation_request,
reader_translation_not_needed, reader_translation_language_unsupported,
reader_translation_budget_exceeded, reader_translation_quote_stale,
reader_translation_unavailable, stale_reader_translation, no_translation_provider,
invalid_reader_edit, reader_edit_not_found, reader_edit_too_large,
reader_edit_upload_conflict. Retain existing identity/CAS/permission errors. No
fallback to legacy whole-item edits or translation when a new capability is absent.
A chapter translation job that fails with the generic error_code
reader_translation_unavailable keeps that code for existing clients; NEW failures add a
typed job.failure_reason from a fixed set: reader_translation_invalid_output (the
response failed structural validation), reader_translation_empty_source, or
provider_failure (a known provider rejection). Unknown sites keep failure_reason equal to
error_code. Never response excerpts or provider prose. Prompt version
reader-chapter-translation-v2 changes the plan identity, so a job admitted under v1
fails stale_reader_translation without dispatch; published translations stay readable.

Acceptance uses synthetic fixtures: old clients cannot activate proofreading;
proofread applied counts/partial coverage/no-change, forged source evidence and
semantic rewrite rejection; no paid action on paired/non-English/stale input;
translation preview drift, same-ID replay, completed-cache reuse, unknown no resend,
complete-output publication and correct AI attribution; side-specific edits across
chunk/restart/CAS/journal interruption boundaries, preserved unrelated chapters,
stale alignment, and title/AI/editor draft protection. Text-bearing private proofs
never appear in public screenshots or diagnostic logs.

Authorization acceptance must distinguish pre-dispatch revoke/expiry (zero owner
command), revoke/expiry between upload and commit (commit denied), and a dispatched
commit held behind a deterministic lock barrier when the session is revoked or
expires (eligible write may finish, no result leaves the old channel). Also cover
resource permission/sensitivity/trash changes before mutation and receipt replay,
fresh-authorized same-identity recovery without a second revision, and CAS conflict.
An existing post-commit revocation test alone does not prove the lock-wait case.
Translation preview tests use synthetic credential state: no provider dispatch or
resolve, no masked-key/secret output, no lookup for ineligible/cached results, failure
closed on unavailable credentials, and generation drift rejecting the later request.

Multiple open tabs/preview-tab pinning are client session state, not a new database
object or an authorization for cross-device tab synchronization. Notes typography,
continuous next-chapter reading and version-page image caching remain frontend work;
prefetch is bounded/read-only and never dispatches repair or translation.

### Unified content formatting v1 — selected text and directory batches

Source: frontend human message #3796 / M-034rdKyEehaedgA6yFjuV9,
2026-11-04T07:54:50.067Z: a standalone Chinese/English format action for imported
Zotero/Evernote-like bodies, removal of unrelated captured-page garbage, repaired
CNKI abstract paragraphing, Low dial processing and left-directory batch access.
The source names one-click processing, not unattended library-wide inference.
Per-change review, immutable history, budgets and recovery below are engineering
safeguards, not quotations of additional human choices. No private screenshot text
is a fixture, special case, model input or diagnostic artifact for implementation.

#### Scope, compatibility and capabilities

Use one visible action, 格式整理, for ordinary canonical Markdown bodies (including
web articles, notes and imported abstracts) and a selected real book chapter/side.
A source abstract stored as the item's body is eligible; this does not rewrite
metadata abstracts or generated summaries. For books the UI names the selected
original/translation side; it never silently includes its counterpart or other
chapters. Directory batches do NOT expand books into whole-book jobs: report those
items as skipped/chapter_required. Chinese, English and mixed text are supported;
preserve their language, meaning and author style, with no translation/alignment.

New daemon features: reader_content_format_v1 -> session.info.reader_format;
reader_content_format_batch_v1 -> session.info.reader_format_batch. Values remain
available/unavailable. Core availability requires single-target execution, changes
readback and restore; batch availability additionally requires directory snapshot,
durable per-target results and resume. Absent fields mean unavailable. No fallback
to align_request, whole-item translation or a model call from a read operation.
Group selection requires both format capabilities; writes also require edit scope.
The revision-text capability below is required for the new before/after UI; none
of these capabilities implies permission or readiness for irreversible reclamation.

Reuse shared job/usage, provenance and writer-journal machinery, not a second model
configuration or a second mutable body authority. The old whitespace Edit stays
strict. If legacy proofread:true is offered, retain its narrower limits and its
own capability; new format powers never flow through that flag. Existing repair,
translation and edit contracts remain unchanged. Formatting a book invalidates
affected correspondence and generated projections; it does not rebuild them or
charge for matching. Keep drafts and revision-bound citations honest.

#### Allowed edits and reviewable evidence

Read only the selected stored body and necessary immutable local source/layout
evidence, not the live URL, other items or external account/browser state. Do
deterministic parsing/normalization first; use saved Low dial for ambiguous boundary,
garbage-role and short correction decisions. Imported text is untrusted data, never
instructions to the model or permission to invoke tools. Do not send document
title/URL/account metadata as incidental prompt context. Recognizable secret spans
must be withheld from provider input; retain opaque source-range placeholders so
no secret text is copied into prompts, diagnostics or public proof. This is not a
claim of exhaustive secret detection or consent to retrieve credentials.

Edit kinds are whitespace, paragraph_boundary, garbage_delete, typo,
extraction_garble, punctuation. Retain original bytes outside explicit edits.
Paragraph split/join/indent edits conserve non-whitespace text; no guessed section
titles, summaries or factual rewrites. Typo/garble/punctuation retain the earlier
64-byte span/replacement constraints and source_verified/context_inferred basis;
do not split a prohibited rewrite into adjacent edits. At most 64 such corrections
per target. Larger damage stays unresolved, not reconstructed from plausibility.

garbage_delete removes only complete identified page-furniture blocks, such as
navigation, advertisements and unrelated saved-page controls/account widgets.
A model selects bounded source block IDs and a fixed reason, never arbitrary
replacement prose. Support requires a structural/source anchor or an explicitly
labelled context-inferred classification; model confidence is not source proof.
Do not drop notes, citations, tables, author/byline/source attribution, quotations,
code, an isolated number/year or a section merely because it appears unimportant.
Legitimate discussion of ads/navigation remains content. Ambiguous blocks stay
unchanged with an unresolved reason. Entirely empty output or loss of all identified
body content rejects. There is no general deletion privilege for retained prose.

Each applied edit records stable id, kind, basis, reason_code, old/new UTF-8 ranges
and hashes, input/result revision, target hashes, source Evidence links and the
model/profile/prompt identity where relevant. Deletion has an empty new range;
insertion has an empty old range. Edits must not overlap or split codepoints; the
complete edit map accounts for every output byte. Store exact before/after text in
private immutable change evidence, not logs. The authorized UI can inspect every
change after application without a second model request. No per-edit confirmation
is required: one explicit target/budget/data-use confirmation permits validated
changes to apply together, followed by review/restore. Invalid output or failed
model calls do not publish a half-edited target; independent targets remain valid.

#### Model reply normalization and bounded rejection diagnostics

This tolerance applies ONLY to untrusted provider replies inside the format planner,
not owner IPC/gateway params, stored proofs or Store validation. Keep the exact
server-owned source/window binding, complete-window coverage and atomic target
publication. A model echo is not an authority or integrity proof.

- The model need not echo source_sha256 or window_id. Bind an absent field from
  the exact frozen plan/window of the completed dispatch, never from response order
  alone, model text, a current mutable source or another job. If a legacy reply
  supplies either field, it must have the correct type and exact expected value;
  reject a mismatch rather than overwriting it. Late/duplicate/wrong-window replies
  cannot be relabelled as the next window. Final source/CAS and per-dispatch guards
  remain unchanged.
- Derive unresolved_ids from the complete validated classifications in input order.
  Classifications still cover every visible range exactly once with known roles;
  missing, duplicate, foreign or protected range IDs reject. An omitted redundant
  unresolved_ids is allowed; if supplied, it must exactly match the derived list.
  Do not infer successful examination or body classification from missing entries.
- Accept bare JSON or one whole-response Markdown fence with no language tag or
  the exact tag json, allowing surrounding whitespace only. The enclosed content
  must be one complete JSON object. Do not extract a plausible JSON substring from
  commentary, multiple objects/fences, truncated output or trailing non-whitespace.
  Unused extra object fields may be ignored, but never interpreted as authority,
  source_verified, Evidence links, complete replacement text, usage or budget.
  Known fields retain their types and enum validation; duplicate JSON object keys
  reject, including nested ones. Unknown enum values are not ignorable fields.
  Apply existing raw reply size, nesting and collection bounds before discarding
  extras; normalization must not bypass input/completion limits or add model calls.
  Bound each raw reply to 16 MiB, each array to 65,536 elements and each object to
  65,536 members, with at most 1,048,576 Value nodes across the entire reply (root,
  containers and scalar values included; object keys remain bounded by raw bytes
  and member counts). Limits are inclusive; all ignored extras consume the same
  budgets. Enforce limits while decoding/collecting, not after allocating the full
  tree; retain the existing serde nesting limit. These are parser resource ceilings,
  not a process RSS guarantee, expanded model budget or permission to truncate arrays.
  Raw-byte, collection and total-node exhaustion use format_output_size via typed
  parser state, never by matching raw exception text. Invalid JSON/duplicate keys
  remain format_output_json. Test both sides of each boundary using synthetic data.
- Do NOT drop an invalid edit and apply the rest. All claimed edits, classifications
  and all window replies must validate before one combined Store apply/publication.
  Keep overlap/order, UTF-8, range, protected-source, correction-count and semantic
  checks, including checks across windows. A rejected edit is not a no-op or an
  examined unchanged range; do not hide it in a successful complete/partial result.
  On failure publish no changed revision/proof for this target, preserve observed
  usage and retain normal failed/unknown distinction. Independent targets may finish.

Keep error_code and the failed target's reason as content_format_invalid_result
for invalid model output. Add a bounded allowlist in job.failure_reason for NEW
failures only: format_output_size, format_output_json, format_output_schema,
format_output_binding, format_output_classification, format_output_unresolved,
format_output_edit_range, format_output_edit_conflict, format_output_edit_policy,
format_output_correction_limit, format_output_window_set, format_output_source,
format_output_store_validation. Map typed rejection sites, not raw exception strings,
to these fixed values; use the existing generic reason if the site is unknown.
Do not diagnose a finer Store cause than the validator actually reports. Preserve
failure_stage, terminal state, durable IDs, usage and receipt-before-failure handling.
Never include response excerpts, unknown field names/values, range IDs/offsets,
titles, paths or provider prose in these diagnostics. Successful jobs retain null
failure_reason. Existing receipts are immutable; generic old failures stay unknown.

Version the changed prompt/parser policy in the frozen quote/plan identity. An old
quote or pending job cannot silently adopt new parsing rules or acquire another
dispatch budget; retain existing stale/pause and unknown/no-redispatch behavior.
Do not rerun past failed jobs or issue paid probes as part of this contract change.

Synthetic acceptance covers absent binding fields versus explicit wrong values,
optional versus contradictory unresolved_ids, inert extra fields versus invalid
known fields/duplicate keys, exactly fenced JSON versus commentary/truncation, and
one valid plus one invalid edit publishing nothing. Cover all-window completeness,
protected/UTF-8/overlap/correction limits, Store rejection mapping, bounded oversized
extras, unchanged usage/unknown/replay, and secret-like reply text absent from error
receipts/logs. A recovered formatting-envelope case is not proof of model semantic
quality or of the cause of an earlier generic content_format_invalid_result.

#### Selection, privacy and quote

All operations use the existing owner reader envelope and response.reader; gateway
names are memex.library.reader.format_<suffix>. Unknown fields reject. Read ops
require read scope; request/resume/restore require edit. Common dispatch admission,
current resource checks, response suppression and unknown-result rules apply.
Request identities are principal/library namespaced. Every read of a batch/quote
must authorize its referenced resources; loss of access must not leak hidden target
IDs, counts or change text. No client-supplied ceiling or principal is trusted.
Historical bodies, before/after proofs and model inputs retain at least their source
sensitivity. Reads authorize BOTH the current item and historical evidence at its
own effective protection; lowering current sensitivity cannot expose old private
text. Restore rejects if it would place that historical text under weaker current
protection; it never changes a label to bypass this rule.

Selections are exactly one of:

```json
{"kind":"item","item_id":"item_fixture","revision_id":"revision_fixture","target":{"kind":"body"}}
```

```json
{"kind":"item","item_id":"item_fixture","revision_id":"revision_fixture","target":{"kind":"chapter","side":"original","chapter":"chapter-1"}}
```

```json
{"kind":"group","group_id":"group_fixture","include_descendants":false}
```

Chapter side is original|translation and must exist. body cannot bypass the book
manifest. Directory scope follows the existing visible include-descendants switch,
sent explicitly; do not silently recurse. Snapshot the distinct authorized members,
their revisions/target hashes and membership scope; no duplicates for multiple
folder memberships, no future arrivals, no hidden or trashed targets. Membership
removal, trash, access loss or revision drift before execution skips/rejects that
target without a new model request, never silently substitutes its new content.
Editing shared canonical content is visible through its other folder memberships;
the confirmation must not promise a directory-private copy.

public/internal/private still require the current authority and ceiling. Private
text may go only to the saved Low dial route after explicit confirmation that this
selected text will be sent there for formatting; read access alone is not model
consent. Restricted is excluded from this new model path in v1, not silently
downgraded. Counts expose only authorized resources. This changes no existing
manual/local edit rights and authorizes no agent-run real-library acceptance.

Preview does deterministic local work only; no provider/network call or content
mutation. Eligible uncached targets may use the same narrowly scoped local
Credentials::status rule as translate_preview. Freeze source selection, exact
windows, policy version, saved Low dial route/profile/settings and credential
generation in a server-owned quote. No new model setting or fallback. Known
credential/route unavailability prevents a usable positive-model-request quote;
generation is not readiness. A wholly deterministic/cached plan needs no credential
read and may have route=null and zero model budget.

For each target plan complete non-overlapping coverage windows, with bounded context
overlap counted in input cost but not twice in checked coverage. Each request fits
the saved Low dial input/completion limits; at most 64 planned requests per target
(2026-11 amendment, previously 8: articles over 128 KiB no longer skip as over budget).
This is a separate explicitly quoted formatting budget, NOT an increase of the
old repair/proofread two-call cap. If full coverage cannot fit, skip as
budget_exceeded before billing; do not silently process only a prefix. Actual
max_requests is the plan count, not always 8. No automatic higher model, larger
profile, malformed-output retry or unquoted extra dispatch. Cache-reused and
deterministic-only targets cost zero provider requests.

Show distinct item/target counts, eligible/skipped counts and reasons, private
target count, selected route, total maximum requests/input bytes/completion tokens
before one confirmation. estimated_cost uses the translation estimate schema or
null when pricing is unknown; never label unknown money as free. Quote expires
15 minutes after becoming ready. No model work starts during directory expansion.

#### Frozen operations and durable result shapes

- format_preview: params {selection} starts a local preview; params {preview_id}
  reads the same preview. These are exclusive shapes. Return
  {preview_id,state,quote,error_code}, state=preparing|ready|failed. This lets large
  directories finish without increasing the normal RPC deadline. quote is null
  until ready; failed has a sanitized error. Quote is
  {quote_id,expires_at,selection,policy_version,route,counts,budget,estimated_cost}.
  counts={items,targets,eligible,skipped,private_targets,skipped_by_reason}; reasons
  map to counts of visible targets only. budget={max_requests,max_input_bytes,
  max_completion_tokens}, the checked sums of target budgets (safe JSON integers).
- format_targets (read): {preview_id,offset,limit}, offset in targets, limit=1..100.
  Return {preview_id,total,offset,next_offset,targets}; each target is
  {target_id,item_id,revision_id,target,source_sha256,source_bytes,state,reason,
  budget}. state=eligible|skipped. Target budget uses the earlier translation
  budget keys with Low dial limits. Body hashes cover complete canonical body;
  chapter hashes cover only that side's chapter. Skipped targets may have null
  source_sha256/source_bytes/budget when no body planning was necessary; an eligible
  target has all three. reason is null for eligible targets. No body text in rows.
- format_request: {quote_id,request_id,accept_cost:true,accept_model_processing:true}.
  `accept_cost` may be omitted (no longer a gate); `accept_model_processing:true` is
  still required. One step: format_preview with a selection also accepts an optional
  `auto_request_id`; when the preview becomes ready the server admits it through this
  same request path (same quote freshness/source checks) and preview status reports the
  additive `batch_id`. A preview that cannot be admitted ends failed with the request's
  error code.
  Persist before dispatch and return {batch}. Even a single target uses this shape.
  Exact replay returns the same batch before quote-expiry validation, subject to
  current access; conflicting reuse rejects. A new request requires a ready,
  unexpired unchanged quote. Atomically consume quote_id into exactly one batch in
  its principal/library namespace; a different request_id cannot reuse that quote
  to create another batch, even after failure/unknown. Reject as
  content_format_quote_consumed with zero dispatch. An explicit new attempt needs
  a fresh quote/confirmation and keeps earlier failed/unknown costs separate.
  No fire-and-forget frontend expansion into N requests.
- format_status (read): {batch_id} -> {batch}.
  batch={batch_id,request_id,state,counts,usage,error_code}; state is
  queued|running|paused|done, not a claim that every target succeeded. counts has
  total,queued,running,succeeded,failed,outcome_unknown,skipped; categories partition
  the frozen target list. usage has requests_dispatched,reported_total_tokens,
  unknown_usage_requests,input_tokens,output_tokens with Common nullable-sum rules;
  per-dispatch detail stays on individual target jobs, not an unbounded batch array.
- format_results (read): {batch_id,offset,limit}, limit=1..100 ->
  {batch_id,total,offset,next_offset,results}. Each row is
  {target_id,item_id,revision_id,target,state,reason,job,formatting}; queued/skipped
  may have job=null. Row states are the six batch count categories other than total.
  A child job is {job_id,request_id,operation,item_id,revision_id,target,state,stage,
  result_revision_id,error_code,failure_stage,failure_reason,budget,usage,elapsed_ms}.
  operation=content_format extends the earlier Common operation enum for this
  capability only. Child states/usage/failure fields use Common semantics;
  stage=queued|formatting|committing|done. result_revision_id is nullable. No invented
  chapter or alignment fields for an ordinary body. formatting is null until a
  succeeded receipt, otherwise {status,changed,applied_count,correction_count,
  deleted_blocks,unresolved_count,checked_bytes,total_bytes,changes_version,
  quality:"not_certified"}. status=complete|partial: partial means identified
  ambiguities or protected/unexamined spans remain, not hidden transport failures.
  Counts include only committed edits; correction_count excludes whitespace and
  garbage deletion. Successful unchanged results still retain an empty proof.
- format_changes (read), view=list: {batch_id,target_id,expected_version,view,
  offset,limit}, limit=1..64 -> {version,item_id,input_revision_id,result_revision_id,
  target,source_sha256,result_sha256,total,offset,next_offset,changes}. Each change
  is {id,kind,basis,reason_code,old_range,new_range,old_sha256,new_sha256,
  before_bytes,after_bytes,source_evidence_ids}. Span hashes cover exact before/after
  bytes, including the empty-string hash for a deletion's after span. reason_code
  is navigation|advertisement|saved_control|account_widget|paragraph_layout|
  short_correction|source_garble|punctuation. Model/profile/prompt identity remains
  bound in the immutable proof, not duplicated as free text in every row.
  view=text instead takes {batch_id,target_id,expected_version,view,change_id,
  part,start_byte,limit_bytes}, part=before|after, limit_bytes=1..8192, UTF-8 aligned;
  return {version,change_id,part,total_bytes,start_byte,end_byte,next_offset,text}.
  This permits full review of a large deletion through bounded authenticated reads.
- format_restore: {batch_id,target_id,expected_revision_id,request_id} ->
  {item_id,request_id,revision_id,changed}. Restore only that operation's target
  bytes from immutable evidence into a NEW revision, never rewind current to an
  old revision ID, delete history, or restore old metadata/permissions/memberships.
  Require current revision to equal both expected_revision_id and the formatting
  result revision. A later edit conflicts and preserves the draft; no silent merge.
  Restore affected chapter offsets/manifests coherently and invalidate derived
  alignment/AI projections without new inference. A no-change target returns
  changed=false. Exact restore replay returns its one original receipt.
- format_resume: {batch_id} -> {batch}, idempotently resumes the same paused batch
  under fresh authority and remaining ORIGINAL budget; no new request identity or
  confirmation charge. A changed route/generation/policy cannot be silently used:
  leave pending work paused for a new explicit preview/confirmation. This never
  resends an unknown/failed provider attempt or resets its cost ledger.

All paging uses the frozen target order/changes_version; mutable result rows may
advance state but never change target identity. Preview/quote/receipt records and
uncommitted input are private retained work, not disposable Library garbage.

#### Target access versus complete-batch access

Freeze v1 without a filtered/redacted batch representation. Every response exposing
the complete batch (status, results, request replay and resume) requires current
authorization for the selection and ALL frozen members, including their applicable
historical protection. If any member is no longer accessible, reject the whole
response with the existing sanitized permission/missing-resource error; expose no
member identity, counts, usage, rows, hidden-member count or denial-specific detail.
Do not filter members, renumber pages, change totals, or invent a redacted field.
Permission failure does not mean the durable batch or its receipts were deleted.

An explicitly addressed format_changes or format_restore for known target A is
independent of another member B's visibility. Validate principal/library binding,
A's membership in the original frozen batch, A's current item/trash authority and
the historical revision/Evidence/proof authority for every returned/restored byte,
using the current ceiling bounded by the original grant. Do not require current
access to B or to the original group merely to read/restore A. Never return batch
metadata through this target path. Unknown, foreign or unauthorized target IDs
must fail without revealing other members. Existing restore CAS, protection and
edit-scope requirements remain unchanged.

After identity and current/historical target authorization, exact restore replay
returns its settled receipt before configuration/generation or unrelated busy
checks; it neither creates a revision nor resends work. A settled Store publication
must be reconciled as a historical committed fact before applying gates intended
for NEW work. This does not bypass access checks or authorize a new provider call.

For v1, resume rejects BEFORE requeueing when complete-batch access fails. It does
not secretly resume while returning an authorization error. This is an explicit
limitation: partial-access resume is NOT supported by the current response shape.
Preserve the original batch, identities, receipts and remaining budgets; once full
access is legitimately available again, the same resume may proceed under all
existing guards. Do not advise restoring access/trash merely to bypass protection,
create a replacement batch automatically, or repeat failed/unknown paid attempts.
Failure of a status/results read does not cancel an already admitted running batch:
its per-target dispatch guards still stop inaccessible targets without making an
unrelated authorized target fail. Ordinary target failure/unknown isolation remains.
Clients retain already-known authorized target handles so their review/restore
does not depend on successfully reloading the full batch first; reauthorize every
request and never render an inaccessible target from stale cached result data.

Synthetic acceptance adds an asymmetric A/B batch: after B becomes inaccessible,
A's changes/restore and exact authorized replay remain usable, B cannot be read or
restored, full status/results/request replay/resume disclose no partial aggregate,
and rejected resume does not requeue or dispatch. An already running batch still
guards each target independently. Reauthorization preserves original IDs/budgets
and never retries unknown work; neither configuration drift nor unrelated busy
state turns a legitimately settled A receipt into failure.

#### Execution, recovery and acceptance

Per-target work is serial with that item's repair/translation/edit commits, but
never locks every batch item while waiting. A busy target may be skipped/busy,
not starve the rest. One failed or outcome_unknown target does not stop others.
Immediately before EVERY provider dispatch, including later windows of one target,
recheck current resource authority/sensitivity/trash, frozen membership scope,
revision/target hash, route/settings/credential generation and remaining budget.
Resource drift stops that target's remaining calls; configuration drift pauses
pending work for a new quote. Never disclose newly Restricted or changed text just
because the first window was authorized. This guard does not recall an already
sent request or change Common session semantics for accepted durable jobs.
Use bounded scheduling; initially at most one in-flight model request per batch,
with no writer lock during network I/O. Persist intent and usage around each call.
After interruption reconcile receipts and pause unstarted work for resume. A known
complete response may continue local validation/commit without model redispatch;
an uncertain attempt or an invalid reply of a window is retried automatically at most 2
times (backoff 30 s, 60 s; each retry may re-send only that window); then it stays
outcome_unknown/failed. Definite provider refusals (rejected, invalid request,
credential scope or model mismatch) are never retried automatically.
Transient commit-only retry follows the existing bounded-backoff durable pattern,
does not consume a new provider request and must not block unrelated queue work.

CAS/journal commits body, immutable Evidence/change proof and receipt consistently;
readback-visible publication cannot be reported as a definite failure. Preserve
all unselected bytes, other edition/chapter identities, source artifacts, metadata
and permissions. No-op does not create a revision. Within one policy version,
already-formatted current result revisions with settled receipts skip without a
new call, including their honest unresolved status; failed/unknown attempts are
not a successful skip marker. Restore removes no historical receipt and produces
a new current revision eligible for a future explicit formatting operation.

The reusable CLI exposes preview/request/status/results/changes/restore/resume over
this same persisted contract, not a private JSONL-only pipeline. Model dispatch
requires explicit --allow-model with the confirmed quote; no inference from read,
preview, import or startup. Credentials and private body text are never CLI arguments
or default stdout. Authorized change-text export is explicit and local-only.

New fixed reasons/errors: invalid_content_format_request, content_format_unavailable,
content_format_preview_not_found, content_format_quote_stale, content_format_quote_consumed,
content_format_batch_not_found, content_format_changes_stale,
content_format_restore_conflict, content_format_invalid_result. Skips use
already_formatted,chapter_required,unsupported_body,restricted,budget_exceeded,
busy,stale_revision,scope_changed; existing permission, missing-resource, identity,
provider unknown and reader_job_in_progress codes retain their meanings. Do not
include hidden IDs, titles, source text or provider prose in an error.

Synthetic acceptance: Chinese/English/mixed imported garbage plus real prose
about advertising; CNKI-like dense abstract with numbers/citations preserved;
footnotes/bylines/code never classified away; ambiguous corrections unchanged;
large deletions fully reviewable; restore CAS, no-op and crash/replay exactly once;
selected book-side isolation and stale alignment; duplicate-folder membership,
explicit descendants, late arrivals/removal and already-formatted skip; private
consent/Restricted exclusion; exact budget/usage with failures and unknowns;
second-window sensitivity/route drift; historical-before privacy after a label
change; two request IDs racing for the same quote produce only one batch;
mid-batch restart/resume continuing remaining work without redispatch; partial/failed
rows honestly rendered, no dirty-editor loss. No real-library fixture/model calls.
Delete-key trash handling remains frontend use of the existing recoverable group
trash seam and does not authorize permanent deletion or change this contract.

#### Before/after comparison and explicitly confirmed reclamation

Additional source: frontend human #3810 / M-034rdhaKUYF7lso4mAK91r,
2026-11-04T08:09:41.836Z: retain the pre-format version for a side-by-side review,
git-like line/intraline red deletions and green additions; after the user confirms
keeping the new version, the old version may be deleted to reclaim space. This is
a product feature request, not permission for an agent to delete current real data.
It supersedes unconditional history-retention wording ONLY for the exact confirmed
pre-format revision's redundant physical snapshot after the checks below. The human
goal is space reclamation, not a request for irrecoverable information erasure.
Freeze v1 as lossless_snapshot_compaction: remove the redundant loose snapshot only
after its exact revision can be reconstructed and validated from compact storage.
This retains readable history; do not advertise it as deleting all old information.
Irrecoverable content retirement would need a separately reviewed retention trust
model and is not implemented or authorized by these operations. Confirmation to run
formatting, successful formatting, UI navigation or elapsed time is NOT deletion
confirmation. Without it, both complete versions and restore evidence stay readable.

Choose exact historical text reads, not model-generated or backend presentation
diffs. Feature reader_revision_text_v1 -> session.info.reader_revision_text.
UDS reader op revision_chunk / gateway memex.library.reader.revision_chunk (read):
{item_id,revision_id,target,start_byte,limit_bytes,expected_sha256}.
target is the same body/chapter shape as above; limit_bytes=1..8192. expected_sha256
is "any" for the first chunk and the returned full target hash for later chunks.
Return {item_id,revision_id,target,sha256,total_bytes,start_byte,end_byte,next_offset,
text}. Validate immutable revision ownership/hash and membership in the current
verified provenance chain, source Evidence/manifest, current
access AND historical sensitivity; this op intentionally permits non-current
revisions without weakening current reader.chunk's stale guards. It cannot traverse
paths, cross items or substitute the current body for a missing historical body.
Versioned full-body/source-side text and newlines are exact, not normalized display
HTML. Any two retained authorized revisions of the same item/target can be compared.
For books never compare English offsets against the Chinese canonical body.

Frontend reuses the existing side-by-side window, old on the left/new on the right,
computes line and intraline differences without an AI request, shows deletions red,
additions green and replacements as delete+insert. Load both complete targets by
bounded chunks; do not present a prefix diff as complete. Images/HTML in text remain
untrusted and must not trigger external loads. Diff rendering is frontend-owned.
The same source/result identities drive restore and reclamation. With a later edit,
restore remains an explicit CAS operation/conflict, never overwrites later work.

Reclamation is a SEPARATE capability: reader_format_reclaim_v1 ->
session.info.reader_format_reclaim. It requires authoritative reference accounting,
supported durable lossless snapshot compaction and crash/replay verification; capability
cannot be declared merely because confirmation UI or unlink() exists. Formatting
and comparison may ship while reclamation remains unavailable. The current Store
validate_owner_edit_chain loads each ancestor revision and validates transitions;
owner-edit journal recovery also loads its base. Direct removal would break this
contract. Centralize all affected revision reads, including recovery, to reconstruct
the exact ItemRevision and pass the ORIGINAL revision_digest/transition validation.
Persist compact bytes plus identity manifest, reopen and verify, then journal the
removal of the exact redundant loose file. Hash-only tombstones or empty bodies are
not substitutes. Until implemented, report provenance_dependency/unsupported_retention.
Do not skip chain validation, rewrite old evidence or count a hidden revision as
physical reclamation. This prerequisite remains an owned implementation task,
not a permanent decision that user-approved redundant storage can never be reclaimed.

Physical removal scope is ONLY the exact input revision's redundant loose snapshot
of each selected successful changed format target, tied to its result revision.
The logical revision and full reconstructable bytes remain. Never delete current, unrelated
history, original import/PDF, independent Evidence, annotations, correspondence,
generated projections, receipts or backup packages. Do not cascade through references.
Exclude the legacy root and any revision still pinned to its loose representation
by a journal, active task or unsupported reader. References resolved through the
verified compact-aware reader remain valid and are not erased or relabelled.
Unknown or unenumerable references block, not optimistically permit, reclamation.
Shared payloads are not exclusive reclaimable bytes. Enumerate actual Store readers,
journals, tasks, history proofs, annotations and indexes, not just filesystem link
counts. A reference requiring the loose representation pins it. Removing a reference
or deleting independent Evidence needs its own scope, not this confirmation. If new
compact storage/manifest overhead consumes all savings, return no_space_benefit rather
than claiming a successful space-saving operation.

Frozen reclamation ops use the same reader gateway prefix and current authorization:

- format_reclaim_preview (read) starts with {batch_id,targets}, where targets is an
  explicit nonempty array of at most 100 target_ids or "all_succeeded" for that frozen
  batch only. Resume its no-write scan with {preview_id,offset,limit}, limit=1..100.
  Return {preview_id,state,plan,total,offset,next_offset,targets,error_code};
  state=preparing|ready|failed, plan=null until ready. plan is
  {plan_id,expires_at,batch_id,version,mode,counts,estimated_reclaimable_bytes};
  mode="lossless_snapshot_compaction"; counts=
  {total,eligible,blocked,unchanged}. Expiry is 15 minutes after ready. Each target is
  {target_id,item_id,input_revision_id,result_revision_id,state,reason,
  estimated_reclaimable_bytes}, state=eligible|blocked|unchanged. The plan privately
  binds the exact deletion set/content hashes/reference-check version and requester;
  it never accepts arbitrary revision IDs or paths from a client. Safe fixed blocked
  reasons are current_revision_changed,referenced,provenance_dependency,
  recovery_dependency,unsupported_retention,no_space_benefit;
  hidden reference identities stay hidden.
- format_reclaim_request (edit): {plan_id,request_id,accept_reclaim:true} ->
  {reclaim}. UI says "保留新版并压缩旧快照以回收空间；历史仍可查看和恢复",
  names the exact old/new versions and shows the estimate after compact-storage
  overhead. It must not claim irreversible erasure. One confirmation may cover the displayed batch;
  no per-target repeated clicks. Atomically bind one plan to one reclaim request.
  Exact replay recovers the same operation; another ID on that plan rejects.
  Recheck current/result/input identities, current authority, reference graph and
  deletion set under the Store's appropriate writer/retention exclusion before
  removal. A new reference or drift blocks that target, never widens the set or
  changes a different revision into the deletion candidate.
- format_reclaim_status (read): {reclaim_id,offset,limit}, limit=1..100 ->
  {reclaim,total,offset,next_offset,results}. reclaim is
  {reclaim_id,request_id,batch_id,state,counts,error_code}; state=queued|running|done.
  counts={total,pending,reclaimed,blocked,failed,outcome_unknown}. Per-target results
  are {target_id,item_id,input_revision_id,result_revision_id,adopted,reclaim_state,
  reason,unlinked_logical_bytes,compact_logical_bytes,net_path_allocated_bytes_saved}; adopted is boolean,
  reclaim_state=pending|reclaimed|blocked|failed|outcome_unknown. Nullable byte values
  mean unknown, not zero. Unchanged targets settle blocked/unchanged with zero bytes.
  Keeping the new version may succeed while reclaim_state=blocked; do not equate
  adoption, successful confirmation or overall done with successful physical reclamation.

Physical removal must be journaled/idempotent and mutually excluded with new references.
Crash/transport loss yields outcome_unknown until reconciliation; same-identity
recovery may finish the EXACT confirmed set, never expands it or repeats model work.
Blocked targets keep their old text; later reference changes require a fresh preview
and explicit confirmation, not an automatic delayed deletion. Confirmation is not
secure erasure of copies or backups. Report logical unlink bytes and path allocation
separately, subtracting newly retained compact objects, manifests and receipts from
net path-allocation savings; volume free-space changes are optional contextual measurements, not
proof those bytes were physically returned on APFS. Evidence remaining in other
objects may make actual reclamation small or zero; disclose that before confirmation.

After verified v1 compaction, comparison/restore reconstructs the SAME exact old
text through the validated compact representation. Do not fabricate a diff, silently
read an unverified backup or advertise the historical content as erased. Missing or
corrupt compact bytes fail closed. Without verified compaction, the original loose
snapshot remains; a later revision may still cause a normal restore CAS conflict.
New fixed errors: revision_text_unavailable,
content_format_reclaim_unavailable,content_format_reclaim_plan_stale,
content_format_reclaim_plan_consumed,content_format_reclaim_not_found.

Synthetic acceptance must cover both full historical texts and exact line endings,
multibyte/deleted spans, cross-item/secret-history denial, large bounded reads;
confirmation absent=zero deletion, same-plan duplicate identities, concurrent new
references, protected Evidence/shared blobs/annotations/indexes retained, provenance
and restart readability with byte-identical reconstruction/digests, interrupted
compaction/removal/replay and truthful net reclaimed-byte
receipts. No real revision is deleted to test these requirements. The CLI exposes
the same compare/read and explicit preview/confirm/status boundaries; --allow-model
or a prior formatting confirmation never authorizes reclamation.

## Bounded reading and AI input

```sh
memex reader --library LIBRARY --max-sensitivity private toc ITEM --revision REVISION
memex reader --library LIBRARY --max-sensitivity private chunk ITEM --revision REVISION \
  --side original --chapter chapter-1 --offset 0 --limit 32768
memex reader --library LIBRARY --max-sensitivity private state ITEM
memex reader --library LIBRARY --max-sensitivity private save ITEM \
  --expected-version VERSION --state state.json
memex reader --library LIBRARY --max-sensitivity private resume
```

The reusable Store API lives in `memex_store::reader`. Ordinary Markdown documents
use side `source`, with a heading-based TOC (or one full-text section); books use
`original` and `translation`. A `ReaderDocument` contains no body: item/revision,
canonical SHA256, default side, alignment kind and editions. Each edition contains
its language, full-body hash/byte length, and chapters `{key,title,level,start,end,
counterpart}`. Chapter ranges form an exact, nonoverlapping partition of that side.

`counterpart` names a chapter in the other edition only when the explicit chapter
sets agree. It means **declared chapter-order correspondence**, not paragraph or
sentence alignment. Front matter/appendices without a counterpart stay accessible.
The user supplied the pair; the backend does not claim to infer equivalent editions.

Chunks carry item/revision, side, side-body hash, evidence ID (book editions), chapter,
absolute `[start,end)` byte range, Markdown and optional `next_offset`. Request offset
is relative to the chapter. Limit is 4–32768 UTF-8 bytes; no split codepoint, no silent
truncation, no crossing into the next chapter. Follow `next_offset` to process a large
chapter. Switch languages by counterpart key, not by byte/paragraph/scroll fraction.
Every chunk revalidates current permissions, trash, revision and canonical integrity.

AI clients can enumerate chapters and consume bounded chunks with these references.
This is a supply API, not an automatic chapter-summary execution pipeline. English
edition offsets **must not** be sent as canonical Chinese `context_read` or legacy
manual-underline offsets. Side/hash/Evidence identity must accompany any new citation.

## Shared bookmarks and resume

`state` returns `{version,state}`. Empty state has an empty `visit_id`, no position,
and no bookmarks. A save sends the complete next state and the version read:

```json
{
  "visit_id": "fresh-id-for-this-deliberate-open",
  "position": {
    "revision_id": "revision_...", "side": "translation", "sha256": "64-hex",
    "chapter": "chapter-1", "offset": 0
  },
  "bookmarks": [{"id":"stable-bookmark-id", "label":"Bookmark", "anchor":{
    "revision_id":"revision_...", "side":"translation", "sha256":"64-hex",
    "chapter":"chapter-1", "offset":0
  }}]
}
```

Anchor offsets are UTF-8 bytes within a chapter, not display-character offsets.
Save is per-item compare-and-swap. A conflicting device receives `stale_reader_state`;
it must fetch/reconcile, never blindly overwrite. Unknown transport/internal/loading
outcomes retain the exact state and expected version. Replaying an identical current
state succeeds without stealing last-opened focus from a newer document.

One atomic file commits bookmarks, per-item position and latest item. Fresh `visit_id`
marks a deliberate open; subsequent saves keep it. Background saves with that same
visit do not change latest. `resume` returns `[item_id,state]` or null, rechecking
current permission/trash. "Latest" means the last **server-accepted new visit**, not
an offline-device timestamp ordering guarantee. Clients share the same owner Store;
this does not establish new mobile pairing, scope or remote access rights.

Old revision bookmarks are retained unchanged and remain visibly stale; new/changed
anchors must resolve on the current revision. Do not search quote text or silently
move old anchors. Clients may clear a stale current position while retaining historical
bookmarks. State is bounded to 128 KiB serialized and 4096 bookmarks, so it fits the
existing owner-socket frame budgets.

### Ordinary documents and cross-device state — B2

Ordinary documents already support this Store/UDS state contract. Use `side:source`,
the edition SHA256 and the real chapter key returned by toc (`section-*`); a document
without headings has the full-text `section-0`. Do NOT introduce null/empty chapter,
absolute body offsets disguised as chapter offsets, a parallel position database or
a reader-state schema migration. Convert a canonical body byte position using the
current manifest: offset = body_byte - chapter.start. At a chapter boundary use the
following chapter at offset zero; EOF may use the last chapter's end. Validate UTF-8
boundaries, revision, side and hash. Rendered DOM/UTF-16 offsets, scroll percentages,
summary/chat positions and AI translation offsets are not canonical source offsets.
Clients can retain pixel/within-paragraph layout hints locally, not in ReaderAnchor.

Add gateway routes `memex.library.reader.state`, `.save`, `.resume` with the existing
UDS payloads (minus op); exact keys and size bounds remain enforced. State/resume
require read scope; save requires read+edit on the currently authenticated owner
session. Principal and ceiling come only from that session, never client payload.
Advertise a separate `reader_state_sync_v1` capability only when routing and daemon
reader support both exist; old gateways retain local-only position behavior.
This extends transport, not pairing, scopes, content-edit authority or model access.

Saves are synchronous per-RPC admission, not persistent background jobs. Recheck the
live gateway grant before admitting each RPC and before disclosing its result; Store
still checks current item access/trash and CAS under its lock. A write admitted before
revocation may finish after it: do not promise atomic remote-button cancellation or
reuse an admitted grant for later saves/retries. This limited admission contract is
not the dispatch/final-CAS live-authority proof required for the held AI batch/map
jobs, and does not release those HOLDs. Lost replies retain an unknown outcome and
the exact attempted state/version, never mint a fresh visit as a retry. Reconnect
requires a new current grant check. Test revoke/expiry/ceiling reduction and lost
responses through the real router/UDS seam before enabling remote state writes.

Preserve per-item CAS and all bookmarks when saving a position. A conflict requires
read/reconcile, not unconditional overwrite or device-wall-clock last-write-wins.
Debounced background saves retain visit_id; an actual deliberate open gets a new
one. Retries/background saves must not fabricate a new visit to steal latest focus;
genuinely new visits retain the existing server-accepted ordering, not device time.
The library's state is owner-shared, not a new per-device or multi-user notebook.

The A-phase plan's automatic paragraph-text relocation conflicts with the existing
revision-bound anchor rule. For this increment, a revision/hash mismatch is visibly
stale and resumes from the start (or clears current position by CAS), retaining old
bookmarks unchanged. No silent text matching or reuse of old offsets in a new body.
Optional cross-revision relocation is a separate future contract, not a condition
for shipping same-revision local or cross-device resume. This also applies to local
position UI; it must not imply an exact recovered position when the binding changed.

Acceptance covers headingless and multichapter source documents, emoji/multibyte
offsets, boundaries/EOF, two-device CAS/bookmark preservation, identical replay after
opening another item, stale revisions, hidden/trashed resume and old capability
fallback. Existing Store tests are a baseline, not proof of newly wired phone/Mac UI.

### Manual reader annotations v1 — approved five-style selection marks

The user approved the v2 rev10 interaction design on 2026-11-08 via the frontend
owner. This permits implementation of explicit user-created marks, not an agent
batch write to real documents. `reader_annotations_v1` is a NEW capability; it is
not the existing `reading_annotations_v1` canonical-underline capability. The
frontend remains sole product writer. No model request is part of saving a mark.

Store owns a versioned per-item ledger separate from `catalog/reader-state.json`,
under `catalog/reader-annotations/<item_id>.json`. Reuse its contained-path checks,
owner-only permissions, writer lock, atomic durable replacement and recovery
primitives. Mark edits must not conflict with position/bookmark saves or change
their version/latest visit. This ledger is user data, not rebuildable query cache;
include it and retained old annotations in backup/restore. No ItemRevision, body,
Evidence, group, metadata, AI insight or model configuration changes on mark edits.

An annotation has a stable opaque `id`, one `style` enum (`bold`, `red`, `blue`,
`underline`, `yellow`) and an immutable anchor. Blue is the user's negative mark,
not a machine verdict; all five count as manual highlights. The anchor binds the
item's `revision_id`, real edition `side`, full edition `sha256`, and ordered
`segments:[{chapter,start,end}]`. Chapter keys come from toc, not a guessed key
pattern; start/end are nonempty half-open UTF-8 byte ranges relative to that chapter.
A selection crossing chapters is ONE mark with multiple segments, committed as one
operation. Segments must be ordered, disjoint and valid against the same edition;
do not silently clip, truncate, quote-search or merge ambiguous selections.

Resolve selection DOM/UTF-16 positions through the renderer's verified source map;
repeated strings, Markdown syntax, emoji and translated text must not be matched
by first substring occurrence. Style is rendered as an overlay, never inserted as
Markdown/HTML into canonical text. Anchor offsets on human original/translation
editions belong to that edition only. AI translations are independent projections,
not `side:translation` editions by default. This edition-anchor v1 never saves marks
on AI output: AI chapter translations use the separate projection-anchored ledger of
the approved increment `reader_annotations_ai_translation_v1` below. Summaries and
chat text remain unsupported (no anchor, no resolver); never fabricate source offsets.
Capability-aware UI distinguishes a persistable selection from an unsupported one.

Allow distinct overlapping marks without merging/deleting another mark. For the
same exact anchor/style, return a duplicate conflict instead of adding another count;
changing a selected mark's style retains its ID and anchor. Renderer combines bold,
underline and yellow independently; red/blue overlap uses latest server-accepted
mark mutation order, not device clock, with stable ID as deterministic tie-break.
AI and manual layers/counts remain separate, and manual styling wins a conflicting
visual channel. A multi-segment mark counts once. Current-view counts exclude stale
or different-side marks; stale marks remain accessible in a separate retained list.

Expose owner UDS reader operations `annotations_list` and `annotations_edit` under
the new capability, mirrored by gateway `memex.library.reader.annotations.list`
and `.edit`. List accepts `{item_id,offset,limit,expected_version}` (null version
only on the first page) and returns `{item_id,version,annotations,total,next_offset}`.
Each returned mark includes a derived current/stale status, never a client-authority
flag. Sort by stable mark ID for paging; do not let later pages mix ledger versions.
Edit accepts `{item_id,expected_version,change}` with one strict tagged change:
`{op:put,annotation:{id,style,anchor}}` or `{op:delete,id}`. Put creates a new mark or
changes only an existing mark's style; changing its anchor under the same ID rejects.
Deleting a mark removes the annotation only, never source text or another mark.
Return the resulting version and affected mark ID/outcome; no full-body response.
Unknown fields/enums, duplicate IDs and malformed ranges reject the whole operation.

Versions must not repeat after a mutation (including put/delete returning to an
earlier set): do not use a content-only hash vulnerable to ABA. Persist the last
mutation's identity (authenticated principal, expected version, canonical change
hash) and result with the ledger in ONE atomic replacement. An immediate exact
retry returns that result without another mutation. After an intervening edit, an
old attempt returns `stale_reader_annotations`; do not replay it against new state
or resurrect a deleted mark. No unlimited request-receipt history is required for
this synchronous CAS contract. Lost response stays unknown until read/reconcile;
clients never replace expected_version and resend blindly. No whole-ledger overwrite
or clock-based last-write-wins. Position and bookmark state stay untouched.

The concrete validation/conflict codes are `invalid_reader_annotation` and
`reader_annotation_conflict`, with existing `reader_annotation_not_found`,
`reader_annotation_limit_exceeded` and `stale_reader_annotations` preserved. These
bounded prepublication rejections are determinate, not transport-unknown. Do not
generalize to "any daemon response proves no write": `internal_error` can follow
publication (for example rename succeeds, then directory fsync fails). A structured
outcome_unknown code, malformed/untrusted response, or internal error without a
proven prepublication boundary retains an uncertain write outcome and exact attempt
identity for reconciliation. This does not authorize automatic retry or treating
received-but-uncertain errors as successful saves.

Bound each item to 4096 retained marks (current plus stale), 64 segments per mark,
and 4 MiB serialized ledger, with a bounded single last-mutation receipt. Requests
must fit 32 KiB serialized, list limit is 1–100 with a 48 KiB result budget (return
fewer records plus next_offset, never truncate a record). Underlying stricter frame
limits still apply. At capacity, deletion remains available; never auto-purge stale
marks to make room. No quoted text, source snippet or client-supplied timestamp is
needed in the ledger, receipt, diagnostic or proof. UI context snippets are derived
only from currently authorized, exactly bound text and are not extra log content.

Creation validates the current revision/edition/hash and full range under the Store
writer lock. On source revision/hash change, retained marks are stale and not drawn
onto the new text. A metadata-only revision (same body identity, see
memex.async_translation.md "Body identity") keeps marks current: they are shown on the
new revision unchanged, because the bound text is byte-identical.
Editing style or deleting a stale mark is permitted only after authorizing the
current item and its bound historical source; its anchor remains unchanged.
Historical read authorization must not be weaker than existing history access.
Hidden/trashed items reveal neither marks, counts nor replay/conflict details.
Deleting/restoring an item or group does not erase/rebind annotations. Unsupported
schema, broken bindings or corrupt ledgers fail closed, not as an empty success.

Gateway list requires read; edit requires read+edit. Inject trusted principal and
ceiling, recheck the live grant before each synchronous RPC admission and before
disclosure, and retain Store current access/trash/CAS checks. B2's admitted-write
revocation boundary applies: no new persistent background authorization, no promise
to undo an already admitted write. Advertise availability only after both daemon
and gateway support it; missing capability never means a fixture was saved.

Legacy `reading_annotations_v1` files/commands remain unchanged and preserved; old
clients cannot overwrite the new ledger or clear five-style marks. New UI may show
legacy canonical underlines as a separately identified read-only compatibility
layer, counted once as manual when current. No automatic migration/copy, cross-side
reinterpretation or implicit merge. A later legacy upgrade needs a separately
verified migration/write-exclusion design. Feature-off hides unsupported editing,
not deletes new user marks; old binaries must ignore the new namespace safely.

Acceptance: round-trip all five styles, overlapping/cross-chapter selections,
repeated text and multibyte source mapping, two-device CAS, lost acknowledgement,
put/delete/put ABA, stale preservation and authorized stale removal, page-version
drift, quota deletion, deny/trash/revoke paths, old-client isolation, corrupt-file
fail-closed and backup/restore. Show actual Mac/phone saved-state readback separately
from UI fixtures. Use synthetic libraries; agent real-data writes are not authorized
by this implementation contract.

#### AI translation marks — approved v1 increment `reader_annotations_ai_translation_v1`

Approved by Staff on 2026-11-08 after the user asked that marks on translations be
saved too. Scope: five-style manual marks on durable AI chapter translations
(`chapter_translation` projections). Summaries/chat are still unsupported. No model
call, retranslation or change to immutable translation publish is part of this.

Anchor (independent variant; edition anchors keep their exact v1 shape and never gain
AI fields): `{revision_id, side:"ai_translation", segments:[{chapter,
translation_version, sha256, start, end}]}`. Version/hash live on EACH segment, also
for single-chapter marks. `chapter` is the `original_chapter` key from toc; start/end
are a nonempty half-open UTF-8 byte range of that chapter's COMPLETE translation
Markdown — not original coordinates, not chunk-local zero. Projection identity is
item+revision+chapter+translation_version+sha256, where `translation_version` is the
effective `version` returned by `translate_metadata`/`translate_chunk` (including
metadata-view inheritance), never a version recomputed from a raw projection. One
mark may span chapters with different versions/hashes; segments are toc-ordered,
disjoint, and saved atomically — any failing segment rejects the whole mark.

Store resolves each chapter under the writer lock through the chapter-translation
resolver (current access, source integrity, full projection validation, inheritance)
and checks effective version, hash, byte length, UTF-8 boundaries and range. Clients
map visible selections through the renderer source map, adding `chunk.start` across
chunks; no substring search, no original offsets as translation offsets. Version or
hash drift before the write is the determinate `stale_reader_translation`; shape/range
errors are `invalid_reader_annotation`/`invalid_reader_anchor`; an old-revision anchor
for a new mark is `stale_item_revision`. Corrupt projections/internal errors propagate
as failures, never as "no translation" or "not committed".

Persistence: `catalog/reader-ai-annotations/<item_id>.json` (schema 1, field
`annotation_space:"ai_translation"`), separate from `catalog/reader-annotations/`,
whose schema, bytes and behavior are unchanged (no "skip AI" flag). Same CAS,
strictly increasing sequence + marks digest (anti-ABA, restore-safe), single last
exact receipt replay (principal, expected_version, change hash) and unknown-outcome
reconciliation as v1. The receipt also records the affected mark's bound
`revision_id`; a replay is returned only after re-authorizing the current item AND
that bound history at the caller's ceiling (no translation body needed, no write).
Otherwise it is `stale_reader_annotations` and discloses no id/outcome/version; a
receipt without the binding is never replayed; the version and receipt change hash are domain-separated with
the AI namespace, so an edition version never passes AI CAS or vice versa. Limits
(AI namespace only; edition quotas unchanged): 4096 retained marks, 64 segments/mark,
4 MiB ledger, 32 KiB request, 48 KiB list page, list limit 1–100. At capacity
deletion still works; stale marks are never auto-purged.

Wire (owner UDS `reader` ops and gateway `memex.library.reader.annotations.list/
.edit`; gateway adds only session-derived ceiling/principal):

- List: `{item_id, offset, limit, expected_version, annotation_space:"ai_translation",
  projection?: {chapter, translation_version, sha256} | null,
  expected_view_version?: string | null}`. `expected_version` and
  `expected_view_version` are both null on the first page and both required (64 hex)
  on later pages. `projection` filters to marks with a segment of that COMPLETE
  identity; a lone translation_version is never a filter. Response:
  `{item_id, annotation_space:"ai_translation", version, view_version, annotations:
  [{id, style, anchor, status:"current"|"stale", order, projections:[{chapter,
  translation_version, sha256, status}]}], total, next_offset}`. `view_version` binds
  ledger version, filter, current revision, the effective projection identities
  consulted and the visible set; any drift (translation change, revocation, hidden
  history) on a later page is `stale_reader_annotations`.
- Edit: `{item_id, expected_version, change, annotation_space:"ai_translation"}`,
  `change` = `{op:"put", annotation:{id, style, anchor}}` (AI anchor only) or
  `{op:"delete", id}`. Response `{item_id, annotation_space:"ai_translation", version,
  id, outcome:"created"|"style_changed"|"deleted"}`.
- Without `annotation_space` both ops are exactly the edition v1 wire and response
  shape: never AI marks or fields; AI-only keys or AI anchors without the space
  reject `invalid_reader_annotation`. A delete without space only touches the edition
  ledger; the same ID in the other ledger is unaffected. Unknown spaces/keys reject.
- Capability `reader_annotations_ai_translation_v1`; gateway `session.info` reports
  `reader_annotations_ai_translation_v1:"available"` only when the daemon advertises
  it. AI list runs on the bounded read-only Store lane; AI edit follows the v1 write
  boundary (after dispatch, non-rejection failures are `unknown`). List needs read,
  edit read+edit.

Staleness: if the revision or ANY segment's version/hash differs from the current
effective projection, the whole mark is `stale` (per-segment status shows which) and
is drawn on no new translation segment; never recovered by substring or hash alone.
UI shows "译文已更新，标记未迁移". Stale AI marks are delete-only in this increment
(restyle is `stale_reader_translation`); current marks may change style; anchors are
immutable. List/delete require the current item and bound historical revision to be
readable; hidden/trashed items disclose no IDs, counts or receipts. If the old
translation body is gone but history access verifies, the mark stays listed (no
excerpt) and deletable; corrupt ledgers fail closed (`reader_annotations_unavailable`).
Unchanged version/hash is never stale due to re-rendering.

Ordinary (non-book) items: the separately frozen increment
`reader_annotations_ai_document_v1` (`interfaces/memex.async_translation.md`,
"Whole-document translation annotations") adds the selector
`projection_kind:"async_document"` inside this same `annotation_space`. Missing
selector keeps exactly the chapter wire, ledger and limits above; the two kinds never
share a ledger (`catalog/reader-ai-document-annotations/<item_id>.json`), CAS
version, receipt, filter, total or view_version, and one mark never mixes kinds.
Document anchor: `{revision_id, side:"ai_translation", projection_kind:
"async_document", projection_id:<document_analysis_id>, translation_version,
sha256, segments:[{start,end}]}` over the complete `DocumentTranslation` body (page
offsets add `start_byte`; titles, covers and name overlays are outside it). List adds
`projection_kind` to the AI list request; its optional filter is
`{projection_id, translation_version, sha256}`; responses carry
`projection_kind:"async_document"`. Store does not depend on Analysis: the daemon
passes an Analysis-validator resolver that the Store calls inside its own read/writer
boundary, and the Store independently checks the record's id, body, purpose and exact
current revision binding before computing the identity. Receipts bind the mark's
revision and Analysis record; replay re-authorizes both. Hidden Analysis records
contribute no marks or counts; a stale document mark is delete-only.

Acceptance (synthetic): two chapters with different versions/hashes, one changed
segment staling the whole mark, UTF-8/repeated text/chunk offsets, same hash with a
different version, version races, two-device CAS and committed-then-internal_error
reconciliation, same-ID isolation across namespaces; an old binary reading/writing
edition marks leaves the AI ledger byte-identical; old client → new daemon unchanged;
write-stopped backup/restore covers both ledgers, projection dependencies and receipts.

### Selection AI and existing highlights — reuse with truthful disclosure

The selection AI button may open the existing conversation composer with the
selected text visibly quoted. Opening/prefilling never sends; only explicit Send
uses the existing item_chat route, privacy gates, budgets and retry rules. It is
user-supplied context, not a new verified citation API. Preserve visible side and
revision identity; never present translated text as an original Evidence quote.
The current chat wire does not freeze expected_revision_id, and the backend also
supplies its usual document/metadata context. Do not claim only the selection is
sent or that the answer is revision-certified; detect known view drift before Send
and disclose that limitation. Bounded selection content must fit existing message/
frame limits; no silent truncation or new system/tool fields.

Explicit “生成 AI 重点” may request only `highlights` via the existing insights route;
reading/toggling the menu does not generate, and failed/unknown is not empty success
or permission for an automatic retry. Source inspection shows highlights/quotes use
the saved Summary purpose, NOT necessarily Low dial. Long documents may be split
into multiple requests, including bounded malformed-response attempts. UI must show
the actual configured route/effort and honestly describe possible multiple billed
calls, not promise “Low” or one call per click without corresponding implementation.
If Low routing is desired instead, freeze that route/configuration change separately;
do not silently alter Summary to make the wording true. This clause authorizes no
agent model invocation or new inference scope, and leaves C1/batch/map HOLD intact.

### Reading passes and optional active-time accounting — approved and implemented v1

Capability `reader_passes_v1` is additive and separate from state, annotations and
AI capabilities. Approval record: this section was frozen as the preimplementation
design in file revision SHA256 b961d65f93fb1cd4…; the user approved that draft and its
interaction design on 2026-11-08, and it has been implemented since (Store, owner UDS
and gateway; the frozen wire is in "Implemented v1 wire" below). Real-library writes
and deployment remain separately authorized. The frontend remains sole product
writer. No model, fee, real-data batch operation, automatic completion claim or
change to content, Evidence, reader-state, bookmarks or annotation ledgers is included.

#### Pass identity, manual dates and lifecycle

Use a separate versioned Store reader-passes module/namespace, included in backup
and restore, not a query cache or analysis job. Each record has immutable opaque
pass_id and server-allocated ordinal within the item. `kind` is immutable
`tracked|retrospective`; method is null or `full|sampled|structural`, status is
`in_progress|completed`. Tracked passes freeze their start revision_id/canonical
hash with `revision_basis:started_on_revision`. Retrospective references are governed
by the section below, not a claim that today's revision was read in the past.

Manual start_date/end_date use optional PartialDate `{value,precision,raw}` following
the metadata date shape, distinct from server created/completed timestamps and
measured intervals. New form writes use valid Gregorian year/month/day precision,
or null for unknown; do not fabricate January 1, month day 1, midnight or timezone.
User edits dates/method without fabricating timing. Compare precision ranges solely
for validation: reject only when the end's latest possible date is before the
start's earliest possible date. Overlapping/unknown ranges leave order uncertain,
not silently resolved; computed bounds are not persisted/displayed as known dates.
In-progress has no end date; tracked completion requires an end date and explicit
user action. Retrospective unknown dates are allowed as specified below. Never
invent dates from added_at or old visit history.
Scrolling to EOF, choosing full, or a long duration does not prove completion.

Create, edit, complete, reopen and delete use per-item CAS, strict fields and atomic
durable replacement/receipts under the existing Store lock. Ordinals increase and
are never renumbered/reused after deletion. Continue targets an existing stable pass
ID across devices; opening on a second device does not create a duplicate. Reopen
does not reset recorded time. An explicit new pass may coexist with an unfinished
older one, including after a source change; do not force false completion or deletion
of that older pass. Each timer targets exactly one explicitly selected pass. A new
reread never resets the previous record. Mere opening creates no persistent pass/
timer: a confirmed start (including the timing control) or explicit completion save
can create one without an extra confirmation ceremony. UI can
offer the first locally observed open date as an editable proposal, not measured fact.

Bind tracked creation to the then-current revision; later content/metadata revisions do not
rebind the pass. Show a source-changed status; new timing on a different revision
requires a deliberately started pass for that revision. Old passes remain readable/
editable under current-item AND historical-source permissions, not silently erased.
Completion closes timing admission; clients flush/reconcile outstanding segments
before completion, or explicitly finish with a visible incomplete-timing warning.
Delayed reports cannot silently add time to a completed/deleted pass. Deletion is an
explicit confirmation removing that pass and its intervals from active statistics,
not deleting the item; retain minimal ID/ordinal/replay tombstones so delayed
packets cannot recreate it. Do not claim deletion erases copies in prior backups.

#### Retrospective entry, approximate dates and display numbering

“补记一遍” creates `kind:retrospective`, normally completed, by explicit save. Its
dates and duration are owner estimates, never recovered measurement. Start/end may
each be null or year/month/day PartialDate; incomplete knowledge must not force a
fabricated end date. Display supplied retrospective dates with an approximate label,
independent of their precision. A retrospective record does not accept timing
sessions/segments and cannot be converted into tracked data; starting a new measured
read creates a tracked pass, preserving the retrospective entry.

`estimated_duration_minutes` is nullable, positive integer minutes up to 525600
(one year's total duration, NOT a per-day allowance). UI accepts approximate minutes
or decimal hours and converts exactly to whole minutes; invalid, zero, negative,
nonfinite, over-limit or sub-minute-unrepresentable inputs reject, not silently
round/clip. Preserve an optional display-unit preference without two conflicting
authoritative values. All nonnull estimates are visibly “约”; tracked records keep
this field null. No fabricated timer intervals, mode allocation or day-by-day
distribution may be derived from an estimate. Estimated totals, if displayed, stay
in a separately named aggregate and never enter measured unions/daily charts.

Retrospective creation may freeze the then-current revision/hash and length policy
as `revision_basis:reference_at_entry`. This is a calculation reference, not evidence
that the user read that revision on the claimed dates. An unknown reference remains
null with unavailable lengths/rates. Do not auto-pick a historical revision by date,
silently rebind on later edits or copy today's body into the ledger. Current-item and
bound-history permission checks still apply; no new historical access is conferred.

Server ordinal stays a nonreused creation identity, not the displayed chronology.
Derive `display_rank` for all currently authorized, undeleted passes of the item,
before optional method/kind filters: known starts first by normalized PartialDate
value ascending (prefix-shorter first), then creation ordinal and pass_id; unknown
starts last, ordered by ordinal/pass_id. Unknown/unparsed values, if ever read, are
not guessed into a calendar position. Editing dates, adding a retrospective entry
or deleting another pass may change display_rank; never use it for mutation, CAS,
dedup, timer targeting or links. Label it “按填写日期排序的第 n 遍”; overlapping partial
dates and ties do not prove actual historical sequence. Keep partial precision visible
instead of promoting the sort key to a factual exact date. Optional display filters
retain those authorized ranks (gaps are allowed); forbidden records never participate
in ranking or disclose their identities/counts through it.

Date display preserves stored precision: day renders 年/月/日, month only 年/月,
year only 年. If a known date's year equals the current year in the pass reporting
timezone, omit that year for month/day precision; year-only renders “今年”, not an
empty string. Other years stay explicit; unknown shows 未知. “约” remains visible on
retrospective estimates. This is rendering only, not a schema rewrite, timezone
conversion or new claimed day. The same display rule applies to tracked manual dates.

For a retrospective record with a positive estimate and verified reference lengths,
permit rates using that estimate as T, prominently labelled “约 · 按补记参考版本的
全文长度估算”; sampled/structural/unselected methods additionally say “等效全文速率”.
Even full is an estimate, not measured speed. Missing duration/reference/source or
translation yields unavailable affected rates; never use elapsed calendar dates as
duration. Unit separation and no mixed words+characters scalar remain mandatory.
Editing the estimate recalculates only estimated rates, never measured statistics.

#### One clock, opt-in activity, bounded synchronized reports

Timing is optional per active pass and starts only from the user's timing control;
opening the chart or reading the page without that opt-in must not start telemetry.
Only foreground-visible reading with a qualifying user scroll/tap/key/pointer
interaction in the preceding 120 seconds accrues time. Use monotonic elapsed time;
split at mode changes, hide, lock, sleep, pause and the exact idle deadline. Timer
ticks, network activity, animation and programmatic scrolling are not user activity.
Never credit the entire delayed interval after browser throttling, suspension or
sleep; an unobservable gap is unmeasured, and resume requires a fresh sample.

A report is a union of nonempty half-open active intervals, each tagged with exactly
one mode `source|bilingual|translation`. No text, reading position, scroll trace or
keystroke payload is stored. Interval timestamps ARE necessary timing metadata for
cross-device overlap/day accounting; a duration-only sum cannot support deduplication.
Do not claim the daemon can independently attest browser visibility or human reading.
Call the metric device-reported active dwell time, not a tamper-proof attention log.

Use server-issued timing sessions bound to item/pass/revision, authenticated client
identity and daemon boot, with a server UTC/monotonic origin. Clients report monotonic
offsets from the corresponding session sample, not an arbitrary trusted wall clock.
Each session covers at most 300000 ms; renew while actively reading, never mint
sessions retrospectively for hours of offline time. Sync samples/round-trip delay
make cross-device alignment an estimate; expose insufficient synchronization rather
than certifying exact overlap. Daemon restart invalidates live sessions, not accepted
records. Server clock discontinuity suspends admission until a new coherent sample;
it must not make backward-clock time or sleep gaps billable as reading.

Each segment is at most 300000 ms and must lie within its issued session and elapsed
server-observed window. Reject negative/reversed, future, oversized or out-of-session
reports; do not silently clamp malformed input into apparently valid reading time.
Normal client segmentation observes the 120-second idle cutoff within this bound.
Connectivity loss may leave a bounded unsynchronized tail; show it as pending/missing,
not successfully saved time. No unlimited offline catch-up is promised by v1.

Freeze immutable segment IDs/payload hashes scoped to session+principal, including
mode and offsets. Exact replay is a no-op; same ID with changed payload conflicts.
Persist accepted intervals, dedup identity and their receipt atomically. Reordered
reports do not lose previously accepted time. Current pass/access/trash checks precede
admission and replay disclosure. A previously accepted receipt remains reconcilable
after session expiry/restart/completion without a fresh live timing session; this
does not admit a previously unaccepted interval. Deleted-pass receipts reveal no
removed interval data and cannot recreate the pass. Metadata CAS and timer append share one serialized
ledger boundary, but a routine heartbeat must not invalidate an unrelated manual-date
CAS: keep separate metadata and timing versions. Final completion/delete fences both.
Unknown acknowledgements retain the original IDs/payloads for read/reconcile, never
mint replacement segments or silently rebase a stale pass mutation.

#### Overlap and dates

For one pass, combine ALL devices' accepted intervals by set union, not by adding
their totals. Every instant contributes at most once. Where modes overlap, the
first server-accepted interval owns that instant; later intervals contribute only
uncovered time. Persist that acceptance order, not device wall-clock order. Thus
source+bilingual+translation time equals the unique total; separately showing device
diagnostics must not add another copy to the reading total. Report accepted versus
overlap-excluded duration, preserving replay-stable results.

Pin an explicit IANA reporting timezone per pass (initially the user's configured
zone, not a hardcoded platform default). Retain UTC intervals and split at that
zone's calendar midnight; changing editable reading dates never moves measured
minutes. Each day's union is bounded by that local day's actual elapsed duration
(usually 24 hours, 23/25 across DST), not devices times 24 hours. Round only for
display after summing integer milliseconds. A future library-wide chart must union
overlapping intervals across passes/items again; it cannot simply add per-pass days.
If shown together, explain that per-pass totals need not sum to unique library time.

#### Lengths and three equivalent-rate views

TOC bytes are NOT words or characters. Derive counts deterministically on the trusted
Mac from the exactly bound textual source, with a versioned text-extraction/tokenizer
policy and source hash; cache counts, not private body text in the timing ledger.
Exclude Markdown syntax/hidden link targets, not visible headings/list/table/code
text. Use defined Unicode word boundaries for word-based text and defined CJK
grapheme counts for character-based text; preserve units rather than guessing a
language-dependent bytes-to-words ratio. Mixed text exposes separate unit counts.
No model/token billing tokenizer is a substitute for displayed words/characters.

For total unique active minutes T, each view is length/T, never T/length: source
count/T; translation count/T; and their combined view. The combined view adds counts
only when units/policies agree; otherwise display, for example, words/min PLUS
characters/min, never a fabricated mixed-unit scalar. These are equivalent full-text
rates using ONE total time, distinct from actual per-mode dwell times. For sampled,
structural, unchosen or in-progress passes label them estimates, not measured reading
speed or proof that all text was read. Even a completed full pass is self-reported.
Zero measured time yields null/unavailable, not infinity or a substituted minute.
This measured denominator applies to tracked passes only; retrospective estimated
rates use the explicitly separate denominator and labels defined above.

Freeze source length identity when starting the pass. A translation length requires
its own validated artifact/version/output hash, not the source byte count or an
assumed human edition. Missing/incomplete translation yields unavailable target/
combined rates while preserving source time/statistics. A translation added later
may be explicitly bound for an unbound target, with CAS and provenance, but never
silently changes a historical denominator. Already bound lengths remain immutable
for that pass; projections can be recomputed only with the same hash/policy identity.

#### Bounds, access and proof

Use bounded paginated read/edit/timing-session/report/statistics operations behind
reader_passes_v1; their exact serialized DTOs are frozen in "Implemented v1 wire".
Permit at most 4096 live (undeleted) passes per item and 65536 retained pass
identities including tombstones, 65536 accepted segment identities per pass,
64 intervals and 32 KiB serialized per report, and 48 KiB per read page. Compaction
must preserve exact overlap/mode/day and replay semantics, never discard unacked
identities to admit an old packet twice. At capacity pause new recording with a
visible limit; allow completion/edit/deletion and preserve prior data. No silent
rollover, purge or false success. Storage and receipt bounds are frozen in
"Implemented v1 wire"; do not allocate by untrusted offsets.

Read uses read, all writes/report/session starts use read+edit; principal/ceiling
and authenticated device binding are server-derived. Local owner follows the same
Store access/CAS constraints. B2 synchronous grant admission/disclosure checks apply,
not background grant snapshots or a claim to cancel an already admitted write.
Hidden/trash/history rules cover statistics, counts, sessions and receipts too.
Old binaries ignore the new namespace; feature-off preserves user data. Backup
restores passes, intervals, length identities and dedup/version state, not active
timers, device grants or sessions. Real library writes and deployment remain separate
from approving this design; all existing AI/remote-delete HOLDs remain unchanged.

Required synthetic proof includes two-device overlapping/different-mode reports,
exact replay/conflicting IDs/out-of-order arrivals, idle at 119/120/121 seconds,
mode switching, sleep/tab throttling, server/client clock rollback, midnight/DST,
300000-ms bounds, lost reply/restart, complete/delete versus delayed packets,
metadata CAS versus heartbeat, stale revision, hidden/history permissions, Unicode
counts/mixed units/missing translation/zero time, quotas, backup/restore and old
client isolation. Add retrospective null/partial dates, overlapping versus impossible
date ranges, current-year month/day/year display, hour-to-minute exact conversion,
estimate limits, kind/timing isolation, chronological insertion/reranking without
identity changes, and measured/estimated aggregate separation. UI samples do not
prove timer behavior; run both real client event paths on synthetic documents now
that the design is approved.

#### Implemented v1 wire, limits and counting policy

Owner UDS uses the reader envelope `{"command":"reader","max_sensitivity":…,
"principal":…?,"request":{"op":…}}`; success data is `response.reader`. Without a
principal the local owner is `local_owner`. The gateway methods are
`memex.library.reader.passes.{list,stats,edit,timing_start,timing_report}` and take
exactly the UDS request minus `op`; unknown or missing keys reject before dispatch.
The gateway injects ceiling and the authenticated device principal (64 lower-hex)
from the live session; list/stats need read, edit/timing_start/timing_report need
read+edit, rechecked before each RPC admission and before disclosure.

| UDS op | Gateway method | Scope |
|---|---|---|
| `passes_list` | `.passes.list` | read |
| `passes_stats` | `.passes.stats` | read |
| `passes_edit` | `.passes.edit` | read+edit |
| `passes_timing_start` | `.passes.timing_start` | read+edit |
| `passes_timing_report` | `.passes.timing_report` | read+edit |

Requests (`expected_*` may be null only on the first page, `offset: 0`):

```json
{"op":"passes_list","item_id":"item_…","offset":0,"limit":100,"expected_version":null}
{"op":"passes_stats","item_id":"item_…","pass_id":"pass_…","offset":0,"limit":400,
 "expected_timing_version":null}
{"op":"passes_edit","item_id":"item_…","expected_version":"64-hex","change":{"op":"…"}}
{"op":"passes_timing_start","item_id":"item_…","pass_id":"pass_…"}
{"op":"passes_timing_report","item_id":"item_…","pass_id":"pass_…",
 "session_id":"tsess_…","segments":[{"segment_id":"seg-1","mode":"source",
 "start_offset_ms":0,"end_offset_ms":60000}]}
```

`limit` is 1–100 for list and 1–400 for stats. `pass_id` is client-generated,
`pass_` + `[A-Za-z0-9_-]`, at most 128 bytes; `segment_id` is `[A-Za-z0-9_-]`, 1–128
bytes. `change` is exactly one strict tagged object:

```json
{"op":"create_tracked","pass_id":"pass_…","revision_id":"revision_…",
 "method":null,"start_date":null,"timezone":"Asia/Shanghai"}
{"op":"create_retrospective","pass_id":"pass_…","method":null,"start_date":null,
 "end_date":null,"estimated_duration_minutes":null,"duration_display_unit":null,
 "timezone":"Asia/Shanghai","reference_revision_id":null}
{"op":"update","pass_id":"pass_…","method":null,"start_date":null,"end_date":null,
 "estimated_duration_minutes":null,"duration_display_unit":null}
{"op":"complete","pass_id":"pass_…","end_date":null,"expected_timing_version":null}
{"op":"reopen","pass_id":"pass_…"}
{"op":"delete","pass_id":"pass_…","expected_timing_version":null}
```

`method` is null|`full`|`sampled`|`structural`. A date is null or
`{"value":"YYYY[-MM[-DD]]","precision":"year|month|day","raw":null}` (`raw` optional,
≤256 bytes, no controls); `unparsed` is not writable. `estimated_duration_minutes` is
null or an integer 1–525600 (retrospective only); `duration_display_unit` is
null|`minutes`|`hours` and requires an estimate. `timezone` is a resolvable IANA name.
`update` replaces every editable field. `reference_revision_id` may only name the
then-current revision. A non-null `expected_timing_version` on complete/delete must
equal the pass's current timing version.

Responses:

```json
{"item_id":"item_…","version":"64-hex","pass_id":"pass_…","ordinal":1,
 "outcome":"created|updated|completed|reopened|deleted"}
```

`passes_list` returns `{item_id,version,current_revision_id,passes,total,next_offset,
pass_limit:4096}` in display_rank order. Each pass is:

```json
{"pass_id":"pass_…","ordinal":1,"display_rank":1,"kind":"tracked|retrospective",
 "status":"in_progress|completed","method":null,"start_date":null,"end_date":null,
 "estimated_duration_minutes":null,"duration_display_unit":null,"timezone":"Asia/Shanghai",
 "revision_basis":"started_on_revision|reference_at_entry|null","revision_id":null,
 "canonical_sha256":null,"source_status":"current|changed|null",
 "lengths":{"policy":"memex-pass-count-v1",
  "source":{"side":"source|original","sha256":"64-hex","words":0,"characters":0},
  "translation":null},
 "created_at_ms":0,"completed_at_ms":null,
 "timing":{"timing_version":"64-hex","accepted_ms":0,"unique_ms":0,
  "overlap_excluded_ms":0,"source_ms":0,"bilingual_ms":0,"translation_ms":0,
  "segment_count":0,"segment_limit":65536,"recording_paused":false},
 "rates":{"basis":"measured|retrospective_estimate","minutes":null,"estimate":true,
  "equivalent_full_text":true,
  "source":{"words_per_minute":null,"characters_per_minute":null},
  "translation":{"words_per_minute":null,"characters_per_minute":null},
  "combined":{"words_per_minute":null,"characters_per_minute":null}}}
```

`source_ms+bilingual_ms+translation_ms == unique_ms`. Rates are length/minutes with
one denominator per pass and null for zero time, missing length or a zero count;
`combined` adds counts only within one unit. `estimate` is false only for a
completed tracked `full` pass. `passes_stats` returns `{item_id,pass_id,timezone,
timing_version,days:[{date:"YYYY-MM-DD",ms,source_ms,bilingual_ms,translation_ms}],
total,next_offset}`. `passes_timing_start` returns `{session_id,item_id,pass_id,
revision_id,origin_utc_ms,window_ms:300000,max_segment_ms:300000,
report_deadline_utc_ms}`. `passes_timing_report` returns `{item_id,pass_id,session_id,
outcomes:[{segment_id,outcome:"accepted|duplicate"}],totals}` with totals shaped as
`timing` above.

Errors (all in the owner-socket allowlist and gateway allowlist):
`invalid_reader_pass`, `invalid_reader_pass_dates` (end's latest possible day before
start's earliest), `invalid_reader_pass_timing`, `stale_reader_passes`,
`stale_reader_pass_timing`, `reader_pass_conflict` (duplicate/tombstoned ID or a
completion/reopen in the wrong status), `reader_pass_not_found`,
`reader_pass_limit_exceeded`, `reader_passes_unavailable` (unreadable, foreign or
unsupported files; fail closed), `reader_pass_closed` (completed or deleted pass),
`reader_pass_not_timeable` (retrospective), `reader_pass_session_expired`,
`reader_pass_clock_discontinuity`, `reader_pass_segment_conflict`, plus existing
`library_resource_not_found` and `stale_item_revision`. All but
`reader_passes_unavailable` are prepublication rejections; after write dispatch the
gateway reports any other failure or a lost reply as `unknown`. The Store replays the
immediately preceding exact edit attempt (principal, expected version, change hash)
and returns its stored result; an older attempt is `stale_reader_passes`.

Timing rules. A session is server-issued for a tracked in-progress pass on the
current revision, bound to item, pass, revision, principal, daemon boot and the
server UTC/monotonic origin, and lives only in daemon memory (never persisted, backed
up or restored). It covers at most 300000 ms. Reports are admitted until server
monotonic origin + 360000 ms inclusive (60000 ms grace). Each new segment has
0 ≤ start < end ≤ 300000, length ≤ 300000 and end ≤ server-observed monotonic elapsed
since the origin; segment times are origin_utc + offsets. If |UTC elapsed − monotonic
elapsed| > 2000 ms the session is invalidated (`reader_pass_clock_discontinuity`);
exactly 2000 ms is admitted; accepted intervals are kept. After the deadline, a
restart or a discontinuity, only exact receipts reconcile: an accepted
(session, principal, segment_id) with the same payload hash (session, ID, mode,
offsets) returns `duplicate`; no new ID can add time. A report is atomic: all its
segments are accepted or none, earlier saved intervals are never rolled back, and
replaying original IDs keeps reconciling. A different payload under an accepted ID is
`reader_pass_segment_conflict`. Completion and deletion close admission for new IDs.
Accepted intervals are unioned with first-server-acceptance ownership and split at
local midnights of the pinned zone.

Limits. Per item: 4096 live passes; 65536 retained pass identities including
tombstones (deletion never frees an identity); metadata file `catalog/reader-passes/
<item_id>.json` admits a creation only if it stays ≤ 12 MiB. Per pass: timing file
`catalog/reader-passes/<item_id>.timing/<pass_id>.json` ≤ 32 MiB and 65536 accepted
segment identities. Per report: 1–64 segments and 32 KiB serialized. Read pages:
48 KiB, returning fewer records with next_offset, never a truncated record. Live
sessions: 4096 per daemon boot. At any live-pass, identity or storage limit, creation
or recording stops with `reader_pass_limit_exceeded` (`recording_paused` reports the
segment limit), while edit, complete, reopen and delete remain available and prior
data is kept; records are individually bounded so metadata stays below a 24 MiB hard
read cap. Metadata and timing versions are separate SHA256 values over item/pass,
a monotonic sequence and the records, so a heartbeat never invalidates a date CAS and
a version from before a backup restore cannot match different restored content.
Backup is the whole-library copy with the daemon stopped; the namespace restores
passes, intervals, tombstones, lengths, receipts and versions, never live sessions.

Counting policy `memex-pass-count-v1`, computed on the Mac from each exactly bound
human edition body whose SHA256 is frozen beside the counts: parse CommonMark with
tables, strikethrough and task lists; count visible text and code (inline and block);
exclude Markdown syntax, link/image targets, image alt text and HTML tags. Words are
UAX #29 word segments containing no CJK letter. Characters are extended grapheme
clusters whose first scalar is a CJK letter: Han (U+3400–4DBF, U+4E00–9FFF,
U+F900–FAFF, U+20000–2FA1F, U+30000–323AF) with 々 〆 〇 〻; Hiragana letters
U+3041–3096 and ゝ ゞ ゟ; Katakana letters U+30A1–30FA, the prolonged sound mark ー,
ヽ ヾ ヿ, U+31F0–31FF and half-width U+FF66–FF9D (with ｰ); Bopomofo U+3105–312F,
U+31A0–31BF; Hangul jamo/compatibility jamo/syllables U+1100–11FF, U+3131–318E,
U+A960–A97C, U+AC00–D7A3, U+D7B0–D7FB, U+FFA1–FFDC, excluding the fillers U+115F,
U+1160, U+3164 and U+FFA0. Punctuation and symbols such as ・ ･ ゠ 、 。 brackets,
standalone ゛ ゜ and full-width ASCII forms never count. Source is the `source` or
`original` edition; translation is the `translation` edition when present. Ordinary
documents have no translation length until a separately contracted explicit binding.

### AI-generated navigation overlay — C1 design boundary

Only an explicitly selected unstructured ordinary item is eligible: no usable
heading or timestamp navigation in its current source. The fallback full-text
section is not a structured TOC. Opening, scrolling, importing or changing type
must never start a model request. Deterministic heading/timestamp navigation ships
independently. This is a derived navigation overlay, NOT a replacement for the
reader manifest/chapter keys and NOT an edit to body, Evidence or canonical metadata.

One explicit preview/confirmation binds item/revision/source hash, complete input
plan and coverage, Low profile/model/settings/credential generation, prompt/parser
policy and total budget. At most two model requests per confirmed item job, including
all retries or correction passes; freeze per-call and total serialized input-byte/
output-token limits, not only source length. Show the source disclosure, configured
model and priced estimate when available; unknown money is null/unknown, never free.
Exceeding the configured complete-input budget is unsupported, not a silently
truncated whole-document TOC. No implicit whole-library job, external lookup or
third request. A new paid attempt needs a new explicit confirmation.

Use server-bound source block IDs/byte ranges; model output proposes plain-text
labels, order and references only. Validate all references against the frozen
revision/hash, UTF-8 boundaries and increasing source order. Reject fabricated,
out-of-range or conflicting references rather than guessing offsets. Labels are
untrusted AI text, never commands, HTML or certified original headings. UI states
“AI 目录”; its links target the original canonical source, not translation coordinates.

Persist an independently versioned, checksummed derived artifact and idempotent
publication receipt bound to that exact source; validate current access and source
before publication/read. A changed source makes the overlay stale, not automatically
regenerated or silently rebound. Existing current artifacts and history are preserved;
bad current artifacts fail closed rather than silently falling back to an older one.
Paged reads bind artifact/source versions. Feature-off ignores the overlay without
deleting it or altering reader state. It must be included in documented backup
coverage if user-confirmed generated work is retained, not treated as disposable
query cache that may trigger paid regeneration.

Freeze durable job identity, request/usage ledger, status/stop/unknown recovery and
publication CAS before production wiring. Stop blocks new dispatches; accepted
in-flight work/receipts remain honestly accounted. Crash or unknown never implies
unbilled/no-dispatch, and never auto-retries. Reuse validated execution components,
not old grant snapshots. New remote paid dispatch/commit needs the same outstanding
live-authority seam as batch/map; neither that seam nor real inference is approved
by this design. Synthetic implementation may proceed, production capability remains
unavailable until wire/storage/recovery and authorization evidence are complete.

### Speaker directory — C2 separate data contract still required

A detected speaker label is an item-local textual occurrence, not a verified person.
Aliases or identical names must not automatically merge people, bind social accounts
or create person-registry nodes. A-phase initials/labels require no persistent person registry.
The proposed directory is a separate module, not new canonical bibliography fields.
Keep external-source identities separate from any personal relationship records.

The later versioned contract must define stable opaque speaker IDs; owner-supplied
name/aliases and provenance; optional avatar references and platform-qualified
account references; explicit item/revision/side/hash-bound label associations; and
an optional explicitly approved opaque external person reference. Cross-project identifiers do
not convey access rights. No inferred social-account ownership, avatar downloads,
account scraping, face recognition, automatic person-registry lookup/sync or private relationship
import is authorized. Labels and URLs are untrusted data, not fetch instructions.

Before durable implementation, specify current/history visibility, how shared
profiles avoid leaking associations from hidden items, CAS/idempotent edits,
unlink/delete/restore behavior, schema/version compatibility, backup coverage and
cross-project authorization. Profile visibility cannot be inferred from one public
mention, and profiles must not reveal hidden-item counts. This freezes separation
and privacy requirements only, not a new entity schema or production write scope.
C2 remains separately pending; it does not block B1/B2 or the independent release.

## Owner UDS transport

Capability feature: `reader_v1`. Envelope:

```json
{"command":"reader","max_sensitivity":"private","request":{
  "op":"chunk","item_id":"item_...","revision_id":"revision_...",
  "side":"original","chapter":"chapter-1","offset":0,"limit":32768
}}
```

Operations are `toc`, `chunk`, `state`, `save`, `resume`. Field names match the CLI
contracts, with `item_id`, `revision_id`, `expected_version`, `state`. Success data
is under `response.reader`; errors carry no reader body. TOC currently returns the
whole small manifest and remains subject to the unchanged 256 KiB response budget.
Unusually large manifests require CLI access until paginated TOC transport exists.
Commands use the bounded single-flight query worker; save is classified as a write,
so a timeout cannot be falsely presented as definitive rejection.

The encrypted mobile gateway router exposes `memex.library.reader.toc` and
`memex.library.reader.chunk` with exact parameter keys matching the reader request
(without `op`). Both require the existing read grant and sensitivity ceiling.
`session.info.reader` is available only when the daemon actually advertises
`reader_v1`; missing/rejected/unreachable capability responses mean unavailable.
The pre-B2 baseline includes no mobile state/save/resume routing; the additive B2
extension above requires its own verified capability. Source wiring and synthetic
tests are not proof that the installed gateway or physical phone has been updated.
Frontend follow-through:
chapter popover, original/translation/parallel tabs, small adjacent-chapter prefetch,
virtualized display, selection-aware positioning, bookmark UI, deliberate-open visit
IDs and debounced per-item CAS saves, plus the user's reading-mode button. Rendering
smoothness and actual Mac/iPhone automatic restore require end-to-end UI validation.
