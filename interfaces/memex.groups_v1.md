# Groups and topic clusters — contract draft 4

Status: g2 approved; tree/query and the single-op writes documented below exist
in current source. Owner-local native import and topic-planning CLI implementation
is specified in `memex.native_import_v1.md`; it does not imply arbitrary folder
moves, synchronized mirrors or the topic_cluster Analysis API. Current real-data
application and verification receipts are separate from this interface contract.
The product writer reports deployment. Frontend relayed user decisions
recorded in its DECISIONS.md, not independently read here.
Spec owner: C-Staff. Current organization increment: B owns backend implementation,
F owns clients; the earlier frontend/backend single-writer window is historical.
Source: user requested Zotero group-preserving import and topic-based grouping.
User clarification: “group 之后是目录，多层级。最终数据库是树状结构。保留子文件夹”.
Latest clarification: “我要导入的是数据，全部 Markdown 化按 MemeX 的数据结构存储，
不是导入目录样子货”. Import produces native editable data, not a read-only mirror.
The include-descendants view switch belongs in the top toolbar; the frontend sends
its value explicitly. Folder transfers follow the 2026-11-11 additive contract
below; item organization follows the 2026-11-08 freeze.
Existing canonical files, Evidence/Revision and sensitivity checks remain authoritative.

## Item organization: folders, mappings and independent copies — 2026-11-08

User decision relayed by F: an item can occur in several folders in one library;
moving between folders is frequent, changing libraries is infrequent. Across
libraries distinguish “映射过去，是同一个东西” from “复制过去，是两个独立的东西”,
so collaborators can edit a copy without changing the private original. Here a
library means a root Group in one physical MemeX Store, not a second database.
This freezes item organization, not remote Group sharing,
cross-Store transfer or ACL implementation. Existing node-delete HOLD is unchanged.

### Four distinct user actions

- “添加到文件夹”: add membership in a target folder, preserving all other locations.
- “移动到文件夹”: within one root, add target membership and remove only the exact
  selected source-folder membership. Other folders, roots, content and item ID stay
  unchanged. It does not mean remove from every location displayed in a recursive,
  All, search or Inbox result. Such views must resolve an explicit source location;
  virtual Inbox has no removable membership.
- “映射到其他库”: add membership in the target root/folder, retaining the source
  membership and the SAME item ID/revisions. Edits affect that same item everywhere;
  this is not a snapshot, isolated fork or permission grant. Removing one mapping
  does not remove other locations or delete the canonical item.
- “复制到其他库”: create new item identity and independent revision/state ownership,
  preserving the source unchanged. A copy has lineage but no automatic merge,
  synchronization or write-through to the source.

Mapping and addition can use existing add_members with its current authorization,
CAS, quota and replay rules. Mapping into a root does not silently recreate source
folders. Membership alone neither lowers sensitivity nor makes an item available
to another principal. Current local roots are not proof of implemented collaborative
ACL or synchronization; clients must not claim that an item has been shared remotely.

### Same-root atomic move_members

Implement an additive group_edit op `move_members`, advertised explicitly in
groups_v1.write_ops. Do not enable by app version or the old generic `moves` flag;
folder transfers require the separate `transfer_folders` capability below. One request still has exactly one op:

```json
{"command":"group_edit","max_sensitivity":"private","request_id":"move_example","expected_version":"target_node_version","ops":[{"op":"move_members","source_group_id":"folder_source","target_group_id":"folder_target","expected_source_version":"source_node_version","item_ids":["item_example"]}]}
```

Both endpoints must be distinct active folders in the same root, currently writable
and visible to the caller. The initial slice is folder-to-folder, not cross-root
movement, root trash or folder-tree reparenting. Require all selected items to be
authorized active direct source members and respect existing global/group-trash
constraints; a stale/missing membership must not become a new import or resurrection.
Normalize the nonempty ID set with the existing 256-item limit. Check both versions
and all relevant authority/membership/trash state under the same Store writer lock.
Any failure rejects the entire set before publication. A pre-existing target
membership is valid: retain it and remove only the selected source membership.

Publish the membership changes, changed node versions, groups generation and exact
request receipt in ONE durable groups-state replacement, retaining sync barriers.
Both nodes whose membership changes advance version; a wholly pre-existing target
need not advance. Reuse the existing GroupEditReceipt shape with group_id/version
identifying the target. Refresh source and target views before subsequent edits;
historical replay receipts do not claim present membership/version. Existing state
and receipt formats need not change merely to add this operation. Validate actual
old-binary read/write preservation; do not activate group-state v2 node deletion
as a side effect. Existing visibility checks precede replay disclosure.

Use existing invalid_group_edit for invalid shape/same node/cross-root/non-folder;
stale_group_version for either version drift, and existing hidden/read-only/limit
errors. A selected item no longer being a direct source member returns
stale_group_membership, before writes. An I/O error after commit can be unknown;
read/replay the original request, never assume rollback or issue a new identity.

Until this capability is delivered, F may use the explicitly non-atomic two-step
transition. Freeze the selected source/target/items and separate request IDs for
add and remove. Remove only after destination addition is definitely confirmed
(including verified already-present membership), not after a timeout/unknown.
Keep source CAS; do not silently substitute a new version after conflict. If removal
fails, display “已添加到目标，尚未移出来源” or unknown as appropriate. After restart,
reconcile the original requests and current memberships before any continuation.
Never automatically delete the destination as rollback: it may have pre-existed or
been edited concurrently. Do not advertise atomicity or offer blind Undo. This
transition does not block shipping ordinary addition/mapping independently.

### Independent copy: content and privacy boundary

