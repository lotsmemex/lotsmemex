# memex.library_query — local trial

## Local revision-bound images (2026-11-03 candidate)

Owner UDS `image_read` accepts `{item_id,expected_revision_id,target,offset,max_sensitivity}`.
Gateway `memex.library.image.read` accepts the same fields except sensitivity,
which comes only from the current read grant. Success adds `image` on UDS; the
gateway returns that object directly: `{item_id,revision_id,target,media_type,
sha256,total_bytes,offset,next_offset,data_base64}`. Chunks are at most 96KiB
before base64, total images at most 64MiB. `next_offset` is null only at EOF.
Clients verify every identity/range and the final SHA-256 before rendering.

Targets are exactly `attachments/[sha256-]<lowercase64hex>.(png|jpg|jpeg|gif|webp)`.
Only that revision's authorized Evidence can resolve a target, either a verified
Store attachment or the retained offline-import `Imports/<batch>/raw/objects/<hash>`
object. No arbitrary filesystem path, URL, SVG or remote fetch is accepted.
No canonical data migration/write is performed. Old daemons reject the additive
command; clients show an unavailable image, never fall back to another database.

The Store keeps at most128MiB/32 immutable verified image buffers, with60s idle
expiry evaluated on access. Every chunk rechecks current revision, global trash,
item/Evidence ceiling, binding and symlink-free path. Stat changes cause rehash;
cache hits return the already verified immutable bytes, not unchecked file bytes.
Per-library trash retains the existing direct-item-read semantics (not revocation).
Gateway responses share a512KiB/1010ms encrypted-wire budget and check grant/trust
again after pacing, at send time. Images never get a public HTTP route.

Mac renders bounded rasters (16million pixels, first animation frame); mobile uses
ephemeral blob URLs and releases them offscreen/on navigation. Neither client
persists image cache. Missing/unavailable images remain explicit. External images
not present in the import remain links and are not automatically downloaded.

## Additive title and highlight contracts (2026-11-03 candidate)

`sort: "added_at"` orders the canonical MemeX entry time, also exposed as an
optional item field. It includes verified legacy backfill records; missing times
remain unknown and sort last, never substituted with source dates or now.
Import commit times and capture entry times retain their existing Store policy.
CLI accepts `--sort added_at`; mobile uses the same query field. This alone does
not distinguish captures from imports or automatically change the UI's default.

`memex.session.info.query_sorts` lists the gateway's accepted sort values:
`["title", "source", "captured_at", "added_at", "item_type"]` at the earlier
baseline. The approved `published_at` extension below is additive and requires
actual daemon/gateway capability support; this document alone does not enable it.

`query.origin` is optional: `"capture" | "import" | null`. Omitted/null includes
all items, including unknown origins. Classification reads the immutable initial
Evidence → CaptureRecord → IngestReceipt relationship: offline/legacy normalized
imports are `import`; safe HTTP, public article API and browser snapshots are
`capture`. Unknown capture methods remain unclassified, never guessed from a URL,
source name, folder or title. Metadata edits and file reconversion do not change
the creation origin. The filter applies before totals, facets, sorting and paging;
changing it invalidates an old query view. Query items add optional `origin`.
CLI accepts `--origin capture|import`. No canonical data is rewritten.

The lightweight owner `agent_version` response adds `query_origin: true` on
supporting daemons (older/missing means false). Gateway `session.info.query_origin`
is `"available"` only after this daemon signal, otherwise `"unavailable"`.
No model credential/status lookup or generation is needed for this discovery.

`memex.library.groups.tree` maps to existing `group_tree_read`, with the session
ceiling. `memex.session.info.versions` contains `gateway`, `web_bundle`, and
nullable `agent: {version,binary_sha256}`. Binary hash is the actual executable's
SHA-256 prefix, null while its nonblocking calculation is pending or unreadable.

