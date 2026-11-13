# Bibliographic metadata v1 — design contract draft 4

Status: md1 and draft1 approved; scoped lookup approval retained. This revision
supersedes draft3's capture-date restriction with the user's explicit definition:
for capture-origin items, collection time IS their MemeX inclusion time.
No new endpoint, schema migration, provider request or CiteSpace implementation
is delivered by this document. Approval was relayed by the frontend owner from
the user decision recorded in its DECISIONS.md, not independently read here.
Owner: C-Staff writes this spec. The earlier frontend/backend single-writer window
is historical; the current publication-date increment below assigns backend work
to B and clients to F. No product write set is reclaimed by C-Staff.
Source: 用户要求“重点是元数据，比如书籍的 ISBN，文献的 DOI 等等，日期”，
后续引入类似 CiteSpace 的可视化分析。用户已认可 md1 与元数据合同并回复“批准”。

## Existing authority and version boundary

At the original draft1 baseline, `memex-core::ItemMetadata` already had sourced `creators`, `identifiers`
(doi/isbn/pmid), string `published_date`, language, site name, abstract and tags.
Creators currently contain only display name and role; `ItemMetadataUpdate` only
edits title/manual tags with `expected_revision_id`. Do not send the new fields
to that strict legacy writer or claim that identifiers/dates never existed.

This contract extends that same canonical revision metadata, not a second database.
The proposed read/edit payload advertises `metadata_schema_version:2`; capability
negotiation must precede use. Contract v1 is the document version, not canonical
`SCHEMA_VERSION`. Persisted schema/version changes require a separate implementation
and migration plan; do not merely bump the global version for unrelated records.
Old readers/writers keep their current wire shape. New clients lacking capability
show unsupported fields read-only/unavailable rather than trying guessed routes.

Bibliographic values retain `Sourced<T> = {value, provenance}`; provenance keeps
`origin`, `provider_id`, `provider_version`, and may add authorized `source_refs`
to immutable import/lookup evidence. Client edits cannot forge source provenance.
Lookup acceptance records both the source candidate and the owner's selection.
Original Evidence and old revisions remain unchanged. JSON examples below show
inner values unless the envelope is explicitly shown.

## Module boundaries and parallel ownership

User instruction: “注意继续遵守模块化开发的原则，方便未来多 Agent 组并发推进。”
Separate responsibilities/modules and commands, without mandating one crate per
function or a new service framework. This spec uses these boundaries:

| Module | Owns | Does not own |
| --- | --- | --- |
| metadata | typed fields, date/identifier validation, local duplicate detection, revision/CAS edits | external HTTP, folder topology, historical added_at mutation |
| lookup | selected identifier requests, provider parsing, immutable field previews | direct canonical writes, a second identifier normalizer, group/import actions |
| groups | directory tree, memberships, recursive filters/counts; groups_v1 is its contract | bibliography edits, provider calls, redefining added_at |
| added_at backfill | source-kind date policy, deterministic preview/apply and migration receipts | reimport, source timestamp edits, ordinary editable metadata fields |

Lookup applies accepted candidates through metadata's existing CAS writer. New
capture/import creation uses the same added_at policy as backfill, through the
Store's transaction/journal boundary; backfill has no independent persistence
authority or alternate library. No network in Store locks. Shared DTOs, capability
manifest, Store schema and CLI/daemon assembly remain a serialized integration
write set, with an explicitly assigned writer. Parallel future groups may own
separate modules/tests, never concurrently edit shared assembly or duplicate policy.
Follow the current increment's explicit writer assignment; C-Staff owns interfaces only.

## Metadata module — types and field names

`item_type`: use `journal_article`, `book`, `webpage` for these three forms;
retain unknown/other existing types without silently recasting them. A science
news webpage describing a paper remains `webpage`: the paper DOI is a cited
reference unless it actually identifies the captured item itself.

All metadata fields below are optional `Sourced<T>`. Absence means unknown/not
provided, not a fabricated zero/empty date. Type controls the editor's suggested
fields, not destruction of retained source data when type changes.

| Field | Inner value | Primary forms |
| --- | --- | --- |
| `creators` | ordered `Creator[]` below | all |
| `identifiers` | `Identifier[]` below | all; DOI for paper, ISBN for book |
| `published_date` | `PartialDate` below | all |
| `journal_title` | string | journal_article |
| `publisher` | string | journal_article, book |
| `volume`, `issue`, `pages` | strings; preserve ranges, Roman numerals, e-locators | journal_article |
| `publication_place`, `edition` | strings, never force edition to integer | book |
| `number_of_pages` | positive integer when known | book |
| `series_title`, `series_number` | strings | book, journal_article |
| `language` | language tag when known; retain unparsed legacy source value | all |
| `keywords` | ordered strings, independent of manual_tags and topic clusters | all |
| `site_name`, `abstract_text` | existing strings | webpage / all |
| `institutions` | ordered `Institution[]` below | all when supplied |
| `cited_references` | ordered `CitedReference[]` below | all when supplied |

ISSNs use `identifiers.kind=issn`, not a separate competing field. Multiple print/
electronic ISSNs and multiple format ISBNs may be retained with their qualifiers.
Top-level `title`, `source_url`, `canonical_url` retain existing item semantics.
The UI shows exactly two dates: 发布日期 (`published_date`) and 采集日期 (read-only
`added_at`). For capture-origin items, added_at is the acquisition timestamp of
the capture that first successfully created the item, by user definition; for
Zotero imports it is the successful import/materialization timestamp. captured_at
remains source provenance, not a third UI date. Use the source-specific policy below.

Creator inner value:

```json
{
  "creator_id":"creator_opaque", "name_type":"person",
  "family_name":"Example", "given_name":"Ada", "literal_name":null,
  "display_name":"Ada Example", "role":"author",
  "institution_ids":["institution_opaque"]
}
```

`role`: author/editor/translator/contributor, retaining source-specific unknown
roles in import provenance instead of pretending they are authors. Preserve order.
`name_type`: person/organization/unparsed. Person has separately supplied family/
given names (either may be unknown); organization/unparsed uses `literal_name`.
Do not split Chinese names, initials or old display_name heuristically. display_name
remains usable by old UI. Opaque creator/institution IDs are item-local stable row
identities, not proof of global person identity. Same-name authors never auto-merge.

## added_at module — source-specific date policy