B owns a new owner-local copy operation, separate from membership editing and
native import identity deduplication. Freeze a source revision/content hash,
target node/version, selected payload/dependency manifest and caller authority in
an authorized preview. UI confirmation identifies the destination and included/
excluded data. A request for a specific copy authorizes only that snapshot and
destination; this contract is not permission to copy private real data as a test.

Default copy payload is the complete current document body, bibliographic metadata
(title, authors, publication date, identifiers, language, abstract and scholarly
keywords), and the locally retained images/attachments actually required by that
body. Preserve source publication and verified original-acquisition facts; new
copy creation time is separate and must not replace the effective date. Do not
reuse native-import keys, ingest identities or automatic DOI/ISBN merge behavior:
a new deliberate copy gets a new ID even when its content/identifiers match.

Original raw Evidence is NOT indiscriminately shared. Inventory required resources
and original Evidence separately with source roles, hashes, sizes and sensitivity;
the preview states what the destination will contain. New copy Evidence records
the exact copied snapshot and approved dependencies, with truthful derivative
provenance. Optional raw source material requires explicit inclusion in that plan;
it may contain private source paths, notes or unrelated fields. Do not mutate or
redact original Evidence in place. Missing/unauthorized dependencies must be reported,
never fetched from the network, silently omitted, or left as privileged source-item
paths. Approved ordinary external URLs may remain links, not a claim of local backup.

By default do NOT copy private manual tags, annotations/highlights, reading positions,
bookmarks, reading passes/timing, AI insights, translations/conversations, provider
configuration/credentials, receipts, old revision history, group memberships/trash,
or live source-parent links. Do not silently turn an inherited publication date
into a self-evidenced date. The preview must separately explain such excluded
state and inherited-date dependencies. Future opt-in annotation/translation/project
copy requires revision/anchor rebinding and its own validated dependency closure,
not duplication of sidecar files under a new name.

The complete payload depends on the item type. A book/project manifest is not a
standalone document: required editions, sections and alignment cannot be silently
dropped while claiming to copy the item. The first implementation may support
ordinary documents independently; preview must explicitly mark unsupported composite
types unavailable until their complete-copy adapter and anchor tests exist. This
does not redefine projects as ordinary documents or claim all-item copy is finished.

Record owner-private lineage: source item/revision/hash, target item ID, copy plan/
policy digest and creation request/time. It is an audit link, not an authorization
path back into the private source. Destination-visible provenance contains only
the approved bibliographic/source information and copied Evidence; source root names,
private IDs/paths, private history and audit records must not leak. Copy-body edits
and deletion of either item must not mutate or destroy the other's retained payload.
Immutable content-addressed blobs may deduplicate physically; mutable canonical
files/sidecars must not be shared by symlink/hardlink. Restoration/GC must preserve
both items' dependencies and private lineage access boundaries.

Require current source read authority and target create/write authority at commit.
The new item's sensitivity is at least the maximum of the source item, included
resources and target effective sensitivity; never automatically downgrade to match
a less restricted Group. Preview explains when the intended audience cannot read
the copy. Reclassification/declassification or actual collaborative sharing needs
its own explicit scope and verified ACL implementation. Remote copy remains
unavailable in the first owner-UDS delivery; neither a client ceiling nor an edit
scope alone substitutes for the trusted final authority gate.

### Copy recovery, capability and delivery

Reserve a new target ID durably against the library/principal/request and frozen
input digest before creation. Same request replays that copy; conflicting reuse
rejects. A distinct intentional copy request may create another independent item.
Recheck source revision, target version/authority and all payload hashes before
new work; drift never silently updates the frozen snapshot. Source canonical data,
Evidence, reader sidecars and memberships remain unchanged throughout.

Content creation and target filing need not be a fabricated cross-file atomic
transaction. Use the Store journal plus durable organization progress/receipts,
reporting created-but-not-filed if only creation committed. Such a partial copy
may be visible to its authorized owner in Inbox; do not report full success.
Retry/status must recover the same reserved ID and never duplicate content or
overwrite later edits. An uncertain commit reconciles original receipts before
continuation. Cancellation does not erase the source or automatically delete an
already-created copy. Refiling after target drift needs an explicitly refreshed
organization plan for that same copy, not a new source-copy request.

Use capability `item_copy_v1` only after preview/create/status, bounded payloads,
durable replay and full-payload verification are implemented. B must specify the
concrete DTO/error/limit/persistence seam and old-writer preservation before enabling
it; client keeps “需要代理支持” until both capability and per-item support exist.
One-item copy is sufficient for the first slice; multi-selection may later compose
separate receipts, not claim batch atomicity. Do not relax byte/dependency/revision
limits or omit content to manufacture a successful copy. Plans/progress, copied
Evidence and lineage belong in the synthetic backup/restore drill.

Acceptance covers move atomicity at fault boundaries, source/target CAS, destination
already containing some/all items, other memberships unchanged, trash/hidden nodes,
replay after later edits, and old writers preserving new receipts. Copy additionally
proves source/target edit independence, no source mutation, missing/hidden resources,
private lineage non-disclosure, sensitivity floors, import-dedup bypass only for
intentional copy, interrupted creation/filing recovery and backup/restore isolation.
Use synthetic data; production copy/sharing is not an acceptance fixture.

B implements after the current publication-date priority reaches a safe delivery
boundary, retaining the paused book-project responsibility. F implements client
organization UX and capability gating, coordinating shared IPC/gateway surfaces
directly with B. No second Store writer. This freeze grants local implementation/
synthetic verification, not deployment or real source disclosure. Copied-state
extensions touching F's annotations/passes remain unassigned, not implicitly owned.

## Sidebar Inbox and recoverable node deletion — 2026-11-07 freeze