This section describes candidate code, not a deployment receipt. Query items add
optional `translated_title` and `title_source` (`title_only|with_context`). The
source `title` is unchanged; readers display the translated title first, a visible
separator, then the original. A metadata-only revision preserves a title-only
translation while a changed title invalidates it. A validated title from explicit
document translation takes precedence, including after the job clears its frozen
text and commits its Analysis records.

New captures schedule one title-only request, without automatically translating
the body. Store/native Zotero and Evernote import and background reconciliation
do not authorize title generation. Capture replay cannot retry a failed or
uncertain provider outcome. Owner-only `title_translation_backfill` accepts
`{batch_size,dry_run}` (1–5000); first preview counts and token estimates, then
explicitly execute the selected batch. No mobile backfill method is exposed.
Failed rows are retryable only through that explicit owner action. This release
does not run a real-library backfill as a test.

Highlights use prompt/job version **2**, while quotes and fact checks retain v1.
Old highlight results are not returned as v2; old queued jobs and metadata carry
cannot silently upgrade them. A new explicit highlight request is necessary.
The existing `block_index`, `quote`, `style`, `label` remain, with additive
`original` and `translation` spans: `{text,block,start,end}` or null. Offsets are
UTF-8 byte ranges within that side's `split_blocks(...)[block].text`, not character
indices or offsets in the other language. English terms have 1–4 words, Chinese
terms 2–8 characters. Every accepted term matches its block exactly; invalid,
overlong or overlapping spans are dropped, never truncated or guessed.

Translated blocks must pair one-to-one with original blocks, including the
passthrough layout. Otherwise highlights honestly remain `original_only`.
`sides` is `original_only|original_and_translation` for succeeded highlights.
Changed or missing translated coordinate space returns
`unavailable / insight_translation_changed`; no old offsets may be applied to
new text. Metadata-only copied translation records with identical text remain
compatible. Providers run only for explicit requests; all acceptance tests use
synthetic fixtures and stub providers.

Owner-only UDS query, implemented by `memex-library` (Store synchronization)
and `memex-query` (in-memory derived index). Neither module depends on a UI,
provider, HTTP gateway or model. Canonical files remain authoritative.

## Request and response

Approved date sort: `sort:"published_at"` retains its wire name but now uses the
publication-or-original-acquisition policy in memex.bibliographic_metadata_v1.md.
Resolve own publication typed first, otherwise parsed legacy with source-bound
Zotero multipart. AboutHealth child notes/attachments prefer their verified owning
paper's current publication date, explicitly `inherited_from_parent`; no parent
acquisition inheritance. If no usable publication is available, use the item's
verified original acquisition date and label it as such. For Zotero this is its own
source Evidence items.dateAdded, NOT the current import-generated captured_at or
added_at. Evernote/native captures use verified original clipping/capture identity.
A present unparsed typed field does not fall back to a more precise legacy value,
but may use the separately labelled acquisition fallback. Never mutate publication
metadata to implement fallback. Do not use a multipart prefix's
sort key to bypass suffix/precision validation. Keep corrupt canonical metadata
on the existing validation/error path; do not turn integrity failure into an
unknown date. This query slice is read-only projection/sorting. The renewed
AboutHealth/Evernote batch implementation scope and evidence rules live in
memex.bibliographic_metadata_v1.md; real backfill application still requires
human confirmation of its exact preview/plan and is not enabled by this sort.

Order valid normalized values lexicographically with a shorter matching prefix
first: ascending `2023-12-31 < 2024 < 2024-01 < 2024-01-01 < 2024-02`.
Precision follows the exact year/month/day representation, not a fabricated
January 1/day 1. This is a deterministic partial-date ordering convention, not
proof that a year-only event occurred before a known day in that year. Descending
reverses this known-date ordering. Original acquisition fallback is projected to
day precision using the shared server policy timezone (currently Asia/Taipei),
not the client's locale. Publication and acquisition keys interleave chronologically;
there is no publications-first bucket. Only items with neither usable publication
nor original acquisition remain last in BOTH directions.