User clarification: “日期的元数据，一个是网页上登记的发布日期，另一个是采集纳入
MemeX 数据库的采集日期。” Keep that wording: do not show separate “添加日期” and
“抓取日期” fields alongside 采集日期. For books/papers, published_date still means
the source publication date, not import or lookup time.

Latest explicit definition: “Evernote 的数据，把原来的剪藏时间直接配套设置为导入
MemeX 的时间。剪藏也是一个采集行为，采集了，就代表这个数据纳入数据库的时间。
所以剪藏时间，就是 MemeX 入库时间，两个时间被定义为同一个。” This is a product
date definition, not a claim that historic physical commit happened at capture time.
It replaces draft3's requirement to find independent commit-time evidence for
capture-origin records. Do not keep that old gate or default all such dates to unknown.

1. **Existing capture-origin records:** the 1220 Evernote clips use their original
   clipping timestamps, already preserved as captured_at by prepare.py/main.rs.
   The 2 pre-existing MemeX items follow the same capture-date rule. Map by source
   identity to the authoritative capture associated with initial item creation;
   no additional independent inclusion/commit timestamp is required. Do not search
   for the oldest arbitrary timestamp across later captures, edits or source pages.
2. **New native captures:** in the first successful creation transaction, persist
   added_at from that successful capture's validated captured_at, not wall-clock
   commit time. A capture before midnight followed by commit next day retains the
   capture date. Failed attempts do not set an item's added_at. Recovery of the same
   frozen capture uses its same timestamp; a different successful capture uses its
   own timestamp when it creates the item for the first time. Preserve the value
   through edits, replay, linking, grouping, lookup, later captures, physical
   relocation and backup restore; never reset it to now. Journal/integrity and
   successful-creation guarantees still apply; this does not relax capture identity.
3. **Zotero imports (unchanged):** use each newly materialized native item's first
   successful MemeX import time, including notes/documents created separately.
   Never substitute Zotero dateAdded/dateModified, old source captured_at or file
   time. Linking to an existing item preserves its existing added_at and policy;
   importing a new membership does not reclassify or redatetime that item.

Normalize valid timestamp offsets to the same RFC3339 UTC instant; display in the
user's timezone (currently Asia/Taipei), preserving raw source timestamps. A legacy
capture-origin item with missing stored added_at may use its authoritative captured_at
in a compatibility read/display; this is the approved definition, not an estimate.
Persist through the dedicated backfill, not a render-time write. Do not apply this
fallback indiscriminately to Zotero or unknown origins, or hide a conflicting stored
added_at. Classify by acquisition/source identity, not item_type=webpage. Only truly
missing/invalid capture time remains “未记录”; malformed data must not become now.

## added_at backfill module — preview and guarded apply

Backfill is a separate command/module, not reimport or ordinary metadata editing.
Preview rows carry `{item_id,source_identity,captured_at,current_added_at,
proposed_added_at,source_ref,status,reason}`. source_ref binds the existing capture
record/identity used for the value, NOT a newly required independent commit proof.
Compare timestamps by instant, not literal offset spelling. For in-scope historic
capture-origin records, status has exactly these four values:

| Status | Meaning | Apply |
| --- | --- | --- |
| `set_from_capture` | added_at missing and one authoritative valid captured_at available | fill from capture |
| `already_present` | existing valid added_at matches the capture-policy value | no-op |
| `missing_capture_time` | authoritative capture time absent or invalid | no-op; reason distinguishes missing/invalid |
| `conflict` | nonmatching/invalid stored added_at, incompatible source binding, or ambiguous authoritative capture | no overwrite; explicit resolution required |

Validate source binding first; ambiguous/unsupported origin is conflict, never
guessed as capture-origin. For a known source, a missing capture time remains
missing_capture_time even if some existing added_at is present: retain that value
without claiming verified agreement. This capture-backfill command does not fill
Zotero dates from captured_at; a Zotero/unknown row explicitly included in its scope
is a conflict with reason `source_policy_mismatch`. It never silently crosses scopes.

Preview binds the selected item/source set, current values and canonical version/
hash. Apply rechecks those under the existing Store writer lock; drift fails rather
than overwriting later work. Only set_from_capture rows may write missing added_at;
persist a replay-safe receipt so retries neither duplicate changes nor reset dates.
Do not change body, publication date, old revisions, capture timestamps, original
Evidence, groups or item identity. Do not use UUID time, mtime, batch completion or
today's migration time as substitutes. No timestamp is fabricated for missing data.

Implement and verify the tool/dry-run on synthetic libraries first. Writing real
MemeXDB remains HOLD until the user approves the concrete real preview/write;
this date definition alone does not release that HOLD. Do not rerun the importer
or scan the private library as part of this spec change. Report each of the four
status counts and applied/no-op/conflict counts, not an all-filled success claim.

Acceptance: capture-before-midnight/commit-next-day keeps capture date; frozen
retry preserves it; failed attempt then a distinct successful capture uses the
successful capture; 1220 Evernote+2 MemeX use capture time without commit receipts;
Zotero still uses import time; offset-equivalent values are already_present;
missing/invalid capture and nonmatching existing added_at remain explicit;
stale preview rejects apply; repeated apply and physical restore preserve values.

## Metadata module — publication date precision

```json
{"value":"2024-02","precision":"month","raw":"2024年2月"}
```

`precision=year|month|day|unparsed`; value is exactly YYYY / YYYY-MM / YYYY-MM-DD,
or null for unparsed. Gregorian components must be valid, including leap days;
raw preserves imported wording. Never turn a year into January 1 or add midnight/
timezone to a publication date. Seasons, approximate dates, intervals and ambiguous
locale strings remain unparsed with raw text until explicitly corrected.
Provider online/print/issued dates remain distinct in its retained source response;
if they differ, preview offers separate candidates for published_date with labels,
not an undocumented earliest/latest rule. No date is synthesized from added_at.
Old published_date strings parse only when unambiguous; otherwise migrate as
unparsed. Legacy wire projection uses the typed value or raw fallback as a string.

## Read-only Zotero multipart projection

This read-only slice can ship independently of batch recovery. Its originally
reported counts/format distribution are execution reports, not independently
verified here. Projection/sorting performs no canonical writes, new revisions or
migration. The later AboutHealth/Evernote increment below renews batch implementation
scope, NOT real apply permission. Evidence may be inspected deterministically on
the trusted Mac for authorized aggregate coverage; no source/body disclosure,
network/model lookup. Parent inheritance follows the latest explicit exception
below. Native Zotero and other import
origins remain separate cohorts; do not apply Zotero rules to non-Zotero records.

