# memex.async_translation_and_summary

## Summary

`memex-analysis` owns independent provider-neutral durable queues for faithful summary and
English→Simplified-Chinese translation. The queues are operational state, never Item/Revision
authority. Successful summary/title/document projections become immutable, exact-revision-bound
`AnalysisRecord` records before their job may become `succeeded`.

## Owner UDS request and saved-provider capability (2026-11-02)

This section records the current source implementation in `memex-agent`'s
`translation_request`, `provider_generation`, `markdown_blocks` and IPC modules.
Deployment was reported by the product writer; this spec update does not independently
verify an installed binary, authorize a provider call, or exercise a real library.
The user has retired Ollama/local-model execution. The only production generation
path is the saved provider route: model settings + scoped Keychain credential,
through the configured CLIProxyAPI transport. No local-model fallback remains.

```json
{"command":"translation_request","item_id":"item_example","max_sensitivity":"private"}
```

The owner-only UDS returns `translation` in its normal response envelope. It is a
current-revision projection with `state`, nullable `reason`, `revision_id` and
nullable `page`; an accepted request is not a synchronous generation result.
Read authorization and the sensitivity ceiling are checked before exposing the
revision or scheduling work; unavailable/hidden items return `library_resource_not_found`.
This ceiling narrows owner access, not remote authentication or new disclosure consent.

- Strongly classified Chinese source returns `not_requested/source_is_chinese`
  before provider checks/enqueue; neither translation nor summary is requested by
  this command in that case. Other ineligible sources return not_requested with
  their bounded skip reason.
- Missing saved translation route/effort/provider credential returns
  `unavailable/no_translation_provider`. An old config block cannot supply an
  alternative execution route.
- Eligible queued/running jobs are authorized for this exact item/revision;
  succeeded jobs are reused. An explicit request for a failed job requeues it with
  a fresh attempt budget and clears frozen partial output/model state. This also
  covers exhausted, malformed and outcome-unknown failures; it is a new owner
  action, not permission to retry automatically or proof no earlier cost occurred.
- If a summary route is also configured, this command additionally authorizes or
  requeues the same revision's summary job. Summary remains an independent queue
  and projection; this response does not assert summary success.

The capabilities response includes the `translation_provider_v1` feature and:

```json
{"translation_provider_v1":{"available":true,"purpose_routes":{"translation":"configured-model-id","summary":null}}}
```

Each purpose route is a model-ID string or null. `available` means the saved
translation route has provider/model/effort and a saved scope-bound credential;
summary alone does not make it true. This local status check sends no model request.
It is not a health check, verified inference readiness, credential validation by
the upstream, or permission to execute an unrequested job. Consumers
must tolerate missing capability on old daemons and must not infer Chat readiness.

### Provider authorization and retry boundary

`translation_jobs.provider_authorized` (and the summary equivalent) is an additive
operational column defaulting to 0. Only explicit requests and the daemon's durable
post-capture hook authorize saved-provider work for the bound revision. Background
reconciliation may repair/enqueue rows but never sets this flag: enabling a saved
provider must not translate the existing library in the background. Pending hook
recovery preserves its receipt identity; it is not a general inventory authorization.
Unauthorized reconciled queued jobs are shown as not_requested, for both translation
and summary, rather than offering a misleading waiting-for-model state. There is
no background_generation mode or local-worker exception to this rule. Existing
validated generated records are not erased or relabeled merely by retiring a transport.

Saved-provider workers lease/retry authorized jobs only. Since the 2026-11
owner-subscription amendment, outcome_unknown (`provider_outcome_unknown`) and malformed
(`malformed_model_output`) failures are part of the bounded automatic retry policy, like
provider failure/timeout: a malformed answer is first re-asked once inside the same
attempt, and a failed attempt is requeued with exponential backoff until `MAX_ATTEMPTS`
(3) attempts, then ends `retry_exhausted`. This applies to whole-document translation
(title and body), AI minutes and insights (highlights, quotes, fact check). Only
authorized jobs are retried; `provider_input_unsupported` and definite refusals are not.
The explicit re-request above still gives a fresh attempt budget. The older `queue.retry(job_id)` API
still requires retryable=true and is not equivalent to translation_request.
All daemon generation now goes through the authorized provider worker; the previous
spec exception for an Ollama background worker is withdrawn.