The user explicitly requested: “左侧栏，Inbox 放到顶部，作为默认的采集导入入口。
左侧栏提供改名、删除操作。你直接 coding”. This authorizes implementing these
controls, not an agent selecting and deleting real libraries as a test. The
following additions are approved behavior, not a claim of deployed capabilities.
Existing rename authority/CAS remain unchanged. Inbox is a virtual view above
the library roots, not a new Group or a rename/delete target.

### Global Inbox query

Add `library_query.query.unfiled`, a boolean defaulting to false when omitted;
explicit null or non-boolean values reject. `groups_v1.query_unfiled:true`
advertises support. Missing/false means unavailable, never silently drop the
filter or approximate it from the current page. With true, select only currently
query-visible items having no membership in any currently visible active library
or folder. Use this ceiling's membership projection, never the global membership
table. A visible item belonging only to a hidden library is in the lower-ceiling
Inbox; hidden-only membership changes must not change that view or its counts.

Apply this predicate before sorting, paging, total and all facets, ANDed with
text, origin and other ordinary filters. Reject true combined with `group_id`,
`cluster_id` or `include_descendants:true`; this increment is a global Inbox,
not the future per-Group unclassified view. Bind the flag and effective visible
membership changes into the query view. False preserves existing behavior.
Retain global and per-library item-trash exclusions: filtering out a trashed
membership must not resurrect its item into Inbox. Item authorization remains
mandatory; absence of a visible membership is not a read grant.

No membership is created, moved or removed merely by opening Inbox. Unassigned
captures/imports appear naturally; native imports with explicit source memberships
retain those memberships and need not appear there. Filing into any visible active
root or folder removes the item from this global Inbox, even without a child-folder
assignment. This does not migrate existing Main/project libraries or supersede the
separate future per-Group Inbox semantics below.

### Node deletion and restoration

Approve recoverable removal of a selected library/folder subtree and its active
memberships, NOT permanent erasure. Canonical items, body, Evidence/Revision,
original import mappings and memberships outside the subtree are untouched.
Otherwise-visible items with no remaining visible active membership enter Inbox;
existing item-trash state must not be cleared or accidentally resurrect items.
Main is an ordinary user root: it may be renamed or recoverably deleted under the
same checks. Its name-based ordering pin is not an immutable identity or a reason
to recreate it automatically. A node with effective `read_only` is not deletable;
imported origin alone is not read-only. If any affected descendant is read-only,
or any affected node/item relationship is outside current write authority, reject
the entire operation without hidden names/counts or partial subtree deletion.

A delete confirmation must bind an authorized server preview of the whole affected
subtree/membership state, relevant item-trash restoration dependencies and target
identity to an opaque token. A target node's current `expected_version` alone is
insufficient: descendant edits do not currently advance every ancestor version.
Recheck current authority and the frozen impact under the Store writer lock at
commit; drift requires a fresh preview/confirmation, never silent token replacement.
The confirmation states that child folders are included, items are retained and
the organization can be restored. No permanent-delete action is approved.

Retain durable tombstone/restoration data for names, stable IDs, parent/source
identity, order and memberships, plus principal-scoped exact replay receipts.
Provide a reachable list/restore action, not just a receipt with no recovery path.
Restoration rechecks current permissions and deletion identity, preserves later
unrelated edits, and rejects unavailable-parent/identity conflicts rather than
silently reparenting or overwriting. Old delete replays cannot delete a restored
subtree again. Existing item-trash markers and their recovery paths remain valid;
deleting/restoring organization does not restore or delete those items implicitly.
Reserve recovery capacity; limit exhaustion rejects new deletes, not restoration.

The owner-local wire and v2 persistence seam below are approved for implementation.
`group_edit delete {id}` is not the approved request. Current v1 validates every
old receipt against an existing node; retain those nodes and receipts. Preserve
old replay behavior and demonstrate restart, crash/replay and old-binary rejection;
do not claim transparent rollback or delete recovery files.

Inbox and existing rename can ship independently of this deletion seam. Synthetic
acceptance covers four ceilings, hidden-only membership noninterference, all trash
combinations, pagination/total/facets and stale views; deletion additionally covers
deep-subtree drift, hidden/read-only descendants, shared items outside the subtree,
Main, request conflicts, restart, restoration and old-request replay. No real-data
deletion, migration, model invocation or new remote authority is granted by this
freeze. New reader batch/map production HOLD is unrelated and remains unchanged.

### Owner-local deletion wire and v2 persistence

Four owner UDS commands, with trusted local-owner identity and a narrowing ceiling,
are approved. Identity is derived from transport, never a client principal field;
the ceiling does not authenticate a remote caller. Payload/response bodies are:

| Command | Request fields | Success body |
| --- | --- | --- |
| group_delete_preview | group_id | group_id, name, confirmation_token, expires_at, affected_node_count, affected_membership_count, distinct_item_count, includes_descendants:true, items_retained:true, recoverable:true |
| group_delete | group_id, confirmation_token, request_id | group_id, deletion_id, outcome:deleted |
| group_delete_list | cursor (nullable) | group_deletions:[{deletion_id,group_id,name,parent_id,deleted_at}], next_cursor |
| group_delete_restore | deletion_id, request_id | group_id, deletion_id, outcome:restored |

These are separate commands, not new `group_edit` ops. Advertise owner
`groups_v1.node_delete:true` only when all four and recovery checks are ready;
do not put delete in `group_edit.write_ops` and invite clients to send the wrong
command. Old/missing capability means unavailable. The remote gateway must
explicitly report node deletion unavailable and reject all four remote routes
before preparation/forwarding. Its in-memory grant checks cannot stand in for a
Store-lock live grant. No remote delete enablement is approved; that requires a
separately verified trusted final authority gate. Existing remote reads, Inbox
query and unrelated capabilities are not disabled by this restriction.