The user's latest priority is publication-first: library/Inbox defaults become
published_at DESC once capabilities support it; list rows/details emphasize the
actual publication precision. The latest decision below adds labelled parent
publication inheritance and original-acquisition fallback for effective ordering/
display, never fabrication of a published_date. Follow library_query's current
effective-date and tie-break policy. The frontend reports 9,646 missing dates (7,184 Zotero and
2,462 other imports); keep that missing-date coverage/preview as the next priority,
not a verified count here or permission to write in this read-only release.

One shared effective-publication-date projection serves metadata reads, query
ordering and clients. Typed PartialDate remains authoritative, including unparsed;
do not override it with a legacy prefix. Ordinary legacy values retain the existing
unambiguous parser. Only legacy dates with verified native Zotero provenance/source
identity may use the multipart extension; a matching string shape alone, folder
name or client-supplied provider label is insufficient.

Multipart is an exact ten-character ASCII `YYYY-MM-DD` prefix, an ASCII space and
a nonempty source-date suffix. Preserve the entire stored string in raw. Gregorian
year must be 0001–9999; validated `YYYY-00-00` becomes year, `YYYY-MM-00` with
month 01–12 becomes month, and a valid full date becomes day. Year 0000, unknown
month with nonzero day, invalid month/day and invalid leap days remain unparsed;
never rescue invalid components by dropping precision. SQL datetimes are not
multipart. No timezone conversion or fabricated day/time.

The prefix alone is insufficient: Zotero strToMultipart can use a range's start
and discard approximate/uncertain qualifiers for sorting. The suffix must denote
one unambiguous date at the same precision and agree with the prefix. Support
explicit year-first numeric/Chinese forms and fixed English month-name forms
(including `Mon YYYY`) deterministically; no system-locale or free-form parser.
Ranges, seasons, circa/uncertain qualifiers, conflicting values, unsupported eras,
ambiguous locale dates or unrecognized suffixes stay unparsed with raw intact.
Do not silently accept just the recognizable substring. A valid ordinary legacy
date must not become unparsed merely because it lacks multipart shape.

Verification includes synthetic matching numeric/English/Chinese suffixes, year/
month zero placeholders, leap boundaries, invalid zero combinations, mismatched
suffix precision, ranges/qualifiers, SQL datetime lookalikes, non-Zotero lookalikes
and typed-value precedence. Metadata display and query sorting must produce the
same value/precision; no canonical field/provenance, body, Evidence or added_at
changes. Keep existing validation, privacy-view and cache freshness contracts;
do not reread the full source corpus on every warm query. Real acceptance reports
only aggregate projected precision/unparsed counts and read/sort behavior, not raw
private values. Format counts alone do not prove all values are safely parseable.

Upstream semantics checked in
https://raw.githubusercontent.com/zotero/utilities/master/date.js:
strToMultipart, isMultipart, multipartToSQL and multipartToStr. This source supports
the zero-component format but does not certify a prefix as an exact publication date.

## Publication-date batch invariants — original Zotero scope

The following preserves the original Zotero batch rules. The current increment
below renews implementation scope and adds a separate Evernote extraction queue;
it does not relax these CAS, preservation, preview or approval requirements.
Real apply requires the user's confirmation of the actual preview/plan.

The user requested batch comparison/filling of imported publication dates and
publication-date sorting. This increment is deterministic and owner-local: no
external lookup, models, reimport or other-item date inheritance. Scope follows
each item's verified native Zotero import identity, not its folder/title. Legacy
Evernote captures without that identity were outside the original scope; they now
use the separate evidence policy below, never the Zotero adapter. Other metadata fields
remain a separate unfinished part of the user's broader batch-enrichment goal.

The existing Zotero adapter already writes `fields.date` to legacy published_date
when supported. Typed-field absence is NOT proof that a date is missing. Reuse
the metadata v2 read policy: typed PartialDate first, otherwise parsed legacy
string. Sorting existing dates does not require rewriting equal legacy values.

Read only the item's own immutable import JSON `fields.date`, verifying import
identity, Evidence hash, containment and sensitivity. Never substitute dateAdded,
dateModified, accessDate, added_at, captured_at, file times, parent records or
dates guessed from text/URLs. Reuse PartialDate parsing and Gregorian validation;
keep ambiguous locale, seasons/ranges/approximate and invalid dates unparsed.
Retain raw wording, never pad year/month to a day or convert to a UTC instant.

Preview returns aggregate counts, an opaque preview_id, plan digest, parser/policy
version and scope description. Keep private per-item mappings of revision/hash,
both stored date fields, source Evidence/field hash and proposed value locally;
do not output titles, URLs, raw dates or per-item locators to shared logs/replies.
Use a consistent Store snapshot and existing full validation without retaining
a second full-text corpus. Broken/unsafe Evidence fails closed, not as missing.
Counts must reconcile with the authorized scope and be mutually exclusive:

- `fill_year`, `fill_month`, `fill_day`: both stored date fields absent and source
  parseable. These are the ONLY writable rows.
- `unparsed`: both fields absent and nonempty source date unparseable; no write.
- `already_present`: existing/source normalized value AND precision agree, or
  identical unparsed raw values; retain current provenance/fields unchanged.
- `conflict`: existing/source disagreement, unequal precision, inconsistent
  typed/legacy fields or ambiguous source binding; never overwrite.
- `missing`: source date absent/blank; retain any existing value without claiming
  source agreement. Source-validation errors are separately reported failures,
  never disguised as successful missing coverage. Inconsistent stored fields
  take conflict precedence, even when the source is missing.

Preview states exact writable total and one new metadata revision per write;
the batch is NOT one all-or-nothing transaction. No eligible rows means no
revision writes. Do not include new imports arriving after preview. Apply accepts
only the server's frozen plan and stable request identity, not client-provided
dates or item lists. Preserve body, Evidence, old revisions, added_at, groups,
unrelated metadata and current trash/permission behavior. Use the existing
metadata CAS/journal/replay writer and revision-limit validation, not direct
file rewrites. Bind source Evidence/field, parser and plan in the durable audit
chain through a trusted internal seam; no forged provider provenance or arbitrary
client-supplied sourced-value override. Preserve metadata-only reader inheritance
and query invalidation; a metadata edit grants no new model execution.

