# Native import and organization CLI v1

Owner: C-Staff. Local implementation, not a published release. This contract
implements the user's requirement that import/classification/organization are
reusable CLI capabilities; models judge bounded proposals instead of performing
bulk file operations or carrying every item's body through their context.

## Modules and commands

`memex import --help` exposes independent operations:

| Command | Input | Result / write boundary |
| --- | --- | --- |
| `prepare-zotero` | `--snapshot`, `--storage`, `--group`, `--out`, `--python` | Frozen source bundle; no Library mutation |
| `supplement-zotero` | `--bundle`, `--python` | Separate final manifest; retains the first conversion attempt |
| `items` | `--plan`, `--library`, `--receipts` | Store bulk ingestion, up to 500 records per API call with recoverable per-item identities |
| `export-evernote` | `--library`, `--receipts`, `--out` | Private local export of existing visible receipt-linked items; never reimports |
| `plan-topics` | `--input`, `--out`, `--root-name`, `--namespace`, `--python` | Proposed editable folders, provenance and compact decision input |
| `plan-zotero-groups` | `--hierarchy`, `--receipts`, `--out`, `--python` | Folder/member plan bound to imported revisions |
| `groups` | `--plan`, `--library` | Atomically add one new root/tree; never overwrite existing groups |

Python adapters are embedded in the CLI binary, not resolved from a checkout.
Supply the Python environment explicitly. Zotero preparation requires
`markitdown[pdf]==0.1.9`, `pdfplumber`, `beautifulsoup4`, `markdownify`; local topic
planning requires `jieba`, `numpy`, `scikit-learn`. Supplemental DOCX/PDF extraction
uses installed Pandoc/Poppler. There is no automatic installation, provider call,
credential access, or fetch. CLI planning is network-denied on macOS; other OS
sandbox support is not yet implemented and fails closed. These are packaging
limitations, not claims of completed cross-platform distribution.

## Frozen source and native items

Use a consistent SQLite backup API snapshot, not a hot copy of a running Zotero
database. Group/library selection is explicit. Deleted source items are excluded.
Preserve source keys, raw metadata, notes, attachment bytes and original hierarchy.
Local original files become Evidence; converted Markdown is the reading projection.
Notes and attachments are native independent items, not read-only remote mirrors.
The source namespace is group ID + Zotero item key, never title or filename.

For query facets only, exact native locators of the form
`urn:zotero:group:<digits>:item:<alphanumeric-key>` share the source
`urn:zotero:group:<digits>`. The complete canonical locator remains unchanged.
HTTP origins and unrelated opaque identifiers retain their existing semantics.
This avoids one source facet per locally imported record overflowing the bounded
query response; it does not merge items, groups or source Evidence.

Every object has a SHA-256 binding. Conflicting frozen input refuses replay.
Items use the normal Store journal, writer lock and generation token. A durable
per-source-item intent records local import-attempt time, not Zotero `dateAdded`;
recovery/replay retains it. This is not a claim that the timestamp was sampled
inside the successful Store commit. Original source dates remain in raw Evidence.
Single-line metadata projections normalize whitespace; Markdown/raw Evidence do
not. Source fields exceeding supported structured limits stay in raw Evidence.

Clarification for publication-date support: the current Zotero adapter also
projects its own `fields.date` into legacy `metadata.published_date` when within
supported limits. Raw retention does not mean all canonical dates are absent.
The active increment in `memex.bibliographic_metadata_v1.md` is read-only
source-bound multipart projection and sorting, not backfill. It preserves the
stored string and raw Evidence; a source sort prefix alone is not an exact date.
Missing-date Evidence coverage and any later preview/confirmed backfill are
separate. Do not rerun import, borrow parent dates, change this added_at policy,
or authorize new lookup through this read-path repair.

Export receipts append and fsync after each batch under a single-writer lock;
canonical Store receipts are already durable before export begins. Resume
validates completed JSONL rows, repairs only a torn non-newline-terminated tail,
and reconstructs its receipt via Store replay. Completed corrupt rows fail closed.
Intent files publish atomically without overwrite and sync their contents; their
directory is synced once per batch before Store ingestion, including on retry
after an fsync error. Its parent is synced before preparing the batch.
Restarting with identical frozen input uses Store identity replay, not new IDs.
An already trashed/invisible result is
not reactivated or included in the group. Keep the bundle's durable path: original
Evidence can refer to retained bundle objects; copying Library alone is not a
portable backup of all original sources. No automatic source cleanup is allowed.