### Retired config compatibility

The old top-level `translation` block is accepted for parsing compatibility, but
its contents are ignored, do not configure a worker, and are omitted when config
is serialized/written back. Startup emits one fixed notice when a non-null retired
block is present; it does not print the contents. Reading/starting with an old file
does not itself rewrite that file. There is no automatic migration from that block
to model settings, no local-model download/start, and no implicit credential transfer.

### Paragraph-aligned translation and reader modes

The saved-provider document adapter splits source Markdown on blank lines, treating
fenced code as one block even with internal blank lines. It keeps block order and
one translated element per source block. Recognized fenced code, Markdown tables,
image-only and pure HTTP(S)-URL blocks pass through unchanged and are not sent to
the model. This is the implemented block recognizer, not a general promise for
every HTML table, indented-code syntax or prose containing an inline image/URL.

Translatable blocks are requested as a JSON array; the response must have the same
number of nonempty strings. Internal empty lines in translated elements are removed,
line-end whitespace trimmed, and blocks are reassembled with one blank line.
Passthrough block bytes are preserved; the original inter-block blank-line count
and terminal newline are not part of that byte-preservation guarantee.
A count/shape mismatch on a multi-block batch splits it into halves recursively,
within a bounded request budget, in the same attempt; a single block that still fails
is kept in the original with an explicit marker (see "Failure diagnostics, partial
documents and segment size"). This bounded internal recovery is distinct from retrying
a failed queue job.
Outcome-unknown transport errors do not trigger that split/retry path.

The reader offers 原文 / 对照 / 译文. 对照 displays source block then its matching
translated block, repeated in order (Bi-Translation-style), not two independent
columns or a new translation request per display switch. Only exact-current-revision
validated output is usable. Block correspondence is not a UTF-8 byte-offset mapping:
translated selections cannot be presented as original Evidence coordinates. Legacy
outputs without established alignment must not be assigned fabricated block matches.

### Failure diagnostics, partial documents and segment size (2026-11-09)

Source implementation in `memex-analysis` `translation` and `memex-agent`
`provider_generation` / `markdown_blocks`; not a claim of deployment.

- `translation_jobs` gains additive, idempotent columns `failure_detail TEXT`,
  `failure_context TEXT`, `started_at TEXT`, `title_untranslated INTEGER`
  (`add_columns`, one IMMEDIATE transaction; old daemons ignore them).
- Every failed attempt stores `failure_detail = scope:check`, scope one of `title`,
  `document`, `glossary`, `provider`, `worker`, `batch_N` or `batch_N_of_M`, check a
  fixed lowercase code (`count_mismatch`, `json_parse`, `incomplete_length`,
  `incomplete_finish`, `invalid_response` (incl. empty answer), `model_mismatch`,
  `outcome_unknown`, `rejected`, `cjk_ratio`, `too_many_untranslated`, `identical`,
  `too_short`, `too_long`, `unbalanced_fence`, `lease_expired`, ...). `failure_context` is
  JSON of numbers only: `batch`, `batches_total`, `expected_blocks`, `actual_blocks`,
  `output_tokens`, `output_chars`, `elapsed_ms`. A new lease and success clear both.
  Invalid codes are never stored or reported. No source, output or provider text.
- `scoped_translation_status` (`TranslationProjection`) adds optional, skip-if-absent
  `failure_detail` and `failure_context` (failed only), `untranslated_blocks` and
  `title_untranslated` (succeeded only), and `progress.started_at` (RFC 3339 start of
  the running attempt). Consumers must treat all of them as optional.
- Segment answers whose element count differs, which are not a JSON array, or which end
  with `length` are split into halves recursively down to single blocks, within 12 extra
  requests per segment; past that budget the segment fails with its diagnostic. A single
  block that still fails, or a translated block that fails its own check (identical to a
  source block of ≥ 24 ASCII letters and ≥ 4 words, or fewer than 1 Han character per 16
  source letters / per 5 output letters), is kept as
  `<!-- memex:untranslated -->\n<exact source block>`. The job still succeeds; the
  whole-document check (also at read time, with the unchanged prompt digests and record
  bytes) verifies each marked block is exact source text, excludes it from the Chinese
  share, and rejects a document whose marked blocks exceed 1/4 of the source bytes
  (`too_many_untranslated`). A source that itself contains the marker gets no marks.
  Readers should hide the marker line and may label the block as kept in the original.
- Document Chinese share: Han ≥ max(8, ASCII letters of the translated source / 64) as
  before, and Han ≥ 1/4 of the other letters (was Han ≥ all other letters). Title:
  ≥ 1 Han (was 1 or 2 by length) and Han ≥ half the letters, or every Latin word of the
  output is a word of the source title. After two invalid title answers the title keeps
  its original wording (`title_untranslated=1`) instead of failing the job.
- `markdown_blocks::SEGMENT_BYTES` is 12 KiB (was 24 KiB; a 24 KiB streamed answer took
  about 3 minutes). A job whose finished segments were stored under 24 KiB keeps that
  segmentation so paid work is never re-sent. Streaming keeps its 60 s idle / 15 min
  total limits; `PROVIDER_REQUEST_TIMEOUT` (120 s) bounds non-streamed requests only.

### Speaker names and reference names: frozen increment (2026-11-08)

This is an implementation contract, not a claim that a deployed adapter supports
it. Reading preferences offer original name (default), Chinese reference name,
and original name with Chinese reference. Original means the spelling in the
source, usually English for these podcasts; never invent an English name for a
non-English speaker. A missing or ambiguous Chinese reference falls back to the
original name with reference-unavailable state. Preferences affect speaker labels
only, not every occurrence of a name in prose, and do not change model routing.

Both document and reader-chapter translation prompts must preserve source personal
names and explicit speaker labels. Separate person references from glossary terms:
person Chinese renderings are display references, never mandatory substitutions;
ordinary terminology retains its consistency rule. Validate identified speaker
labels against their bound source blocks; prompt wording alone is not proof that
labels survive. Unrecognized labels must not acquire invented identities.

Persist available name references as separate, versioned derived metadata bound
to item/source revision, source hash, exact translation version/hash and explicit
speaker/block identity. Record the basis (model-generated reference or verified
recorded correspondence); a reference is not an authoritative person identity.
Apply current and historical source authorization to reads and receipt replays.
Do not insert new fields into immutable old projections or change their hashes.
Expose optional metadata through a separately advertised supported capability;
absence on old daemons is absence, not a reason to make extra model calls.

Use only references obtained within already authorized generation. Do not add a
glossary call for short documents or reader chapters, which currently have none.
Old outputs remain unchanged. Legacy fallback may use explicit unambiguous labels
within already recorded, validated one-to-one block correspondence; matching only
paragraph counts, paragraph order or a substring is insufficient. With no reliable
correspondence, retain the existing label and report reference unavailable; do not
claim to have recovered its original identity or automatically persist an inference.

Rendering is an overlay, not a Markdown rewrite. It must preserve exact UTF-8
selection/source-map semantics for annotations. An internal selection in a renamed
label cannot be proportionally mapped to original bytes; support an exact whole
label mapping or reject that selection, leaving other text selectable.

Version both translation prompt families and the glossary schema. Freeze the
adapter/prompt contract with new job admission; preserve old jobs' frozen outputs,
request identities and paid work. A version bump must not re-send old batches,
auto-retranslate completed outputs, clear unknown outcomes or silently mix old
and new contracts. If an old pending job cannot safely resume its pinned contract,
hold it with an explicit reason; any new paid request needs the existing explicit
authorization path. Recovery of committing jobs uses their original receipts.
This increment authorizes no real inference, automatic migration or deployment.

### Whole-document translation annotations: frozen increment (2026-11-08)

Ordinary items use the completed `DocumentTranslation` Analysis projection, not
`chapter_translation`. Do not copy it into a synthetic chapter, retranslate it,
or mistake a title, summary, progress buffer or queue success flag for a valid
annotatable document. Reuse the existing validated current translation-pair and
source/Analysis authorization rules. No original-to-translation alignment is
required to mark bytes in the translated document itself.

Expose an additive identity alongside the existing translation page:
`projection_kind=async_document`, `projection_id=document_analysis_id`,
`translation_version`, and `sha256` (64 lowercase hexadecimal digits). The last
field hashes the COMPLETE `DocumentTranslation.body_markdown`, not a page, title,
rendered text, speaker-name overlay or cover. Keep the legacy prefixed
`content_sha256` and existing fields unchanged. The version is lowercase SHA256
of the compact UTF-8 JSON string array:
`["memex.async-document-annotation.v1",item_id,current_revision_id,projection_id,record_digest,body_digest]`.
Here both digests are lowercase hex without a prefix; record_digest hashes the
complete validated AnalysisRecord serialized with serde_json::to_vec, including
its input revision/Evidence bindings and provenance. A byte-identical document
under a different record or source revision is a different identity. The server
produces this version; clients echo it, never reconstruct it from visible text.
No old immutable Analysis record needs rewriting to expose the identity.

Retain `annotation_space=ai_translation`; add the explicit request selector
`projection_kind=async_document`. Missing selector continues to mean the existing
chapter space, with its wire and ledger unchanged. Advertise the independent
capability `reader_annotations_ai_document_v1` only when daemon and gateway both
support the complete path. Do not infer it from chapter annotation support.
Document anchors contain `revision_id`, `side=ai_translation`,
`projection_kind=async_document`, `projection_id`, `translation_version`, `sha256`,
and `segments:[{start,end}]`. All segments bind the same complete document;
there is no invented chapter key and no mixing of projection kinds in one mark.
Offsets are nonempty, UTF-8-boundary, half-open ranges in the complete Markdown;
chunk selections add start_byte. Use verified rendered source maps for visible
text, not DOM character counts. Titles and injected covers are outside this space.

Store the document ledger separately under
`catalog/reader-ai-document-annotations/<item_id>.json` so old chapter binaries
cannot reject or rewrite mixed-schema entries. Reuse the five styles and current
chapter limits (4096 marks, 64 segments, 4MiB ledger, 32KiB request, 48KiB page)
per explicitly selected space. CAS versions, receipts, filters, list totals and
view_version bind this selector; never combine counts implicitly with chapters.
Deletion must select the same space. Include the ledger in stopped-writer backup
and restore; absence is empty, malformed data is an error, not an empty overwrite.

Validate current item, source history, the effective Analysis identity, hash and
all ranges in the same consistent writer boundary as annotation CAS. Reuse the
Analysis validator without introducing a Store-to-Analysis dependency cycle or
trusting client-supplied validation results. Validation outside the write boundary
alone is insufficient. Any effective identity change makes the whole mark stale,
never relocates it by substring or matching hash. Authorized stale marks are
delete-only. Hidden source or Analysis records contribute neither marks nor counts.
Current corruption fails closed; it is not ordinary missing/stale translation.

Exact receipt replay rechecks current and bound historical source/Analysis access,
including after deletion; retain minimal authorization bindings in the receipt.
Legacy receipts lacking sufficient binding cannot replay, and require list
reconciliation. Changed visibility invalidates pagination. Internal errors and
lost replies remain unknown outcomes, reconciled with the same request identity.
This does not broaden inheritance, authorize remote resources beyond existing
read+edit grants, or enable inference, real migration or deployment.

## Post-ingest producer contract

The capture/relay consumer calls this only after `memex-store::ingest` or `commit_capture` has
durably returned its canonical `IngestReceipt`:

```rust
let outcomes = memex_analysis::enqueue_ingest_translations(&library, &receipt)?;
let summaries = memex_analysis::enqueue_ingest_summaries(&library, &receipt)?;
```

For a single exact pair it may call:

```rust
let outcome = memex_analysis::enqueue_item_translation(
    &library,
    &receipt.item_ids[index],
    &receipt.revision_ids[index],
)?;
let summary = memex_analysis::enqueue_item_summary(
    &library,
    &receipt.item_ids[index],
    &receipt.revision_ids[index],
)?;
```

The hook verifies that the receipt revision is still current. Exact receipt replay returns the same
job. It must not run before Store success, reconstruct an Item, enqueue from staged CAS, or let the
relay write Analysis/Store directly.

Translation outcomes are either one privacy-safe `enqueued` job view or `skipped` with a bounded reason:
`already_chinese`, `challenge_or_login`, `source_language_unknown`, `source_too_large`,
`stale_revision`, or `unsupported_item_type`. Unknown and login/challenge content is never sent to a
model. Summary uses the same bounded reasons except `already_chinese`: strongly classified Chinese
source is eligible for a Chinese summary.

## Durable state and identity

`<library>/analysis/translation-jobs.sqlite3` is the existing owner-only operational database.
Translation and summary use separate tables and leases in that file; summary does not copy Store
authority or reuse translation job identity. Each row is exact-revision-bound and stores the typed
revision SHA-256. Public states for both queues are exactly:

```text
queued → running → succeeded
              └→ failed → queued (bounded safe retry)
```

Running work has a lease. Expired work returns to `queued` on the next worker pass. Title and
document outputs are frozen independently under the lease before Analysis application, so restart
or retry reuses frozen bytes. A successful job removes the duplicate frozen output after both
Analysis records are verified. At most three attempts are allowed; only provider failure, provider
timeout are retryable. The daemon automatically requeues at most one eligible failure per worker
pass after a persisted exponential delay (1 second, then 2 seconds); restart cannot reset the clock
or attempt budget. Malformed output, stale revision, conflicting Analysis, invalid provider
configuration, and provider/model mismatch fail closed without automatic retry.

Summary accepts only bounded strongly classified English or Chinese article/webpage input and emits
Simplified Chinese. One summary is bounded to 600 Unicode characters and 8 KiB. Its frozen output is
published as exactly one `AnalysisKind::Summary` record carrying `GeneratedTextPurpose::Summary`,
exact summary job ID, revision digest, model digest, frozen prompt provenance, adapter provenance,
and output limit. The job is `succeeded` only after that exact immutable record is durable and
revalidated; queue state alone never makes summary text visible.

## Provider contract

The provider-neutral queues accept `TranslationModel` and `SummaryModel`; the queue
does not resolve credentials. The saved-provider agent adapter resolves its own
scope-bound credential separately. Their shared descriptor shape records bounded
model ID, model digest, adapter ID/version, and a timeout
expressed as an exact whole-millisecond duration from 1 through 120,000 ms per call. The concrete
adapter must enforce that timeout around every provider request. Provider diagnostics collapse to
bounded model errors (including failure, timeout, unknown outcome and malformed output);
raw errors, request/response bodies, credentials, and headers must not be
logged or returned. Descriptor validation is syntactic, not proof of provider model identity.
The provider adapter's model_sha256 hashes routed provider/base/model/effort, with settings
revision and credential generation in adapter provenance; it is NOT a weight digest.
It resolves the saved route per job and pins that adapter for the attempt. A queued
job has not yet frozen a model; a changed setting can affect its later dispatch.

`memex-agent` uses `SavedProviderSource` and its translation/summary adapters over
`memex-provider::generation::complete`. They resolve the selected purpose and
scope-bound saved credential, enforce bounded requests/responses and deadlines,
and reduce errors to the bounded model enums. The retired pinned local-model
adapters and credentialless execution authority are not supported interfaces.
Synthetic models remain test inputs, never a production fallback. No model weights
are downloaded or loaded by this provider path.

The production worker leases only provider-authorized work:

```rust
let mut queue = memex_analysis::TranslationQueue::open(&library)?;
while queue.process_next_authorized(&mut approved_model_adapter)? {}
let mut summaries = memex_analysis::SummaryQueue::open(&library)?;
while summaries.process_next_authorized(&mut approved_summary_adapter)? {}
```

Manual failed retry uses `queue.retry(job_id)` and is allowed only when the public job view says
`retryable=true`. The daemon normally uses the bounded persisted automatic retry described above.

The daemon runs capture and post-Store/generated-analysis work on distinct supervised threads. A successful
`commit_capture` atomically records its exact Item/Revision receipt and a pending hook in the durable
capture queue before reporting `succeeded`; the separate worker idempotently enqueues that exact
revision and only then acknowledges the hook. Hook, Analysis, or model delay/failure cannot block,
roll back, or misreport canonical capture work. A crash after Store commit replays the frozen capture
through Store idempotency, and legacy success rows lacking a revision receipt are likewise replayed
when their frozen artifact is complete or failed closed when it is not. The hook worker
still queues both durable Analysis jobs independently of the retired `translation`
config block. Execution requires an authorized job and its configured saved route/
credential; without those, no provider is invoked.
The single post-Store hook is acknowledged only after both idempotent
enqueues succeed. A failing hook is durably deferred for one second before another attempt, so
later hook intents and already-enqueued Analysis work continue; the failed exact Item/Revision intent
remains pending rather than being dropped or converted into capture failure.

Direct loopback intake and owner metadata edits intentionally acknowledge the already-durable Store
write even if immediate queue access fails, avoiding an ambiguous client retry after commit. The same
worker therefore performs a bounded all-sensitivity Store-authoritative reconciliation pass: it pages
current revisions in deterministic owner order, idempotently enqueues both jobs, retains only one page
and one cursor, and repeats completed scans every 30 seconds. Store remains the sole durable item
authority; the reconciler creates no second item catalog. A transient failure leaves the current
revision without a queue row until a later bounded pass repairs it. That repair does
not authorize provider work; unauthorized reconciled queued jobs display not_requested
unconditionally, with no legacy-worker mode switch.

## Classification and output validation

V1 only schedules bounded, strongly classified English article/webpage input up to 256 KiB.
Classification scans that whole bounded input, requires dominant ASCII letters plus several
common-English tokens, recognizes sufficiently dominant Han text as already Chinese, and rejects
ambiguous scripts. Strong login/challenge markers stop before queue/model even when they occur late
in the document. This is a conservative eligibility classifier, not a general language detector.

Model outputs must be non-empty, different from source, proportionate to source length, within
Analysis limits, structurally valid for title or balanced-fence Markdown document, reject wholesale
source copying, and contain a dominant/proportional Han signal. A title and document pair is
prepared against the same still-current revision and published under one Analysis lock/index
update. Both records use `AnalysisKind::Translation`, the corresponding
`TitleTranslation|DocumentTranslation` purpose, source/target language, model ID and SHA-256, frozen
prompt ID/version/SHA-256, immutable translation job ID, adapter provenance, and a content-free
config digest. Any competing record for the same revision/purpose/language family stops application
and the surface fails closed.

## Read/surface contract

The privacy-safe current-revision status seam is:

```rust
memex_analysis::translation_status(&library, item_id, revision_id)
memex_analysis::summary_status(&library, item_id, revision_id)
```

The local surface exposes the same body-free view at:

```text
GET|HEAD /api/v2/items/<item_id>/translation
GET|HEAD /api/v2/items/<item_id>/summary
```

Metadata preview and reader show `等待翻译` for `queued`, `正在翻译` only for `running`, a retry-safe
failure message for a retryable `failed`, and `翻译完成` only when a `succeeded` job also has one valid
title and one valid document Analysis projection whose IDs exactly match the pair committed by that
job. Operational Analysis/index read failure fails generated text closed while preserving source Markdown.
Pending/failed/unavailable state never hides source Markdown and never exposes a partial
translation. There is no percentage. Pre-queue immutable Analysis pairs remain readable only
through the legacy no-job compatibility path when both records have no translation job marker; any
job-owned pair requires the exact succeeded binding above, so an orphaned pre-success pair cannot
become visible after queue loss or interrupted application.

The trusted paired-browser owner RPC returns no Store source body for library reads. It returns
independent summary and translation states. Non-succeeded projections contain null generated text;
`succeeded` contains only exact-current-revision text loaded from the validated Analysis record/pair. The existing
job-scoped capture item read remains separately authorized and unchanged. Summary plus translated title and
document are capped at 240 KiB in aggregate before serialization so the complete encrypted RPC plaintext,
including its envelope, stays below the existing 256 KiB aggregate bound.

Static Dashboard exports freeze the state represented by their rendered HTML. A Dashboard consumer
must rebuild its snapshot after a transition to `succeeded` (and re-render from the same exact
revision/Analysis readback) before Chinese output can appear in that export. It must not infer
success from relay acknowledgement or job state alone. This change does not mutate the checked-in
private generated snapshot or deploy it.

## Privacy and non-authority

- Job/status/enqueue output contains IDs, languages, enum state/reason, attempts, retryability, and
  timestamps only—never title, source/translated body, prompt text, URL, local path, credential, or
  provider diagnostics.
- Model input and frozen output are local operational data; they are not `Debug`/status payloads.
- Store remains the only Item/Revision/Evidence/receipt writer. Analysis records remain generated
  text authority. Relay and Dashboard remain consumers/adapters.
- Provider execution uses separately scoped credentials and the configured CLIProxyAPI
  transport. A loopback proxy may forward to a remote provider; local URL does not
  mean local inference. Neither a capability nor this spec changes disclosure
  authorization, deploys a service, or makes synthetic output production-visible.

### Body identity: derived results survive metadata-only revisions (2026-11)

An item keeps one immutable `item_id`; its title and other metadata may change. Every
owner metadata edit creates a new revision, but derived results are bound to the body,
not the revision. `memex_store::body_identity(revision)` is the SHA-256 over the item
ID, `content_markdown`, `evidence_ids`, attachments and sensitivity; it excludes title,
creators, dates, keywords, tags, groups and provenance. Two revisions on the verified
revision chain with the same body identity are "same body".

Reads resolve each derived result at the current revision first, then at the newest
same-body earlier revision (read-time lineage; no data migration, nothing is rewritten).
The existing integrity checks still run on the resolved record (revision digest of the
record's own revision, prompt SHA, generated-text validation) and the sensitivity ceiling
is that of the current item; only the binding key changed. A body change makes results
of earlier bodies stale exactly as before.

| Derived result | Before | After |
|---|---|---|
| Whole-document translation (body) | revision_id | same body |
| Title translation | revision_id (pair) / title SHA | title text: a changed title shows its title-only translation (or the title itself when already Chinese); never the old title's translation |
| AI minutes / legacy summary | revision_id | same body |
| Insights (highlights, quotes, fact check) | revision_id | same body |
| Metadata enrichment job | revision_id (stale_revision) | same body (not stale) |
| Chapter translation, mapping, paragraph alignment | metadata_view carry | same body |
| AI chapter annotations, owner marks, reading passes, underlines | revision_id | same body |
| Item chat, cover | item_id | unchanged (item_id) |

Queued jobs created for a metadata-only revision whose same-body predecessor already has
a succeeded result are not executed: the worker marks them failed with
`inherited_from_revision` (not retryable, no model request), and new requests reuse the
inherited result instead of enqueueing. A running job survives a title edit and its
result is inherited. After a title edit, only the new title is translated (title-only
job), and only when the item's translation was already authorized.

### AI minutes suggested title (2026-11)

AI minutes prompt version 3 (`ai-minutes-v3.txt`) asks the same single minutes call to
end with one line `建议标题：…` (Chinese first; `中文 / English` allowed for English
sources; at most 40 Han characters requested; no invented facts). The whole reply is
validated and recorded as before. Readers split the last line off: `ai_minutes.text`
never contains it and the additive, read-only `ai_minutes.suggested_title` (absent when
none) carries it. Version-2 minutes stay current, are verified against the v2 prompt and
are never re-run for this field; unfinished version-2 work moves to version 3 on request.

`{"command":"item_title_apply","item_id":…,"expected_revision_id":…,"title":…,
"request_id":…}` is the explicit owner click that writes a title (1–512 characters, no
control characters; request_id 1–128 of `[A-Za-z0-9_-]`). It keeps manual tags, goes
through the same Store CAS as `update_item_metadata` (`stale_item_revision` on drift) and
returns the same receipt. Idempotent: if the current title already equals `title`, nothing
is written and the current revision is returned. Derived results are kept (body identity
above); only the title translation is redone. Capabilities: `ai_minutes_suggested_title_v1`,
`item_title_apply_v1`, and `one_step_model_jobs_v1` (chapter translation and formatting
accept one-step requests, see memex.reader_v1.md).

### Boundaries kept by the 2026-11 amendment

Removing cost gates did not remove: `provider_authorized` (background reconciliation
never sends the library to a model without an owner request — a privacy boundary, not a
cost one); the sensitivity ceiling and Restricted exclusions; credential scope checks;
prompt-injection framing of source text; read-time integrity verification; and bounded
request counts and token limits as runaway guards.