Preflight the frozen write set before mutation, then recheck authority, revision,
both missing fields and source binding at each row's CAS. On drift/error stop
admitting rows; earlier commits remain reported as partial, not rolled back or
claimed untouched. Persist stable batch-to-item request identities before writes.
After interruption reconcile durable receipts before retrying/reporting; completed
rows never rewrite and uncommitted rows never silently rebase. Restart does not
auto-resume. A changed plan needs new preview/confirmation. Bounded progress/status
and explicit same-plan continuation may reuse its frozen approval without widening
scope; an unfinished or failed batch must not appear fully successful.

After synthetic validation, this batch request permits a deterministic read-only
real-library preview on the trusted Mac, outputting aggregates only. Real apply
remains HOLD until the user sees actual counts/write total and explicitly confirms
that preview/plan. Record library identity, trusted owner, preview/plan digest,
displayed counts/scope, policy version, confirmation time/source and apply request
identity. The source is the owner UI confirmation or an exact user-message reference;
an agent-created approval flag or deployment permission is not confirmation.
This is an audit record, not cryptographic proof of human presence. Mac Settings
may submit apply directly after this confirmation; chat approval records its exact
reference and invokes the same frozen plan without another generic permission
prompt. No real writes as a test. New batch write commands remain owner-local;
mobile sorting is independent and does not enable remote batch application.

Test synthetic typed/legacy populated values, unequal precision/conflicts,
leap/invalid/ambiguous dates, unrelated timestamps/parents, source hash/permission
failure, CAS drift, interruption/replay, revision limits, preservation of body/
Evidence/added_at and metadata-only inheritance before enablement. Concrete batch
DTO/receipt assembly belongs to the current product writer, with these semantics;
do not claim implementation or real apply from this design freeze alone.

## Current increment: AboutHealth and Evernote publication-date recovery

On 2026-11-08 the frontend relayed the user's new priority: “最重要的原始发表日期
还没搞定，制定框架，读取日期，机械执行的时候让 secretary low 来执行。把已经导入的
aboutHealth 和印象笔记的条目都清理一遍。” This supersedes the earlier batch
implementation deferral and Evernote exclusion. It authorizes deterministic local
inspection, implementation and synthetic verification, then an aggregate real
preview. Real apply still awaits confirmation of the concrete preview/plan above.
No model calls, paid requests, new network retrieval, reimport, deletion, unrelated
metadata correction or daemon deployment is granted by this increment.

Scope is the existing AboutHealth and Evernote import cohorts, verified through
durable import/source identity rather than folder names or string shapes. Audit
both populated and missing fields; do not replace existing dates automatically.
Keep cohorts separate even if their items moved folders. Record snapshot identity,
selection rules and excluded/unauthorized coverage without leaking hidden counts.
Earlier 9,646 and newer approximate counts are historical reports, not frozen write
totals. Reconcile an exact snapshot before preview; candidate scans may overlap and
must not be added together as writable counts. Local deterministic source reading
is permitted; it is not accurate to describe a header scan as “no body read”. No
private body, title, URL, raw date or per-item mapping enters model/tool output,
shared logs or proof artifacts. Private plans retain only required evidence spans/
references, not a second full-text corpus.

### Evidence eligibility, not confidence voting

All extractors bind the item's own immutable Evidence/revision, content hash and
exact field or decoded UTF-8 span, with decoding/extractor/policy versions. Verify
source identity, containment, integrity, current permissions and trash state.
Malformed or inaccessible Evidence is an error, not missing evidence. A parser
recognizing a date proves syntax only, not its publication role.

| Evidence kind | Meaning | Eligible for a missing-field fill |
| --- | --- | --- |
| `zotero:fields.date` | Own verified native import date field, using the existing unambiguous/multipart policy | Yes, if valid and no competing evidence |
| `clip:header-labeled` | Origin-specific header field explicitly labelled publication/first publication, bound to this captured article | Yes, if the frozen rule establishes publication role and an unambiguous value |
| `clip:stored-datePublished` | Explicit publication field already retained in immutable capture evidence and bound to the same article | Yes, if available and valid; no fetching missing pages |
| `clip:header-single` | A lone date near the beginning without an explicit publication role | No; clue only |
| `clip:header-generic` | Generic “时间/来源” label or unclear date role | No; clue only |
| `clip:url-path` | Date-like URL path components | No; clue only |

Header position, uniqueness, a “来源” label or a confidence score cannot promote a
clue into a publication fact. Exclude quoted/cited articles' dates, page footers,
copyright years and update/creation/access/capture/file timestamps. Freeze concrete
origin-specific header boundaries and labels with counterexamples before enabling
an extractor; arbitrary text containing “发布” is insufficient. Retain evidence
roles such as published/updated/unknown separately. Distinct explicit publication
candidates, print/online differences or unequal precision require review, not an
implicit earliest/latest/most-precise rule. Ambiguous, approximate and invalid dates
remain unresolved. Parser changes require a new plan, not reinterpretation of an
approved plan.

The latest user decision below authorizes AboutHealth note/attachment inheritance
with a distinct provenance type, not own-publication evidence. Other relationships
remain insufficient. The book-project `source_heading_date` with role=unknown is
not publication evidence. Without qualifying publication evidence use the labelled
original-acquisition fallback below. Retain clues locally without demanding manual
inspection of every unknown row. Network/model enrichment remains separate scope.

### Latest user decision: inherited publication and acquisition fallback

This amendment supersedes the previous no-parent-inheritance and all-unpublished-
last rules. User wording relayed by F: “1. AboutHealth 的子笔记和附件，直接用它们所属
论文的发表日期，并标注「随父条目」。2. 日期未知的，按采集日期算，而不能按导入 MemeX
数据库的日期算。” Real metadata writes still require the concrete preview approval;
these rules also permit a read-only effective-date projection before persistence.

For AboutHealth child notes/attachments, resolve the owning paper using verified
native source parent identity and the persistent source-to-item mapping, not folder,
title or a guessed relation. Use that paper's actual publication date and precision
as `inherited_from_parent`, labelled “随父条目”. This is the preferred effective
publication date for these children; retain their own existing fields unchanged.
Do not inherit the parent's acquisition fallback, recurse through arbitrary parents,
or inherit an unknown-role date. Missing/ambiguous/cyclic relationships are explicit
unresolved states. If no usable parent publication exists, use the child's own valid
publication date when available, otherwise its own acquisition date, otherwise unknown.