Preview uses a short-lived, bounded random in-memory token, bound to library,
trusted principal/ceiling, physical subtree, memberships, ancestor authority,
ordering, relevant trash dependencies and item authorization metadata. Expires_at
is informational; use a monotonic expiry, invalidate on daemon restart, and never
extend on use. Counts describe the newly affected active nodes/relationships and
distinct items, not previously deleted descendants; the binding and authorization
checks still cover the entire physical subtree. New deletes cannot target an
already inactive node. Unknown/expired/drifted tokens do not write anything.

Exact committed replay precedes token-cache lookup but not current disclosure
authorization. Store only the necessary request digest, not raw bearer tokens.
Request identity is library/principal-scoped, binds command and normalized payload;
conflicting reuse rejects. Response outcome on replay is the historical receipt,
not a promise about current activity; clients refresh deletion list/tree/Inbox
rather than applying that historical outcome to current UI state. Preserve all
existing edit/import/trash receipts without silently renamespacing old identities.
Old edit/import replay must never reactivate tombstoned nodes. New ordinary edits,
membership additions and import ID reuse against inactive nodes reject.

`catalog/groups/state.json` v2 retains nodes, memberships and old requests and
adds deletions plus principal-scoped deletion_requests. One validated
durable_replace publishes deletion/restoration state, version changes and receipt
under the normal writer lock/generation invalidation protocol. No concurrent
second state-file write or item/group-trash mutation is part of that commit.
Keep the original array slots; restoring does not undo later root reorders.
Retained nodes/relations still count toward quotas. An independent receipt pool
reserves enough capacity for each outstanding restoration. Malformed tombstones,
missing referenced nodes, inconsistent coverage or receipts fail closed.

Independent overlapping deletion records are allowed: deleting a child, then its
parent, retains both identities. Restoring the parent clears only that deletion;
the independently deleted child remains inactive. Restoring a child whose ancestor
is still inactive rejects without consuming its restoration. Normal writers cannot
mutate the retained hidden subtree to evade these rules. List only outstanding
deletions for which the current caller has complete applicable write authority,
not just deletions created by that principal. Filter before pagination; return no
hidden counts/names. Bound pages to 50, use an opaque cursor tied to the authorized
list/ceiling; visible changes require refresh rather than skipping/duplicating rows.

Deleted roots retain an authorized path to existing group_trash list/restore using
their retained identity and memberships. These operations do not restore the root.
For an otherwise query-visible item, a trash marker on a retained ceiling-visible
membership continues suppressing it when there is no active visible non-trashed
membership. Without such a marker and without any active visible membership, the
item enters Inbox. A hidden retained membership/marker cannot change the lower
view; global item trash always wins. Restoring organization never clears markers;
explicit existing item-trash restoration may make the unfiled item visible again.

Read existing v1 without rewriting it. Do not upgrade on startup, preview, list,
ordinary legacy writes or exact replay: first successful explicit node deletion
may atomically write v2. Once v2 is present, every supported writer preserves it
and its recovery records. Old binaries must reject v2 through strict schema/version
validation; test an actual pre-change binary against a disposable v2 fixture,
including warm-query failure and attempted old writes leaving v2 intact. This is
fail-closed incompatibility, not a transparent downgrade. Do not roll production
back to a v1-only binary after v2 activation, strip fields, or restore an old whole
state snapshot over subsequent writes. Inbox-only release must remain v1-compatible.
Production migration/deletion is not a test step; synthetic validation precedes
capability activation and the user's specific subtree confirmation.

## Project-library direction — user clarification, 2026-11-03

This is the required next architecture, NOT a claim about the current runtime.
A Group usually serves a project. A research Group, for example, gathers external
literature for one project's research, development and promotion; it does not merge
any other personal database into the library.

- A Group is the boundary for loading, indexing, retrieval/AI context and future
  collaboration, not merely a filter applied after loading the entire library.
  Opening Group A must not require reading/reindexing Group B's item bodies.
  Unrelated Group B writes must not invalidate Group A's unchanged view. Shared
  items still invalidate every affected Group when their visible revision changes.
  Navigation metadata must not force full-text indexing. Cross-Group search is
  an explicit aggregate operation, not a prerequisite for opening one Group.
- A personal Main Group serves casual material; lack of a project assignment
  and lack of a folder classification are different states. Each Group has its
  own hierarchical folders and Inbox. Main is not an alias for all project data.
  Existing imported roots, source identities and many-to-many memberships remain
  intact; this clarification does not itself execute a canonical migration.
- An item may appear in multiple folders without duplicating its original text.
  Storage deduplication does not grant access to another Group's memberships,
  annotations or conversations. Group-specific access and synchronization remain
  separate from any shared physical content.
- One-click AI filing acts on the selected Group's Inbox and folder hierarchy.
  It is an explicit user operation, not automatic model inference during import
  or library startup; original source hierarchy/provenance must be preserved.
- Future shared Groups synchronize between collaborators' clients and support
  authorized creation, editing, deletion, search and AI conversation. Conversation
  synchronization is optional, including one-click sharing of selected conversations;
  sharing a Group does not silently share all private conversations. Membership,
  revocation, revision conflicts and the end-to-end encryption/key protocol require
  their own implementation and verification. HTTPS or local IPC is not proof of E2EE.