Equal effective keys and the entirely unknown bucket use verified
effective_capture_at DESC (missing after known), then item_id ASC. Secondary
directions stay fixed for published_at ASC. Compare acquisition instants with the
existing timezone/nanosecond semantics. The latest explicit exclusion of MemeX
import time removes the former added_at tertiary key from this sort; other sort
modes remain unchanged. Preserve filters,
authorization, facets and view/CAS semantics; changes to any visible effective
sort key stale the view, including parent date/relation/permission/trash dependencies
and date-policy timezone. Hidden-only changes do not notify lower views; never
leak a hidden parent's date or identity through a visible child. Persisted inheritance
snapshots are audited data, not permission to trust stale/hidden parent values.

Supporting daemons advertise `published_at` in additive
`agent_version.query_sorts` together with
`query_date_policy:"publication_or_capture_v1"`. Gateway session.info forwards
both only when its validator/transport and connected daemon support the policy.
Clients require both signals for the revised default and effective-date labels;
a legacy published_at advertisement alone must not masquerade as the new policy.
Missing signal
means this new sort is unavailable, not permission to silently use added_at/title.
Preserve existing sort negotiation on old daemons. Clients enable the menu from
the effective capability, not an installed app version. CLI support and both
directions/pagination must agree with the same backend ordering. Sorting can ship
before any backfill apply; it neither migrates metadata nor authorizes writes.
Test year/month/day prefixes, mixed years, duplicate dates, null/unparsed in both
directions, asymmetric original-capture keys (including nulls and equivalent
offsets), typed/legacy equivalence, scope filters and stale pages. Include an
unpublished older capture imported later than a newer published paper, a newer
capture preceding an older publication in DESC, original dateAdded differing
from import captured_at, parent date/visibility changes and timezone midnight.

Once the effective capability is available, library lists including Inbox default
to published_at DESC instead of capture-time ordering. Clients send the explicit
sort/direction and reset pagination/view when the default changes. This changes
the default, not an explicit user-selected alternative sort; do not misclassify
an automatically persisted old default as a deliberate user choice. An old daemon
retains its supported default and shows publication sorting unavailable, never a
publication label backed by capture sorting. No default change is a metadata write.

List rows and item details make effective_date the primary date with its actual
precision and effective_date_basis: published displays “发表日期”,
inherited_from_parent displays “发表日期 · 随父条目”, captured displays “采集日期”
(Zotero detail explains “加入 Zotero 的时间”), unknown displays “发表日期未知 ·
采集日期未记录”. Detail may still explain unknown publication when capture is used.
Query and metadata return the same effective_date PartialDate/null,
effective_date_basis, effective_capture_at and effective_capture_origin; original
published_date remains publication-only. No client-only reparsing, per-row full-body
reads, import-time fallback or raw wording presented as a precise date.

Approved additive Inbox query (implementation/deployment separately gated):
`query.unfiled` is a boolean, omitted means false; true selects query-visible
items with no currently visible active group/folder membership, before every
page, total and facet. It uses the current ceiling's projection, not global
memberships, and never overrides item-trash or sensitivity exclusions. Reject
true with a group/cluster selector or `include_descendants:true`; bind this flag
and effective membership state to the view. Capability is
`groups_v1.query_unfiled:true`; unsupported clients/daemons must not silently
substitute All or page-local filtering. The authoritative details, import
preservation and node-deletion distinction are in
`memex.groups_v1.md`, “Sidebar Inbox and recoverable node deletion”.

```json
{"command":"library_query","query":{"text":"机器","source":null,"item_type":null,"sort":"title","descending":false},"max_sensitivity":"public","offset":0,"limit":50,"expected_view":null}
```

Successful responses add `library_query` to the existing `ok=true` envelope:

- `schema_version: 1`, opaque `generation` and `view` strings;
- `total`, `offset`, `next_offset` (null at the end), `items`;
- `sources` and `item_types`: `{value,count}` arrays over the **entire authorized,
  filtered result**, not just the returned page. These are not disjunctive facets.
- Each item has `item_id`, `revision_id`, `title`, `source`, `item_type`,
  `captured_at` and `sensitivity`; no body or generated translation is included.