Persist inherited provenance separately from own publication fields in an optional
`inherited_publication_date` sourced record: PartialDate, origin=`inherited_from_parent`,
parent item/revision identity, parent publication-field/evidence digest and the
immutable source relationship reference/digest. Never flatten this into an own
published_date or a manual/provider lookup origin. Bind both child CAS and parent/
relationship versions in the preview and recheck under Store's writer boundary.
Only create/refresh this separate record; do not overwrite child publication evidence.
Report inherited-create/refresh/no-op/conflict separately from own-date fills, with
an exact distinct-item write total. Same-plan replay uses the same receipts.

Inheritance is not a permanently authoritative copied date: effective reads resolve
the current visible parent publication; persisted records retain their frozen audit
snapshot and can become stale. Parent date/relationship/permission/trash changes
invalidate the affected visible child projection and pagination view. Do not silently
refresh canonical metadata during reads. Both current child and parent authority are
required; inaccessible parents reveal no inherited date, ID, evidence or counts.
Do not treat a dependency integrity error as missing and silently fall back. A changed
parent at apply causes conflict, never automatic rebase onto a new date.

When there is no usable own/inherited publication date, the effective date is the
original acquisition timestamp projected to a calendar day, labelled “采集日期”.
It participates in the same chronological ordering as publication dates, not a
separate unpublished tail. Preserve unknown/unparsed publication fields as-is; the
fallback never writes published_date or upgrades its evidence. Only when neither
publication nor original acquisition is usable does the effective date remain unknown.

Original acquisition is source-specific:
- Native captures and Evernote clips: the verified original successful capture/
  clipping timestamp bound to initial item creation, not a later recapture/edit.
- Zotero: the item's own original `items.dateAdded` retained in immutable native
  JSON Evidence, interpreted as “加入 Zotero 资料库的时间”. This is an explicitly
  chosen acquisition proxy, not proof of first website access or publication.
  Missing/invalid dateAdded remains unknown; never substitute dateModified,
  accessDate, parent dateAdded, file time, MemeX import time or added_at.

The existing native importer sets CaptureEnvelope.captured_at from import intent
time (native_import/items.rs); therefore field name alone is NOT an acquisition
proof. Do not COALESCE that stored captured_at, rewrite it, or change historical
added_at rules. Resolve a separate read-only `effective_capture_at` with source
origin `native_capture|evernote_clip|zotero_date_added`. Zotero source SQL timestamps
are UTC, not the Mac's local timezone; offset-bearing ISO values normalize by instant.
Unsupported/ambiguous timestamps are unresolved. Preserve raw evidence. Zotero's
official API v3 changes document its legacy SQL timestamp UTC interpretation:
https://www.zotero.org/support/dev/web_api/v3/changes_from_v2.

Use the same server date-policy timezone for fallback display and sorting (current
product timezone Asia/Taipei), preserving the original instant separately. Acquisition
fallback has day precision; this does not invent day precision for publication dates.
Bind the timezone/policy to the query view. Metadata and query rows expose the same
`effective_date` PartialDate/null, `effective_date_basis` of
`published|inherited_from_parent|captured|unknown`, and `effective_capture_at`/origin.
Keep the existing published_date response's publication-only meaning. Clients use
the effective fields and source labels, not their own parsing or `added_at` fallback.
`inherited_from_parent` is distinct in evidence reporting, not a new user item type.

The previous added_at tertiary tie-break is removed from published_at ordering so
MemeX import time cannot decide this date sort. Equal effective keys use verified
effective_capture_at DESC (missing last), then item_id ASC. Other explicitly selected
sort modes are unchanged. This supersedes the prior captured_at/added_at tie policy.

### Exact preview and mechanical execution

Reuse the batch snapshot, CAS, receipt, interruption and approval invariants above.
Only `fill_year|fill_month|fill_day` with BOTH stored fields absent may fill own
publication fields; the separate inherited record follows the amendment above.
Present typed unparsed or legacy raw fields are not “absent” and are never silently
overwritten. Audit them as existing unresolved/conflict, while sorting/display
continues through the shared effective-date projection.

Each selected visible item has exactly one terminal preview category: eligible
fill by precision; already-present agreement; existing unresolved/unverified;
conflict; source-unparsed; clue-only; missing; or validation error. Specify category
precedence so these sum to the snapshot total; source errors and stored/source
conflicts cannot disappear into unknown/missing. Per-extractor candidate counts are
separate, possibly overlapping diagnostics. Publish exact writable/skipped/error
totals by cohort and evidence kind plus precision, not just dates-found counts.
Extend these counts with distinct inherited-record outcomes; acquisition fallback
is a read-only display/sort result and never contributes to the write total.

Bind preview_id, plan digest, library/snapshot identity, scope, item revisions and
both date fields, source bindings, extractor/policy versions and executable identity.
Persist batch/item request identities before mutation. Real confirmation covers
that exact plan and displayed writable count, not all future missing items.
Preflight plus per-row CAS rechecks remain mandatory. The durable source receipt
retains extraction evidence through a trusted Store seam; do not impersonate a
manual edit or invent provider provenance. No direct canonical JSON/SQL edits,
lock/sync_dir bypass or blanket metadata rewrite.

The user-selected secretary runs in Amp `low` mode as a mechanical executor only.
Coder owns extractor implementation, source-role decisions and validation; C-Staff
owns contract/acceptance. Dispatch low only with the frozen command/tool build,
scoped input identity, expected aggregate outputs, budget/bounds and stop rules.
Read-only preview execution does not grant apply. Apply additionally requires the
recorded human confirmation for that plan. Deterministic code may privately read
the authorized source; low must not inspect raw plans/body, infer dates, edit code,
change scope/policy, call models/network or produce its own approval. No automatic
mode escalation or replacement of the requested low role.

On drift, unrecognized errors or uncertain outcomes, stop admitting new writes;
reconcile durable receipts using the original request identities. Never label an
unknown commit as “not written”, switch request IDs, or regenerate a plan to force
continuation. Report partial counts and remaining rows. Recovery/status inspection
does not itself resume writes; explicit same-plan continuation follows the existing
approval boundary. Backup/restore validation covers plan/receipt/source dependencies
and completed revisions with a synthetic consistent snapshot, not a hot-copy of
the real database or a destructive real-library test.

