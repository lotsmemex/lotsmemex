# Book projects v1

Status: implementation in progress; no capability or production activation yet.
Authority: `book-project-pilots-20261108`, Staff decision of 2026-11-08.
This additive contract does not change reader annotations, passes, published_at
sorting, existing reader coordinates, global item types, or paid reader HOLDs.

## Identity and organization

A collection project binds one container item. Volumes are directory nodes, not
books. An article is an ordinary item with an independently stable section ID;
section is internal structure, not a new global item type. A novel chapter keeps
its existing item/revision/side/chapter identity and is not copied into a new item.
Read-only inherited bibliography is provenance-bearing; explicit fields win.

Before any visible ingest, persist the request digest, source identity mappings,
container, volume, section and article identities. Content hashes identify bytes,
not permanent article IDs. Same request with different frozen input conflicts;
different request for an already bound source reconciles existing identities.
Changed bytes are variants or conflicts, never silent replacement of current text.

## Source bundle (implemented preparation slice)

`book prepare-sources --source DIR --out NEW_DIR --encoding gb18030|utf-8
--python EXECUTABLE` has no Library handle. `book verify-sources --bundle DIR
--python EXECUTABLE` validates a self-contained bundle without the source directory.
Successful stdout contains only counts, byte count and input digest. Failures use
`book_source_operation_failed`, never source titles, paths or decode excerpts.

The new directory is private (0700); objects and manifest are 0600. Existing
outputs, including incomplete outputs, are never replaced. An interrupted prepare
may leave objects without a valid manifest; that is not a completed bundle.
Limits: 4096 input files, 100 MiB per file, 512 MiB aggregate. No symlink inputs,
lossy decoding, network or model dependency. Source originals are never modified.

Layout: `sources.json` and `objects/<lowercase-full-sha256>`. The schema-v1 JSON
contains `policy: date-heading-coverage-v1`, `sources`, and `input_sha256` over
canonical sorted-key compact UTF-8 JSON of `sources`. Each source records:

- `source_key`: SHA256 of relative POSIX locator, scoped to this source set;
  this is not the persistent article or project ID.
- `relative_path`, `sha256`, `bytes`, explicit `encoding` (null for attachments).
- `coverage`: ordered contiguous original-encoding byte intervals `[start,end)`
  with interval SHA256 and kind `article_candidate`, `unclassified`, or `attachment`.
- Article candidates also record raw-byte title/date/body boundaries and
  `source_heading_date: {raw, role: unknown}`. This stage does not claim a parsed
  PartialDate or identify writing versus publication date.

Preparation nominates boundaries using standalone parenthesized Chinese year
date lines and their preceding nonblank title line. It does not claim these are
all articles, or that every line in a candidate is article prose. Prefixes and
undated inputs are explicitly unclassified; trailing material stays preserved.
Semantic classification must be completed before project acceptance.

Verification independently recomputes the coverage policy, all object and range
digests, contiguous coverage and reconstructed source digest. Raw byte intervals
are not UTF-8 reader positions. Any later conversion must declare its policy and
bind its mapping to both raw and UTF-8 digests. Moving the complete bundle retains
all source dependencies. Paths and body-bearing manifests stay private, not proof.

## Dates and versions

Volumes sort 1 through 6. Articles sort by source date's possible lower bound,
then year/month/day precision, then stable section ID. Unknown/unparseable dates
sort last by stable ID. Prefaces and other non-article material retain structural
positions. Sorting does not turn a lower bound into a known date. Preserve raw,
PartialDate precision, role=unknown and source binding; do not fill published_date.

Strong normalization only produces duplicate candidates. Preserve distinct raw
variants and all source mappings, including identical-title/different-date pieces.
Record the default reading-variant selection explicitly; never silently replace
an existing item's current revision. The parallel-reading record references two
independent sides and alignment; it is not a third editable body.

## Identity reservation and per-item ingest (implemented Store slice)

Owner-local Rust entry points `book_projects::reserve` and `ingest_part` have no
IPC/gateway exposure. `catalog/book-projects/reservations.json` schema 1 records
immutable ImportPlans, generated project/container/volume/section/item IDs and
request aliases in one durable replacement. Limits: 16 MiB registry, 1024 projects,
65536 requests, 4096 articles/project, 64 volumes/project. SHA fields here use the
Store `sha256:` prefix, unlike bare SHA fields in the portable source bundle.

ImportPlan freezes source identity, source-bundle digest, project sensitivity,
volume count, and container/article Store ingest keys and input digests. The full
capture timestamp must already be frozen when computing those input digests.
Requests are scoped to the owner-local Library. Same source identity or bundle
digest with an unequal plan returns `book_project_input_conflict`; identical plans
under a new request alias reuse all generated identities. Reservation creates no
item or tree. Per-item ingest accepts only the exact reserved input, uses the
reserved item ID with the existing ordinary journal, and refuses altered inputs,
current revisions differing from the original receipt, or trashed replay targets.

Errors include `invalid_book_project_plan`, `invalid_book_project_input`,
`invalid_book_project_source_key`, `invalid_book_project_request`,
`invalid_book_project_state`, `book_project_limit`, `book_project_input_conflict`,
`book_project_identity_conflict`, `book_project_item_conflict`,
`book_project_part_not_found`, and the existing non-disclosing
`library_resource_not_found`. These APIs are an internal preparation slice, not
finished navigation DTOs or permission to activate a project in production.

## Final publication and recovery (required, not yet implemented)

Use Store writer locking, recovery, generation and durable directory sync. Reserve
identities durably before ingest. Each ordinary item commits with its own durable
receipt; publish the final project manifest with one CAS only after verifying all
references, exact revisions, permissions and non-trash status. Partial ordinary
items may be visible before organization publication; this is not batch atomicity.

Progress distinguishes ingested, pending, organization-unpublished and conflict.
Reconciliation consults receipts first, never overwrites intervening edits or
reactivates trashed items. Organization undo only removes request-added relations
or manifest changes still matching their recorded state. It never deletes items,
Evidence, later user relations or restores an old whole-groups snapshot. Replaying
an undo receipt does not undo again.

Project/source/item/section current and historical permissions all apply. Tree,
counts, pagination, sorting, search and receipts cannot reveal hidden references.
Relations do not widen access. Stale/damaged references fail explicitly, rather
than silently selecting newer revisions. Public DTOs, manifest limits and cursor
binding must be specified and tested before advertising navigation capability.

## Local reads and compatibility (required, not yet implemented)

Chapter projections are rebuildable and bound to canonical revision/content hashes;
they never replace canonical or Evidence. Preserve old UTF-8 anchors, manual marks,
annotations and conversations. Warm chapter reads must not parse the full revision
body; cold projection construction costs are reported separately. File names or
mtime alone do not prove integrity. Existing anti-tamper strength must be retained.

Legacy clients may read ordinary items and existing reader endpoints without new
project navigation. No new project data is activated for an incompatible running
daemon. No service switch, push or release is authorized by this contract. Before
activation, report exact compatibility and rollback effects and coordinate with
the frontend owner. Paid batch/map HOLD and remote node-delete unavailability stay.

Acceptance still includes staged interruption/restart, conflicting retries, complete
source coverage, variants, hidden children, concurrent edits/undo, relocated backup,
both real pilots and measured cold/warm IO, latency and memory. Source-bundle tests
alone do not satisfy project, reader or real-ingest acceptance.