`source` groups web URLs by origin (scheme, host and non-default port); opaque
non-URL source identifiers retain their original value. It is not the source
profile ID or a translated display label. `captured_at` currently matches the
existing Store projection's translate-provenance timestamp, or null; it is not
claimed to be a separately verified retrieval timestamp.

- `text` is a literal Unicode-lowercased substring of title **or** original
  Markdown, never a phrase spanning both fields. Empty text means browse.
  No query operators, stemming, simplified/traditional conversion or ranking.
- Exact optional `source`/`item_type` filters; sort is `title`, `source`,
  `captured_at`, `added_at`, `item_type`, or capability-gated `published_at` above.
  String order is byte order, not pinyin collation.
  Capture/inclusion timestamps compare timezone-normalized instants including
  nanoseconds; publication dates use the partial-date convention above. Null dates
  sort last in either direction. Other sorts retain item_id tie-breaking;
  published_at uses the captured_at/added_at/item_id hierarchy specified above.
- Window limit is 1–200. A first request uses offset zero with no view; subsequent
  random offsets require that view. A view binds generation, all query options
  and effective sensitivity ceiling. Visible-set changes, daemon restart and scope/query
  changes reject the old view with `stale_query_view`; refresh from offset zero
  before requesting more windows. Do not mix rows from different views.
  Store-backed queries may share one in-memory contentless full-text index per
  library, retaining at most four lightweight ceiling-specific visibility/view
  states rather than four copies of the full-text postings and stored text.
  Above-ceiling-only edits do not rotate a lower-ceiling generation/view or
  invalidate its page. All canonical data is still verified before indexing;
  storage-integrity failures fail closed, not as a timing-isolation guarantee.