### Ownership and acceptance

B owns the backend increment: shared effective-date/multipart projection, Store/
query/library sorting and invalidation, assembly/capability surfaces, extractors,
preview/apply and receipts. F owns client capability gating, defaults/date displays
and the agreed preview/confirmation interface, retaining its annotations/passes
write set. Shared gateway/DTO assembly remains serialized with B; F and B coordinate
directly. Preserve B's book-project checkpoint and unfinished responsibilities;
publication-date work takes priority without discarding book changes. Low receives
execution work only once coder supplies validated frozen tooling. C-Staff does not
take product ownership or create a second backend writer.

Read-only sort/projection can be delivered before backfill; sort support must be
verified end-to-end before the clients switch defaults. Retain published_at DESC
for Library/Inbox using the latest effective-date policy above: publication or
labelled original acquisition, effective_capture_at DESC then item_id ASC ties,
and only entirely unknown effective dates last in both directions. One projection
serves metadata and query results; no warm-query full-corpus rescans or acquisition
dates masquerading as publication.

Synthetic acceptance must distinguish actual publication headers from cited/update/
generic dates; own attachment evidence from inherited parent dates; Evernote from Zotero
multipart lookalikes; single/URL clues from eligible rows; and existing raw/typed
dates from true absence. Cover exact coverage accounting, source/hash/permission
failures, preview drift, partial interruption and receipt replay, unknown commit
outcomes, recovery preservation, and query sorting/filtering/pagination/invalidation.
Add source dateAdded versus import captured_at/added_at counterexamples, parent
date/permission/trash drift, non-inheritance of parent acquisition, child-own field
preservation, inherited receipt replay, mixed publication/acquisition chronology,
UTC-to-policy-date midnight boundaries and neither-date cases in both directions.
Report code, synthetic results, read-only real preview and confirmed real apply
as separate delivery states. No real write count is promised from the initial
approximate coverage investigation.

## Metadata module — identifier normalization, validation and duplicate handling

```json
{"kind":"isbn","value":"9780306406157","raw_value":"0-306-40615-2","qualifier":"print"}
```

Retain kind/value for compatibility. Server computes normalized values and
read-only `validation:{status,reason,normalizer_version}`, where status is
`valid|invalid|unparsed`; reason is null when valid. Invalid legacy/source values
are retained for correction but excluded
from exact dedup/lookup keys; a new manual invalid identifier rejects the whole
edit with a field error. `valid` means locally valid structure/checksum, not proof
of registration, ownership, existence or metadata accuracy.

- DOI: trim outer whitespace; remove a leading case-insensitive `doi:` or exact
  doi.org/dx.doi.org HTTP(S) resolver wrapper. For a URL, reject credentials and
  ambiguity from query/fragment; decode percent escapes once, not repeatedly.
  Store bare DOI with ASCII case normalized to lowercase, preserving raw input.
  Require `10.` numeric prefix (allow dot-separated numeric components), slash,
  nonempty suffix; reject controls/internal whitespace. Do not use a Crossref-only
  suffix allowlist, strip terminal punctuation blindly, or truncate long IDs.
  Non-ASCII case-equivalence beyond the supported normalizer is unparsed/manual
  review, not falsely claimed invalid or silently merged. Syntax success is not
  a registry lookup. DOI suffix punctuation and embedded slashes must survive.
- ISBN: remove an explicit ISBN/ISBN-10/ISBN-13 label and display whitespace/hyphens;
  uppercase X. ISBN-10 has nine digits plus digit/X and weighted sum 10..1 divisible
  by 11. ISBN-13 has 13 digits, prefix 978 or 979, and alternating 1/3 weighted sum
  divisible by 10 (including the check digit). Convert valid ISBN-10 to its 978
  ISBN-13 counterpart by recalculating the check digit for the comparison key;
  retain raw_value. Do not invent ISBN-10 for 979. ISBN identifies an edition/
  format, not all editions of a work; publication title alone is not a key.
- ISSN: strip display hyphen/whitespace, uppercase X, require seven digits plus
  digit/X and weights 8..1 divisible by 11; display XXXX-XXXX. It identifies a
  serial, NOT an article, so shared ISSN is never an item duplicate key.
- Existing PMID and unknown source identifiers are retained; no new validation
  or automatic equivalence claim is made for them in this increment.

Duplicate detection uses normalized DOI or equivalent ISBN-13 keys only inside
the authorized target library; cited-reference identifiers do not participate in
the containing item's identity. Return authorized candidate item/revision IDs and
which identifier matched. Hidden duplicates must not leak titles, IDs or counts.
One verified same-object match can link an import's membership to the existing
item; multiple matches, conflicting identifiers/types/edition facts require review.
This is not authorization to delete/merge existing canonical items, overwrite a
different capture, discard attachments or relax durable capture replay identity.
Different Evidence/revisions remain distinct. Existing ambiguous duplicates stay
visible as a review task; no full-library automatic merge or reimport is implied.

## Lookup module — provider requests and frozen preview

Approved source selection: Crossref for DOI (`https://api.crossref.org/works/{doi}`),
Open Library edition-level ISBN API (`https://openlibrary.org/isbn/{isbn}.json`).
Crossref not_found does not mean invalid DOI; Open Library missing a book does not
mean invalid ISBN. No silent provider fallback, domestic paid catalog, credentials
or model enrichment. Additional sources require their own approved adapter/scope.

Architecture: the Mac orchestrator fetches bounded responses; pure metadata
adapters parse them to sourced candidates. Reuse safe fetch/pinned transport,
per-origin rate limiting and Retry-After handling. Translators/parsers do not gain
network authority. Only approved HTTPS provider hosts/routes; encode identifiers
as data, never interpolate an arbitrary user URL or follow arbitrary redirects.

Authorization: user has approved sending only the DOI/ISBN of user-selected items
from the Mac to Crossref/Open Library for metadata lookup. Do not request the same
approval again for that scope. The UI keeps this disclosure visible; a selected
item is not authorization to send its title, body, personal tags, group names,
credentials, entire library or reference list. No background full-library lookup,
model use or provider-credential changes. Existing unrelated HOLDs remain in force.
This is distinct from the earlier six-article trial, not an expansion of it.
No item is currently selected by this contract update, so it triggers no request.
Normalization, checksum checks and local duplicate search remain offline.