First delivery is local Group-scoped reading/indexing, followed by Group-scoped
Inbox/context behavior; collaborative synchronization is a later delivery. The
2026-11-03 deployed tree cache only repairs repeated navigation work. It still
builds a global, ceiling-specific in-memory index on a cold daemon and does not
satisfy the independent-Group loading requirement above.

## Library ordering and recoverable deletion — 2026-11-03

Chinese UI calls a Group a 库 and labels its navigation section 分库. Root order
is retained in the v1 nodes array; a user root named Main (case-insensitive) is
pinned first. Other roots, including imported roots, support `reorder_root`
with `group_id`, `expected_version`, and nullable `before_id`; the target must
be another non-Main root, or null for the end. This is not folder reparenting.
`agent_version.group_reorder` and the `groups_v1` write operation advertise it.
The gateway exposes `memex.library.groups.reorder` under edit scope, using an
authenticated-principal-namespaced request ID. Main's current pin is name-based,
not a permanent special identity across renames.

`group_trash_list {group_id,cursor,max_sensitivity}` returns `trash_items` and
`next_cursor`, up to 50 visible entries per page. `group_trash_edit` takes
`group_id,item_id,expected_revision_id,request_id,restore_plan,max_sensitivity`.
Null restore_plan deletes from that root; restoration supplies the exact plan_id
from its trash entry and the current item revision. Reads and writes require
visible membership; a new root-wide mutation additionally requires authority
over every affected membership in that root. Hidden relationships are not
disclosed in errors. Read-only imported roots can have local trash markers.

Markers and durable replay receipts are atomically stored in
`catalog/groups/trash.json` under the normal writer/snapshot lock. Canonical
body, revisions, Evidence and folder memberships remain unchanged. An item
stays visible in other roots; All excludes it only if all its visible memberships
are trashed. Removing a membership of a currently trashed item requires restoring
it first, retaining its restoration path. New deletes reserve receipt capacity
for all active restorations within the 65,536 receipt limit; exhaustion rejects
further new deletes, never consumes the reserved restoration slots. Exact
replay does not delete a restored item again or undo a later deletion.

`agent_version.group_trash` and `groups_v1.trash` advertise support. Gateway
`memex.library.trash.list/edit` enforces the grant ceiling and edit scope for
writes; clients do not supply a ceiling. Write transport failures are unknown,
not proof of rollback; clients read back and never automatically send a new
write identity. The UI restores entries and offers no permanent deletion.

Compatibility: older binaries ignore this new sidecar and may show trashed
items again; rollback to them does not preserve trash visibility. No canonical
data migration is needed. Trash is not revocation of direct item-read authority,
nor secure erasure, nor collaborative Group ACL. Production data must not be
used as a destructive test fixture.

## Organization and authorization

An item can belong to multiple groups and collections without duplicating its
canonical content. `zotero_group` identifies an imported Zotero library, not a
collection folder or remote write authority; `collection` initially retains its source hierarchy within that
library and also represents a locally created folder. Personal Zotero libraries
retain their own source namespace as well. `user` is a locally managed root group,
whose folders support the same multi-level nesting as imported collections.
`topic_cluster` is a derived Analysis view outside the directory tree, not a
replacement for any imported or manual membership.

Each root group has `parent_id:null`; every collection has exactly one parent
(its root group or another collection in that root) and a stable `root_group_id`.
Roots use their own ID as `root_group_id`. Cycles, orphan folders and multiple
parents are invalid. There is no product-level fixed nesting depth; operational
limits must report an explicit error, never truncate or flatten the hierarchy.
Thus multiple groups form separate rooted trees. Item membership is many-to-many
across nodes; the tree describes folders, not unique ownership or copies of text.
Imported nodes/memberships are native MemeX state. Local edits change that state,
not the immutable original import mapping; no write-back to Zotero is implied.

Design-facing node shape:

```json
{
  "id": "group_opaque",
  "name": "Health",
  "kind": "collection",
  "parent_id": "group_opaque_parent",
  "root_group_id": "group_opaque_root",
  "item_count": 12,
  "subtree_item_count": 18,
  "source": {"system":"zotero","library_key":"opaque","object_key":"opaque"},
  "owner_id": "owner_opaque",
  "sensitivity": "internal",
  "read_only": false,
  "version": "opaque"
}
```

`parent_id` and `source` may be null. IDs and source identity are stable and
namespace-qualified, never derived solely from names. Same-name groups from
different Zotero libraries do not merge. Names are display text, never paths.
`read_only` is the server's effective write permission, not a client-supplied ACL.
Imported origin does not imply read_only: authorized owners may rename imported
folders, create children and edit memberships exactly as for local folders. Actual
permissions may still make a node read-only. Generated clusters remain read-only
derived views. Local edits retain source identity/provenance and use version checks.
For tree nodes, `item_count` counts distinct authorized direct members;
`subtree_item_count` counts the distinct union of members of that node and its
authorized descendants. Neither sums child counts nor duplicates an item that
belongs to several folders. These navigation counts ignore the active text query;
query-result facet counts below apply the query's full filters instead.

Node and member reads require both group authorization and existing item
authorization. Membership cannot raise the caller's ceiling or lower an item's
sensitivity. `item_count`, facets, ancestor labels and cluster names expose only
authorized data; invisible nodes are not represented by zero-count placeholders.
Cross-owner moves and cross-library parent changes are not implicit operations.
Removing membership does not delete an item or its Evidence.

## Query and editing semantics