- Ceiling is required and only narrows a local owner's read. This is **not a
  remote API**: the [synthetic mobile gateway](../remote/README.md#scoped-mobile-gateway--synthetic-local-slice)
  imposes session scope and ceiling rather than trusting the client. It is not
  installed/deployed or authorized for the real library. Do not forward source
  text to a provider.

Error responses and all old commands **omit** `library_query` to preserve strict
old decoders. Error codes include `invalid_query_window`, `stale_query_view`,
`library_query_failed` (sanitized Store/index failure) and `result_too_large`
(the existing 256 KiB serialized UDS envelope bound, no silent truncation).
Malformed requests retain the existing sanitized error behavior. Unsupported
commands on an old daemon are unavailable, not permission to bypass it.

## CLI

```sh
memex library-query --max-sensitivity public --query '机器' --limit 50
memex library-query --max-sensitivity public --query '机器' --offset 50 --view '<view>'
```

Optional `--source`, `--item-type`, `--sort`, `--descending` map directly to the
request. `--socket <path>` selects an explicitly isolated daemon; omission uses
the existing standard socket. This command does not start/install a daemon.
It prints the normal daemon response envelope and exits nonzero on errors.
Existing `recall`, `show`, `context-read`, and `library_list` remain unchanged.

## Shared in-memory index storage boundary

This memory-reduction increment permits a contentless FTS5 index and shared
postings, NOT a new disk-backed query cache. Canonical Store files remain truth;
do not alter canonical data, body retention, permission labels or wire fields.
Keep the existing literal Unicode-lowercased substring semantics, per-scalar
encoding, title/body field separation, deterministic sort and absence of ranking.
Contentless must not weaken positional phrase matching or the existing atomic
replace/upsert/delete behavior. Removed, replaced and trashed items must not leave
searchable old terms; failed synchronization invalidates all affected query views
until verified rebuild. Do not retain a second full-body copy merely to emulate
contentful FTS reads or to hide unsupported contentless delete/update operations.

Every query requires an effective ceiling from current authority. Enforce item
sensitivity and applicable trash/selection visibility in the database selection
before sorting, LIMIT/OFFSET, totals and every facet; post-filtering a privileged
page is insufficient. Browsing with empty text has the same checks. Group existence,
group names, hierarchy/closure, membership, item group_ids and group facets also
retain ceiling-specific authorization. A visible item in a hidden group does not
make that group visible; hidden and nonexistent group selectors remain
indistinguishable. A shared documents table plus a sensitivity predicate alone
does not authorize exposing the global groups/memberships projection.

Physical-index updates and public view generations are distinct. Retain per-ceiling
visible-set/group fingerprints and generation/view semantics: a hidden-only edit
must not alter a lower-ceiling page, counts, facets, group tree, generation or view.
Visible mutations and scope changes still reject stale views. A global FTS rebuild
must not be used as a hidden-write notification. Keep the existing Store snapshot,
complete canonical verification and integrity-failure behavior; no newly granted
timing-side-channel or concurrent-mutation guarantee is implied by this change.

No query database, encoded token dump, spill file, WAL/journal or serialized full-text
cache may be newly persisted by this increment, including under Runtime/cache.
Keep SQLite temporary work memory-backed. Unicode hex tokens and positional
postings are sensitive derived content, not anonymized text or encryption;
contentless removes stored source columns, not information carried by the index.
0600 and exclusion from application backups do not establish absence from system
backups or secure erasure. The existing operating-system swap/crash-dump policy is
not changed or certified by this application-level no-new-persistence boundary.
Disk-backed indexing requires separate approval covering the persistent sensitive
derivative, location, lifecycle and recovery/deletion policy; general repair/release
permission does not override the current explicit no-persistent-index contract.

Use bounded cache/transient working sets without weakening search completeness.
SQLite cache_size is not a total memory/RSS bound, especially for an in-memory
database whose index pages still need storage. Account for transient source/token
buffers, verified snapshots and concurrent rebuild work, not only steady FTS size.
Do not promise a specific reduction before measurement, raise query timeouts, spill
to disk, exclude records or silently disable authorized ceiling queries to meet a
memory target. If a safe memory budget cannot be met, report the measured limit
before proposing the separately authorized disk-backed option.

An independently verified steady-state reduction may be released before cold-build
working-set refactoring. Report the measured peak separately and retain full-corpus
transient buffering as an unfinished follow-up; do not claim a total memory bound
or completion of the bounded-working-set requirement. Observed OOM, correctness,
permission or query-deadline regressions are not waived by steady-state savings.
A later bounded-build implementation must preserve the consistent Store snapshot,
complete validation, atomic visibility/generation publication and fail-closed
recovery; batching must never expose a partially verified searchable view. Freeze
the accepted release candidate before handing its source files to the next writer.

Synthetic acceptance compares all ceilings, empty/text queries, exact substring
and title/body boundaries, updates/deletes/rebuilds, pagination/totals/facets and
group descendants with existing behavior. Include a visible item in a hidden group,
hidden-only mutations preserving the lower view, visible changes staling it, and
failed canonical verification invalidating warm caches. Verify no SQLite query
files/spills appear. Measure daemon RSS before/after at the same corpus and exercised
ceilings, warm steady state and rebuild peak, alongside query latency/correctness;
fixture-only measurements do not establish real-library memory savings. This
freeze does not authorize new private-body disclosure, model calls or real-data
mutation; existing scoped local read-only acceptance authorization is unchanged.

## Freshness, cost and rollback

Freshness follows the existing Store-writer model, not continuous detection of
out-of-band filesystem tampering. Every query takes the consistent Store snapshot
lock, including existing journal recovery, and reads catalog/generation. All
supported Store writers must advance that marker before releasing the writer lock.
An unchanged present generation reuses verified item and view state. On changed
or absent generation, enumerate item head stamps; unchanged stamps may reuse
previously verified items, while changed/new items undergo the existing complete
canonical/Evidence validation before entering the projection. Initial construction
and rebuild after invalidation validate all indexed items, not just the result page.

The memory-sharing increment preserves this freshness model; it neither introduces
nor certifies detection of revision/Evidence edits that bypass the Store writer
and leave the generation/head stamps unchanged. Such edits can leave a warm index
serving its previously verified projection. Advancing generation alone does not
force rereading an unchanged head stamp. External-tamper detection is a separate
unfinished integrity-hardening task, not a requirement to reread the whole corpus
on every query in this increment. Any supported writer that fails to invalidate
its affected cached state is a correctness defect, not this exception.

When snapshot acquisition, synchronization or canonical verification detects an
error, clear all cached ceiling views, items and generation, including navigation;
fail with the sanitized error rather than serving the old projection. Recovery
requires successful verified reconstruction. "Complete verification" elsewhere in
this contract describes validation on construction/changed-item reads, not a claim
that every warm query hashes all canonical files. Permission, group, trash and
stale-view checks remain mandatory under this Store-writer freshness model.

Committed journals with distinct item IDs now recover in at most four scoped
workers while the caller holds the same Store lock. Every worker still validates
the complete journal graph and executes the cleanup durability barrier, even
when staging is absent. Any Prepared journal retains ordered serial recovery.
Healthy private, single-link derived projections are compared byte-for-byte and
kept; damaged or shared projections are replaced from verified canonical data.
The catalog is read once per unchanged recovery pass, not once per item.

The following is historical activation evidence, not a description of current
query scheduling or a completed large-library performance acceptance: full
canonical scans/hash checks scale with library size, and the daemon at that
activation handled UDS requests serially. Large scans may exceed the existing
five-second client timeout or delay other UDS requests. A local 1222-item check
passed within the unchanged timeout after this repair; a launchd check returned
4957/3923/1759 ms for three queries. Earlier attempts timed out. Those samples do
not establish cold-start, concurrent availability or a latency guarantee; GUI
acceptance and runtime evidence live in the private local activation receipts.
Do not fix latency by silently raising timeouts or weakening guards.

No persistent query index, new canonical format, network listener, provider or
data migration is introduced. Restart discards/rebuilds the derived index;
rollback stops using the new command and leaves all canonical records intact.
Store recovery may repair derived files and create locks; this is not a promise
of zero filesystem writes. No existing installed service is changed by tests.

## Publication day and duration summary — additive local implementation (2026-11-09)

`library_list.library_items[]` and `library_query.library_query.items[]` add
`published_at: string | null` and `duration_seconds: integer | null`.
The gateway `memex.library.query` returns that query page directly as `result`;
`remote/mac/library-router.js` does not project away these fields and needs no edit.
This is a DTO increment, not a new sort capability or a deployment receipt.

`published_at` is an actual publication day in `YYYY-MM-DD`, derived from the
current revision's `metadata.published_partial_date` first, otherwise the existing
unambiguous legacy `metadata.published_date` parser. Year/month/unparsed/missing
dates yield explicit JSON null; a typed partial date suppresses legacy fallback.
RFC3339 legacy dates retain their source calendar day, not the UTC-shifted day.
Neither `captured_at`, `added_at`, inherited dates nor `effective_date` are fallback
values for this field. Existing effective-date policy and sorting stay unchanged.

`duration_seconds` unwraps the current revision's sourced duration as positive
integer seconds (u32); missing yields explicit null, never zero or an estimate.
For example a synthetic episode can return
`{"published_at":"2026-11-06","duration_seconds":3723}`.
Provenance remains in canonical metadata; these summaries do not replace it.

The query retains its authenticated sensitivity ceiling and Store generation /
head-stamp cache invalidation. RSS metadata fills create a new current revision:
the next warm query sees its date/duration without rebuilding historical baselines.
Hidden-only changes do not change a lower-ceiling page or view token. The legacy
owner-only `library_list` continues listing all owner sensitivities; it is not a
new low-privilege remote endpoint. No authorization or routing policy changes.

Older payloads may omit these additions; clients treat omitted and null as unknown
and must tolerate additive response fields. Existing request shapes and metadata
schema version remain unchanged. No persisted schema migration or root `item.json` /
`index.json` rewrite is required. Rollback removes read projections only; RSS
source evidence, immutable revisions and current pointers remain intact.
