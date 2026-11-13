# Item cover v1 — Amp podcast increment

Status: Store and remote read implementation; not a deployed remote capability.
Owner: backend B. F owns display and the future tiled view; this increment does
not implement that view. URL-job enrichment remains gated until the full worker
and zero-generation recovery tests pass.

## Independent resources and compatibility

Cover publication never edits the item/revision, canonical bytes, Evidence,
attachments, reader annotations or AI translations. No relaxed metadata revision
inheritance exception is needed for a cover. Existing clients ignore the new
`catalog/item-covers` directory and continue to read the same item.

The directory is part of full-library, stop-writes backup and restore:

- `<item_id>.json`: schema_version=1, item_id, current cover, request receipts.
- `resources/<sha256>`: immutable original raster, precomputed thumbnail, and
  exact original RSS response. No absolute source filesystem paths.

Resources are durable before a single atomic state+receipt rename under the
existing Store writer lock, with directory fsync and generation invalidation.
An interruption before rename leaves only unreferenced immutable resources;
after rename the original request reconciles to its saved receipt. Do not retry
with a new request ID to resolve an unknown publication outcome.

## Owner-local write boundary

`covers::put_cover` accepts `CoverEdit`, image bytes and the exact RSS bytes.
It is a trusted worker API, not a client upload or arbitrary URL fetch endpoint.
The intake worker must verify RSS episode identity, episode-to-video mapping,
fetch permissions and network policy before calling it. Client hints are not
evidence. Store additionally checks the episode URL equals the bound item's
source/canonical URL, validates the RSS digest, and retains the bytes.

CoverEdit: item_id, expected_revision_id, expected_cover_version (null for absent),
request_id, source {episode_url, youtube_video_id, image_url, rss_sha256}.
Request identity is scoped to item_id + request_id and binds every input field,
exact original image digest/size/dimensions, and RSS digest. Reusing it with any
different input fails `request_id_conflict`. Replay returns the historical result,
never overwrites a later cover, and rechecks current and historical permissions.
New publication CAS-checks both item revision and cover version.

Current item visibility/trash and source revision sensitivity remain required;
the saved sensitivity is a floor, not a grant. Chunk reads recheck these gates,
source binding and resource hashes. No filesystem locator is exposed as a read
capability. A stale cover version fails rather than switching silently.

## Raster and fetch limits

Only decoded JPEG, PNG or WebP, at most 5 MiB and 20,000,000 pixels. MIME/header
recognition alone is insufficient: decode must succeed before persistence.
Decode allocation budget is 100 MiB. Reject missing-image-size placeholders
(selected YouTube variants must be at least 320 × 180).

The Store URL-shape check permits only HTTPS `i.ytimg.com/vi/<11-character-id>/`
and maxresdefault.jpg, hqdefault.jpg, mqdefault.jpg; no credentials, query,
fragment, alternate host or alternate port. This is NOT itself SSRF protection.
The fetching worker must use the existing DNS-pinned transport, reject non-public
addresses on every hop, enforce the smaller 5 MiB body limit, and check each
redirect against the same source policy. Bounded fallbacks do not turn a failed
or placeholder image into success. Cover failure is partial completion, not
permission to rewrite item text or retry model inference.

## Read DTO and future thumbnail seam

Cover: item_id, version, source_revision_id, source, raster, thumbnail, sensitivity.
Each raster descriptor contains sha256, media_type, width, height, byte_size.
Thumbnail is precomputed PNG with longest edge at most 320, preserving aspect
ratio. List clients need not decode original images or trigger resize work.

- `covers::cover_batch(library, item_ids, ceiling)`: at most 50 item IDs, returns
  one optional Cover per requested position. Hidden, trashed, missing and no-cover
  items all yield null; no hidden counts. Integrity failures are errors, not null.
- `covers::cover_chunk(library, item_id, version, thumbnail, offset, ceiling)`:
  current-version-only, at most 96 KiB per chunk; returns media_type, sha256,
  total_bytes, offset, next_offset, bytes. No remote write is implied by read support.

F/B agreed remote shape: commands `item_cover_batch` and `item_cover_chunk`,
using existing library read routing, scope and error sanitization. IPC encodes
`bytes` as base64; other chunk fields pass through unchanged. Advertise the
separate `item_cover_v1` capability only when both daemon and gateway are wired
and verified. Clients do not issue these requests without that capability.

Remote methods: `memex.library.item_cover_batch` takes `{item_ids:[...]}` and
returns the ordered nullable Cover array directly; `memex.library.item_cover_chunk`
takes `{item_id,version,thumbnail,offset}` and returns
`{media_type,sha256,total_bytes,offset,next_offset,data_base64}`. The owner IPC
response wraps either result in `item_cover` (omitted for every other command).
Gateway session.info exposes `item_cover_v1: available|unavailable`, available
only if the daemon advertises that exact feature. The authenticated session,
never request parameters, supplies the sensitivity ceiling. Both reads use the
bounded Store read lane, not a queue/model worker or a new write endpoint.

Reader uses original (`thumbnail=false`), assembles until next_offset=null and
verifies SHA256 before displaying above canonical text. On stale_cover_version,
repeat batch and restart at offset=0; never concatenate different versions.
List/future tile clients use only thumbnail. Null means no visible cover, with
no placeholder count. Batch/receipt success alone does not prove every raster
resource is intact; backup acceptance reads both original and thumbnail bytes.

Limits: state <= 1 MiB; <= 256 retained requests per item (no silent eviction),
RSS response <= 2 MiB, episode URL <= 2048 bytes. IDs use existing Store validators.
Failures include invalid_cover_request/image/offset, cover_placeholder,
cover_source_mismatch, stale_item_revision, stale_cover_version,
request_id_conflict, cover_request_limit, cover_unavailable and existing
library_resource_not_found. The remote boundary must sanitize internal I/O errors.

## Acceptance still required before production

Store CAS/replay, both sides of publication interruption, whole-library moved
restore, current/trash/history access, actual decode limits and hash/symlink
failures. Whole integration must additionally prove E5 translation/marks survive,
source and redirect verification, plugin reads, exact target membership and
durable zero-generation. Passing Store tests alone does not enable the feature
or authorize daemon replacement.

## WeChat article source extension

`source` accepts exactly one of two complete, untagged shapes: the existing
podcast four-field object, or `{article_url,image_url,html_sha256}`. Mixed,
missing or unknown fields are invalid. All three WeChat values are nonempty
strings; `html_sha256` is the lowercase 64-hex digest of the exact frozen HTML.
The podcast serialization order and receipt hash domain stay unchanged; no
existing state or receipt migration is required.

Only a public HTTPS `mp.weixin.qq.com/s/<id>` article can own this source.
The trusted worker chooses a unique `og:image`, otherwise the first article
body image, from admitted frozen HTML, never a navigation fallback. Images
are limited to HTTPS `mmbiz.qpic.cn` and known `mmbiz_*`/`sz_mmbiz_*` raster
paths. Every request/redirect repeats exact image identity and public-only DNS
checks, with pinned transport, MIME and 5 MiB decoded-size limits. No cookies,
session or arbitrary URL proxy is introduced. HTML evidence is at most 8 MiB;
WeChat covers require at least 64×64 pixels. Unsupported raster encodings fail
closed. Existing revision/cover CAS, replay authorization, trash/sensitivity
checks and immutable resource publication apply unchanged.

This is backend capability, not proof that an article's anonymous endpoint
returns its body. Login/challenge responses must not become article or cover
Evidence, and storage success is separate from client rendering acceptance.