Proposed additions to `library_query.query`: nullable exact `group_id` and
`cluster_id`, combined with existing filters by AND. `group_id` may identify a
root group or any collection. With `include_descendants:false`, it selects only
direct members; with `true`, the backend selects the distinct union of the node
and its authorized descendants before filtering, sorting, pagination and facets.
`include_descendants:true` without `group_id` is invalid. Approved g2 retains the
direct-members view with a top-toolbar view switch; frontend sends false/true
explicitly. Omission retains false for compatibility, not a second UI setting.
The backend owns recursive selection/counting: frontend must not expand parent
chains and merge separately paginated queries. Cluster IDs
identify one immutable clustering run, never an unstable label or array index.
An obsolete run returns `stale_query_view`; the UI refreshes instead of mixing runs.
Query views bind group/cluster filters, `include_descendants`, hierarchy and
authorized membership versions and ceiling. Relevant hierarchy/membership or
authorization changes invalidate the view rather than mixing pages or counts.

Proposed result facets `groups` and `clusters` contain `{id,name,kind,count}` over
the complete authorized filtered result, not just the page. An item contributes
once per facet node even if imported repeatedly. A group facet's `count` is direct
membership; its additional `subtree_count` is the distinct subtree union within
that same filtered result. Cluster facets have no subtree semantics. Items expose authorized
`group_ids` and `cluster_ids`. Existing source/item_type facets keep their semantics.

Design may show list/tree, membership chips, local root-group and nested-folder
create/rename, membership editing and the include-descendants toggle. Folder
transfers follow the additive contract below; item-folder moves and cross-root
mapping/copy follow the item-organization freeze above. The implemented baseline follows;
new ops are not delivered by documentation alone. Clients check the
connected daemon's capabilities rather than infer support from this document.

## Folder-node transfers — 2026-11-11

User decision: dragging folders with ⌘ moves them; without ⌘ maps them. This
operation applies to active local folders within one physical Store, including
across root libraries. A root library is not a valid source; `reorder_root`
continues to implement root ordering separately.

Mapping creates an independent directory-subtree snapshot with new folder IDs
and memberships referencing the same canonical items. It copies no item body,
Evidence or revision and creates no dynamic alias. **Items or subfolders later
added to the source do not automatically appear in the mapped snapshot.** Changes
to the shared canonical item itself remain visible through either membership.
Moving preserves folder IDs and memberships and changes the selected top-level
parents, subtree root bindings and versions atomically. Inherited sensitivity is
preserved rather than lowered on detachment. Selecting a parent and its descendant
transfers that subtree once, not twice.

Enable only when `groups_v1.write_ops` explicitly contains `transfer_folders`.
The older generic `moves` flag and app version do not enable it. The owner UDS
request contains exactly one op:

```json
{"command":"group_edit","max_sensitivity":"private","request_id":"folder_transfer_example","expected_version":"target_parent_version","ops":[{"op":"transfer_folders","source_ids":["folder_source"],"target_parent_id":"folder_target","expected_versions":{"root_source":"source_root_version","folder_source":"source_version","folder_child":"child_version"},"moving":false}]}
```

`expected_versions` must exactly cover every source subtree node and all its
ancestors, using opaque versions from the tree. Freeze source tokens at drag
start; the outer token binds the chosen target parent at drop, when ⌘ is sampled. Empty or more than
256 distinct sources, root sources, self/descendant targets, inaccessible or
read-only/non-local nodes or ancestors, inactive/deleted descendants, invisible
canonical items and retained source/target-library trash members reject without
publishing any partial subtree. Any relevant source version/scope drift or target
version drift returns `stale_group_version`; never silently refresh a frozen write.
Mapping and moving retain existing node/member quotas and cannot grant remote ACL.

The existing Store writer lock serializes validation, mutation and one durable
`catalog/groups/state.json` replacement containing both state and the historical
request receipt. Existing v1/v2 state schema is retained; a transfer does not
activate v2, migrate state or write other business files. Restored deletion history
keeps its node IDs and receipts without constraining the now-active nodes' current
topology; outstanding tombstones still require their current subtree closure.
Same normalized request ID/op replays the original receipt without new nodes or
members; conflicting reuse returns `request_id_conflict`. Authorization is checked
again before replay, but later source edits do not change a historical receipt.
An unknown transport result must retry the same ID, op and frozen tokens; it is
not a new mapping. The desktop retains this pending identity in memory; process
restart does not constitute durable automatic retry/reconciliation support.
Transfer success returns a bounded `group_edit` receipt, without `group_tree`;
clients require a target-matching nonempty receipt and refresh the tree separately
with paginated reads. Read-projection failure or size cannot erase write success.
Generic `internal_error` / `result_too_large` after a transfer are uncertain, not
proof of rejection; retain the same pending request identity for reconciliation.

This source contract does not claim that a deployed daemon already advertises
the operation. Old daemons reject the new op; old clients continue using existing
writes, and old schema readers can read the resulting ordinary folders/members.

## Implemented owner UDS group_edit and capability

```json
{"command":"group_edit","max_sensitivity":"private","request_id":"request_example","expected_version":"node-version-from-read","ops":[{"op":"create_folder","parent_id":"group_example","name":"Health"}]}
```

`ops` must contain exactly one operation; empty/multi-op requests reject with
`invalid_group_edit`, never partially execute a batch. Supported operation shapes:

| op | Fields inside op | Top-level expected_version |
| --- | --- | --- |
| create_root | name, optional sensitivity (default private) | omitted or null; a supplied token is invalid |
| create_folder | parent_id, name | parent node's version |
| rename | id, name | renamed node's version |
| add_members | group_id, item_ids | target node's version |
| remove_members | group_id, item_ids | target node's version |

Versions are opaque strings returned by node reads, not the tree-view token or a
global groups version. create_folder advances the parent's version and creates a
child; refresh the parent before another write. Its receipt identifies the child,
not the parent. Unknown ops/fields reject; folder transfers and recoverable deletion
require their separately advertised capabilities, not these baseline writes.
The caller's ceiling cannot grant access to hidden nodes/items or create a root
above that ceiling. The local owner transport supplies authority; max_sensitivity
is not remote authentication. Permission/read-only checks remain effective.