Deferred request: one-click article/book fulltext retrieval is NOT opened by this
approval, including Unpaywall/arXiv/PMC or shadow-library sources. Metadata lookup
must not follow returned fulltext/download URLs. Any future fulltext feature needs
its own source, rights/access and request scope; no work on that feature this round.

Preview binds `preview_id`, item_id, expected_revision_id, provider/adapter version,
normalized identifier, fetched_at, response hash and retained source reference.
Each candidate has `candidate_id`, `field`, `current_value`, `proposed_value`,
`source_label`, `conflict`, `selected:false`. Missing values are null, not strings.
Equal values are marked unchanged; conflicts default to retaining current values.
User selects fields; no silent overwrite or auto-commit. Arrays (creators,
identifiers, references, institutions) are one field-level choice in this version,
not independently merged rows; preserve linked IDs and validate all references.
At most one candidate may be selected per field. For coupled creator/institution
choices the preview identifies required fields; a missing dependency blocks apply,
never silently selects an extra field on the user's behalf.
Selecting nothing is a no-op. Preview is not a content revision.

## Metadata module — atomic editing and lookup acceptance

Versioned edit intent (illustrative shape, NOT an implemented route):

```json
{
  "metadata_schema_version":2,
  "item_id":"item_opaque", "expected_revision_id":"revision_opaque",
  "request_id":"request_opaque",
  "set":{"published_date":{"value":"2024","precision":"year","raw":"2024"}},
  "clear":[], "preview_id":null, "selected_candidate_ids":[]
}
```

`set` values are unsourced owner inputs; backend assigns provenance. Omitted fields
remain unchanged; `clear` is explicit removal, and cannot overlap set or accepted
candidate fields. Reject unknown/readonly fields (especially added_at), invalid
date/checksum values, duplicate row IDs and dangling institution references as one
atomic edit. Applying lookup uses preview_id + selected_candidate_ids; only the
server's frozen candidate values may be applied, never client-forged provider data.
Authorization is checked before revealing revision conflicts. Under the Store
writer lock, expected_revision_id mismatch returns existing `stale_item_revision`
and writes nothing; client refreshes and reviews again, no silent rebase.
Changed fields commit in one new revision with a previous/new revision receipt;
body bytes, original Evidence, membership and unrelated metadata are unchanged.
Request identity replay returns its durable receipt without applying twice; same
identity with different payload conflicts. No-op returns metadata_unchanged.
No network call runs under the writer lock. Invalidated projections/Analysis follow
existing revision-bound rules; metadata edit is not a new generation authorization.

## Metadata module — bibliometric data and Zotero preservation

`Institution`: {institution_id, name, identifiers:[{kind,value}], country:null|string}.
Names are supplied text; optional external IDs require their own validation. Link
creators by institution_ids only when the source explicitly provides the relation;
item-wide affiliation text does not prove every author's institution.

`CitedReference`: {reference_id, raw_text, title, creators, published_date,
journal_title, volume, issue, pages, identifiers, target_item_id, source_refs}.
Fields other than reference_id may be absent; raw_text preserves unparsed citation.
Nested creators/dates/identifiers use the same value types; arrays retain source
order. target_item_id is optional, authorized and explicitly resolved, never a
title-similarity assertion. Keep unresolved citations; do not auto-fetch their URLs.
Absent cited_references means unavailable/not supplied, whereas an explicit empty
list means the source supplied no references. Record that distinction and source
coverage; do not present incomplete graphs as complete citation counts.

Preserve Zotero creator roles/order/name mode, identifiers, raw dates, journal/
publisher/edition/page fields, tags and available keywords, series, affiliations
and cited references with source library/item keys. Import Extra/custom/unmapped
fields losslessly in the retained source evidence even if no typed field fits.
Zotero exports may lack affiliations/references: report unavailable, do not infer
or claim preservation of data not supplied. Initial source hierarchy is retained
under groups_v1 draft3, then becomes editable native MemeX state rather than a
read-only mirror. Imported notes and supplied attachments also become native
Markdown content with preserved originals; bibliography-only import cannot stand
in for full content conversion. Enrichment cannot replace those memberships.

Future graphs derive from authorized item/revision/hash sets, record algorithm/
normalizer versions, and invalidate when revisions/permissions change. Authors,
institutions, co-keywords and citation edges are separate entities/relations;
same display names do not establish identity. This prepares data only: no graph
layout, CiteSpace compatibility/export, disambiguation accuracy or analysis UI is
promised by this contract.

## Accepted extension: edition identities and sourced metrics (not implemented)

The user requires native fields instead of repurposed Zotero fields. This section
records the accepted behavior; it does not add a capability or claim that current
metadata v2, lookup adapters, query sorting or clients implement it.

For paired books, distinguish the work from its editions. Original/translation is
an explicit relationship, not inferred from canonical content, default reading
side, file import order or the user's language. Each edition carries its language,
author/translator roles and independently evidenced publication/ISBN identity.
A published translation is not model-generated text. Unknown exact edition stays
unknown: matching a title and translator alone does not establish publisher/year
or ISBN. The specified MemeX pair is original English and translation Chinese by
Yu Xiaodan (于晓丹), authored by Vladimir Nabokov. Its existing side hashes and
Evidence remain independent; future metadata enrichment must preserve them.

Each metric observation needs a provider, provider entity ID and source URL,
metric name, exact numeric value, unit/scale, observation time, and object scope
(work/edition/article/film/model release). Preserve unknown separately from zero.
Keep ratings, rating counts, review counts, views and citations as separate fields.
Never transfer an edition-specific metric to another translator/edition or merge
providers into a fabricated common score. Later observations supersede the current
sortable projection, not the retained evidence of earlier observations.

- Books: Goodreads and Douban Books ratings and rating counts. Determine whether
  the provider exposes work-wide or edition-specific aggregates before binding.
- Papers: provider-specific incoming citation counts and observation dates;
  an article's reference-list length is not its incoming citation count.
- Web articles: source-published view/comment counts where available, with the
  site's stated meaning. Missing/hidden metrics are not guessed by a model.
- Films: IMDb, Rotten Tomatoes and Douban Movies, keeping user ratings, critics'
  approval percentages and audience metrics distinct.
- Models: exact release/version, release date, named benchmark and its version,
  score/unit and known evaluation conditions. Unknown conditions remain explicit;
  unlike runs must not silently share a comparable ranking.
- Company financial/stock analysis: explicitly deferred commercial-edition
  candidate, not part of this implementation authorization.