## Shared 500-item Store batch API

`memex_store::ingest_batch(library, Vec<(VerifiedCapture, ItemDraft)>)` accepts
0–500 already-verified records from trusted adapters and returns one receipt per
input, in input order. Zotero `import items` uses this path. Chrome can reuse this
Store primitive after its own authenticated admission/translation; this does not
claim a deployed Chrome bulk HTTP endpoint or change its existing request limits.
The CLI checks object sizes before buffering them and flushes at either 500 records
or 64 MiB of retained payload (including the draft/Evidence Markdown copies).
A single larger record runs alone, not excluded or truncated. Trusted API callers
must likewise bound payload bytes; 500 is an item limit, not a memory-size promise.

Source-identity replay is not whole-library content deduplication: an identical
identity/input returns its existing item; a changed immutable input is a conflict.
Equal text under distinct identities is retained as distinct items. Multiple
Zotero collections refer to the same imported item ID, never duplicate bodies.
Cross-source duplicate detection/merge remains a separate operation.

The request is not one 500-item atomic transaction. Internally, bounded windows
share one identity-catalog load/publication and at most four workers prepare and
install independent item journals. The writer lock is released between windows
so other writers and reader snapshots can interleave. All workers are joined
before lock release, including failures. File/directory durability barriers and
full v1 journal recovery validation remain intact; no new disk schema is required.

A failed call may have committed some items. Retry the same frozen request; do not
invent new capture identities or assume zero writes on error. Old readers recover
prepared journals using their existing startup/recovery path. Existing items,
trash, permissions, Evidence and original collection mappings are not rewritten.
Batching does not grant any model, network or classification permission.

Conversion accounting distinguishes notes, PDF/DOCX/text/HTML, metadata-only,
linked-only, missing original, unsupported format, OCR required, invalid glyphs,
page limits and failures. A placeholder is never counted as successful full text.
No linked URL is silently fetched. Complex tables/images require separate fidelity
verification; passing text extraction is not proof of lossless document conversion.

## Hierarchy, themes and safe application

Preserve original collections including nested and empty folders. Explicit source
memberships remain recorded. Notes/attachments additionally follow the parent's
collections for reading; this derived membership is separately attributed in the
plan provenance, not falsely described as an original Zotero membership.

Topic planning operates locally on item/revision-bound input. TF-IDF/KMeans proposes
folders; original tags remain separate. These editable native folders are not the
unimplemented `topic_cluster` Analysis query API. The namespace separates different
organization plans even with identical input. Suggested labels are not model claims.

`groups` validates the complete tree, visibility and every expected item revision
under the Store lock before one durable state replacement. Stale revisions, orphan
folders, cycles, existing IDs and hidden members fail without partial membership
publication. Exact request replay returns the prior receipt without undoing later
owner renames/removals. Initial root creation is not synchronization or relocation;
ordinary subsequent group edits retain the existing single-op/CAS contract.

## Model decision boundary

`decision-input.json` contains the plan hash and per-cluster ID, suggested label,
keywords and item count. Full texts and per-item membership lists are omitted.
The complete deterministic mapping stays local in `groups.json`/`analysis.json`.
This decision input remains private; generation does not authorize transmission.
No model is called in this implementation. A future configured-model adapter must
honor selected data scope, budget and existing provider routes, bind its structured
decision to the reviewed plan, and use the same Store validator/executor. The model
cannot bypass visibility, revision checks or Evidence preservation. Do not describe
this adapter, arbitrary moves or reversible batch undo as already implemented.

## Acceptance boundary

Verify source accounting, native item/body/Evidence hashes, stable replay IDs,
tree/membership counts and real daemon query/readback separately. Full Library
backup precedes real application; SQLite analysis databases use SQLite backup API.
Current phone UI has no group tree: item search/open and Mac daemon group queries
can be accepted, but phone directory navigation cannot be claimed until supplied.