Response includes `group_edit:{schema_version,group_id,version}` and `group_tree`
(the refreshed first page, not a promise of the whole tree). Store applies the
single operation and durable request identity under its writer lock. Same request
ID with the same normalized operation replays its receipt; conflicting reuse
returns `request_id_conflict`. Refresh response tree may differ from an old replay
receipt. A stale node token returns `stale_group_version`, without applying the
edit; client refreshes/reviews, never substitutes the current version silently.
Other bounded reasons include `group_read_only`, `groups_unchanged`,
`library_resource_not_found`, `group_limit_exceeded` and `group_request_limit_exceeded`.
Removing membership never deletes canonical content/Evidence.

Capabilities include the `groups_v1` feature; its object retains tree/query flags
and advertises `writes:true`, `moves:false`, and:

```json
{"write_ops":["create_root","create_folder","rename","add_members","remove_members"]}
```

Enable only operations present in this allowlist on the connected daemon. Missing
capability/write_ops means unavailable, not permission to guess an endpoint. These
writes do not advertise native Zotero conversion or topic-cluster execution.

## Native data import, preview and receipt

Import means canonical MemeX items with readable Markdown, original Evidence and
revision/provenance records, plus editable native folders. Creating only folder
shapes, bibliography cards, external file links or summaries is not success.
Regular bibliographic items retain their metadata as typed fields, with a
deterministic Markdown representation when no source body exists; explicitly mark
`metadata_only` rather than inventing a full text. Every supplied note and attachment
body must be processed, not hidden by a parent item's successful metadata import.

Notes become native `note` items and supplied document attachments become native
`document` items, each with its own source identity, Markdown revision and Evidence.
An optional read-only source relation `{parent_item_id,kind:note_of|attachment_of}`
preserves the Zotero parent link; standalone notes/documents have no parent link.
These are content relations, not directory parent_id. Parent links alone do not
duplicate text or silently add folder memberships; preserve explicit source folder
membership separately. Child bodies are not concatenated into every parent copy.
Fulltext can be read/queried as native content; UI may group related items, but a
list/count must distinguish bibliography, note and document items by item_type.

Keep all original attachment bytes with hash and source locator as immutable
Evidence; Markdown is the editable canonical content derived from them, not a
destructive replacement of originals. Edits create revisions, never mutate source
Evidence. Images/figures remain content-addressed local resources referenced from
Markdown, not lossy prose substitutes or remote-only image links.

Initial local conversion decision (implementation and acceptance still required):
- Markdown/plain text: preserve full text and encoding evidence; no truncation.
- Zotero HTML notes/snapshots: use the existing inert HTML-to-Markdown pipeline,
  preserving supplied local resource references; no scripts, remote image fetches
  or navigation. Import parsing is not permission to launch a browser.
- Text PDFs: use local Poppler text/layout extraction, page by page, then produce
  Markdown with page provenance. Poppler pdftotext/pdftoppm are present on this Mac;
  presence is not conversion-quality proof or a portable-install guarantee. Preserve
  page boundaries and original PDF; inspect multi-column ordering, tables, formulas
  and images against source pages before declaring conversion complete.
- Scanned/image-only PDFs or unreadable pages: mark `needs_ocr`, retain originals,
  and do not count as fully converted. Offline OCR is a follow-on conversion step
  with explicit tool/runtime and quality validation, not a cloud/model fallback;
  no OCR installation or inference is performed under this contract update.
- Other attachment formats require an explicit bounded local converter and
  format-specific proof. Missing, encrypted, corrupt or unsupported inputs retain
  per-resource reasons (`missing_source|encrypted|invalid_source|unsupported_format`)
  and block full-success claims. No silent skipping, invented text, password bypass,
  arbitrary linked-file traversal, converter plugins or external fetching.

Conversion records input hash, converter/version/options, output hash, page/resource
coverage and warnings. Native Markdown byte citations must not be confused with
original PDF coordinates. Resume retries only unfinished conversion/materialization
against the frozen input; completed imports and owner edits are not overwritten.

Import has a read-only preview before any new canonical writes. It identifies
source library/group keys, collections and parent links, item keys, current
matches, missing hierarchy and unsupported attachments. Initial import preserves
every source folder, including empty folders, original parent links and all item
memberships; there is no flatten mode or optional “preserve subfolders” setting.
Missing parents or cycles are surfaced for review, not silently repaired by
reparenting. A flat export without
group/collection identity cannot recreate that hierarchy: report missing source
metadata, do not infer it from titles/tags. Existing 1222 items are not reimported
to manufacture grouping; link only with verified source identity, otherwise hold
ambiguous associations for review. Missing notes/attachment content linked to an
existing source item may be imported once with their own source keys; do not
replace the existing parent's edited content. Replay of the same snapshot reuses
its original receipt; a later source snapshot requires a new preview and explicit
conflict resolution, never resets locally edited folders/memberships to source.
Retain originals/backups and use a consistent
export or closed-source snapshot, never hot-copy a live Zotero database.

Progress design shape:

```json
{
  "import_id":"import_opaque", "state":"preview_ready", "stage":"match",
  "processed":12, "total":12,
  "created":0, "linked_existing":10, "skipped":0, "failed":0,
  "needs_review":2, "can_cancel":true, "reason":null,
  "resources":{"total":24,"converted":0,"metadata_only":0,"pending":24,"needs_review":0,"failed":0},
  "receipt_id":null
}
```