Reusable CLI enrichment separates candidate discovery, bounded public-page
extraction, identity judgment and validated application. Use deterministic parsing
for source numbers and identifiers; configurable Low dial may choose among bounded
candidate IDs or return uncertain, not invent values or rewrite source evidence.
An explicit one-click job may orchestrate those phases within its request/token
budget; it must preserve confirmed owner fields, record conflicts, cache unchanged
inputs and never run paid requests merely because the Library was opened.
Source HTML and model output are untrusted. Access restrictions or unavailable
metrics produce an honest missing result, not a bypass or synthetic replacement.
Sorting is by a selected provider/metric with scope and missing-value semantics;
no composite popularity or cross-provider normalization is implied.

## Podcast type correction — deterministic preview and explicit apply

This independent B1 increment changes only item_type to `podcast` through the
existing metadata CAS writer. It does not reopen publication-date backfill or
authorize a real batch now. The frontend remains the sole product writer.
No model, network fetch, semantic body classification, reimport, group change,
date change or source Evidence mutation is included. Structural extraction from
already authorized local source material is deterministic and stays on the Mac;
titles, URLs, body excerpts and source payloads do not enter aggregate reports.

Versioned source rules must identify an episode, not merely a page discussing
podcasts. Parse URLs into exact host/path components and combine with the rule's
required episode/source structure (for example explicit episode metadata or a
verified episode route). A substring `/podcast/`, duration, speaker labels or
timestamps alone is insufficient. No new URL resolution or external discovery.
Each rule has positive/negative synthetic examples and a stable ID/version;
ambiguous/conflicting source signals remain unresolved, not automatically changed.

Preview includes only authorized current nontrashed items. It returns an opaque
plan ID/hash, expiry, rule version and aggregate counts: eligible changes,
already podcast, protected/conflicting existing type, ambiguous, unsupported or
missing source. These categories are disjoint and sum to the inspected scope;
per-rule candidate counts must not double-count an item. The protected plan binds
library identity, principal/ceiling, exact item set, revisions, source hashes,
current type/provenance and proposed podcast type. Apply never rescans to expand
that set. Existing explicitly owner-chosen types are protected conflicts; defaults
or imported webpage/unknown types may be candidates when the rule is conclusive.
Other established types are conflicts rather than automatic replacements.

The user's confirmation of that actual preview authorizes that exact batch only.
Persist the accepted plan hash, confirmation identity/time and request identity;
a boolean detached from a frozen plan is not authorization. Preview performs no
canonical write and creates no content revision. Expired/missing or drifted plans
require a new preview; this contract itself is not a confirmation of real counts.
Initial real apply is owner-local; remote batch apply stays unavailable without
its own authenticated write/admission contract. No production run is triggered
by implementing or advertising the feature.

Apply checks current access/trash, frozen source identity and expected revision
for each item under the metadata writer boundary, and changes no other field.
Preserve source provenance, body, Evidence, memberships, added_at and history.
Existing metadata revision/receipt/invalidation semantics remain authoritative.
Use bounded server-generated per-item request identities bound to plan+ordinal;
retain a batch manifest/progress record rather than claiming whole-library atomicity.
On conflict, failure, stop or unknown, admit no further items; previously committed
items remain committed and are reported by count. Reconcile durable receipts before
any continuation; replay returns completed receipts, never re-applies them. A
restart must not silently resume writes. Reverting a mistaken committed type uses
a separately confirmed CAS edit against the current revision, not a snapshot reset.

Before capability enablement prove false-positive source rules, protected manual
types, hidden/trash exclusion, preview drift, concurrent metadata edits, crash after
commit before acknowledgement, exact replay, partial completion and unchanged body/
Evidence/dates using synthetic fixtures. Capability/DTO integration must expose
preview, apply and recoverable progress without claiming this document is a shipped
endpoint. Real acceptance first presents only aggregate preview counts to the user.

## Implementation proof and rollback gates

Before enablement, test legacy metadata read/roundtrip without fabricated name/date,
partial/leap dates, ISBN-10/978 equivalence and 979 boundary, invalid checksums,
DOI prefixes/case/punctuation, ISSN not deduplicating articles, hidden duplicates,
conflicting metadata, stale preview/edit, durable replay, and source-preserving
import with absent references/unknown fields. Use synthetic fixtures; no private
inventory or real import is authorized here. Keep source snapshots and old revisions;
new projections must be rebuildable. Feature-off disables new writes/lookup, not
destructive downgrade or erasure of newly stored metadata.

References checked for design (documentation only, no item lookup):
- https://www.doi.org/resources/DOI_article_ELIS3.pdf — DOI prefix/suffix and case semantics.
- https://staging.doi.org/the-identifier/resources/factsheets/doi-system-and-the-isbn-system — ISBN-10 to 978 ISBN-13 conversion.
- https://www.crossref.org/documentation/retrieve-metadata/rest-api — DOI works lookup and provider scope.
- https://openlibrary.org/dev/docs/api/books — edition/ISBN vs work-level metadata.

## Current-revision duration read increment (2026-11-09 local implementation)

`bibliographic_metadata` (`ItemMetadataV2Read`, metadata_schema_version remains 2)
adds top-level `duration_seconds: integer | null`: positive u32 seconds unwrapped
from `item.metadata.duration_seconds.value`, absent as explicit JSON null. This
is read-only and does not add a duration edit field or change provenance.
The existing top-level `published_date: PartialDate | null` and
`published_date_origin: "typed" | "legacy_string" | null` retain their shape.
Clients must tolerate the additive field; older omitted values mean unknown.

Both fields read the verified current revision, not historical root `item.json`:
`read_item_metadata_v2` calls `read_item_metadata_at_sensitivity`, whose verified
header selects `revisions/<current_revision>.json` and checks its identity/hash.
Sensitivity/trash filtering precedes exposure. Typed publication dates take
precedence over parsed legacy strings; no capture-date fallback is introduced.
An RSS fill is visible on the next read with the new `item.revision_id`.

For list display, use the separately documented `published_at: YYYY-MM-DD | null`
and `duration_seconds` on library summaries (see `memex.library_query.md`).
Partial publication dates stay available here as PartialDate, but never become
fabricated day-precision list dates. No canonical schema migration, baseline
rewrite or deployment is part of this increment; rollback removes read fields
without deleting existing metadata or evidence.