States: `queued|running|preview_ready|needs_review|succeeded|partial|failed|cancelled`.
Stage: `scan|match|convert|groups|items|memberships|verify`. `total` may be null until scan
completes. Receipts retain source fingerprint, original-to-local identity mapping,
per-item outcome and initial source hierarchy, separate from subsequent native
edits. Top-level counters count source records; resources count conversion units
(item text, note or attachment), not folders or pages. A resource has one current
outcome; metadata_only cannot stand in for a supplied attachment body. Receipt rows
carry source key, native item/revision IDs when committed, input/output hashes,
conversion outcome and reason. Only all required supplied content converted and
verified can yield succeeded; unresolved conversion yields needs_review/partial
or failed, never success merely because folders/metadata were created.
Counts never imply an all-or-nothing
transaction. Resume reuses the same import identity and verified source snapshot.
Cancellation stops future work only where the returned flag permits; committed
items remain in the receipt. Raw private paths/titles do not enter shared logs.

## Topic clustering

First implementation may use deterministic local text features; model use stays
behind the existing provider HOLD. A cluster run records algorithm/version,
parameters, authorized input item/revision/hash set, creation time and generated
memberships in derived Analysis. It does not mutate original classification,
source membership, Evidence or item text. Small/ambiguous sets may be unassigned;
no promise that every item gets a meaningful topic. Generated names are labeled
as suggestions. Promotion to a local group is a separate explicit user action.

Design-facing cluster adds `run_id`, `analysis_id`, `algorithm`, `stale` and
`read_only:true` to the node shape, with `parent_id:null`; tree-only
`root_group_id`/`subtree_item_count` are omitted. It is never a folder parent or
child. Revisions/sensitivity changes invalidate the
affected visible result; do not reuse a cluster label derived from now-hidden
Private material in an Internal session. No merging with other personal systems' data.

## Implementation and release gates

Implement canonical organization/membership contracts and permission/replay tests
first, then derived query projection, import mapping and deterministic clustering.
Shared DTO/manifest assembly is serialized by backend owner. No real Zotero/MemeX
migration, installed-service update, provider call or deployment follows from this
draft; source inventory and actual import write scope must be established first.

## Owner-local podcast archive Store seam — 2026-11-08

This is a trusted local-owner Rust API, not a new GroupEditOp, remote route,
capability advertisement, queue or permission grant. The caller authenticates the
local owner; `ceiling` only narrows visibility. The groups module provides:

```rust
archive_podcast(library: impl AsRef<Path>, request: PodcastArchiveRequest,
                ceiling: Sensitivity) -> anyhow::Result<GroupEditReceipt>
// PodcastArchiveRequest:
// item_id, expected_revision_id, request_id, target_group_id,
// expected_target_version: String; sources: Vec<PodcastArchiveSource>
// allow_root_or_nested_target: bool (serde default false; omitted when false)
// PodcastArchiveSource: group_id: String, expected_version: String
```

Use exact IDs, never folder names or a virtual Inbox identity. With omitted/false
`allow_root_or_nested_target`, target remains an active owner-writable collection
directly beneath its library root. True permits a physical root (`user` or
`zotero_group`) or a collection at any depth. Under the same writer lock, validated
state must establish an acyclic complete parent chain ending at the declared root;
target and every ancestor including the root must currently be visible, active,
owner-local and writable, including on replay. Imported origin is not itself a
permission or a prohibition. The flag grants no transport authority.

Zero to two distinct explicit sources may still only be the target's root itself
or that root's direct child collections; target cannot be a source. Nested and
cross-root sources reject even when the flag is true. Current sources must also
be visible, active, owner-local and writable. The explicit `selected_group`
destination caller uses true with empty sources: only add the selected membership,
preserving every existing membership. The flag alone never infers removal sources.
The item must be currently readable and neither globally trashed nor trashed in
this root. Membership in other roots is preserved, not a sharing grant.

Under the existing Store writer lock and pending-journal recovery, a new request
checks the exact item revision, target CAS, every source CAS and every direct source
membership before any publication. One durable `catalog/groups/state.json`
replacement adds target membership, removes only the listed memberships, advances
only changed nodes, increments groups generation and records the request receipt.
An already-present target is valid and does not advance its node version. Empty
sources permit filing from virtual Inbox without inventing removable membership.
Even an entirely already-filed request records a replay receipt.

Source order is normalized; duplicate sources reject. The digest is domain-separated
from ordinary group edits and uses the existing request record/quota and unchanged
v1/v2 schema. False is omitted from serialized requests, retaining the exact legacy
request bytes/digest and replay identity; true is serialized and bound into the
digest. Existing Rust request literals must explicitly supply false for legacy
behavior. Current authority, active state and trash checks precede exact replay.
Exact replay returns the historical target receipt without applying again: original
CAS/revision values bind the original write, not a demand that current versions or
memberships still match. Later removal/revision/version edits are not rolled back;
callers must refresh current membership/version before reporting current filing or
issuing another operation. Conflicting request reuse rejects `request_id_conflict`.
New-write conflicts use `stale_group_version`, `stale_item_revision` or
`stale_group_membership`; invalid shape uses `invalid_group_edit`; existing hidden,
read-only, trash and quota errors remain applicable. An I/O error may be an unknown
commit: retry the same frozen request, never invent a replacement identity.

No migration or v2 activation is introduced. Recovery uses existing durable atomic
replacement and receipts; backup/restore requires a stopped, complete Store copy,
not replacing groups state alone over later writes. Synthetic tests cover exact
same-name disambiguation, all CAS rejection without partial state, retained other
memberships, replay after later edits, authority/trash rejection and full-copy restore.
