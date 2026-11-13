# Owner-local podcast RSS metadata fill v1

Rust entry point (not exposed directly to remote clients):

```rust
memex_store::podcast_metadata::fill_podcast_metadata(
    library: impl AsRef<Path>,
    request: PodcastMetadataFill,
    rss: &[u8],
    max_sensitivity: Sensitivity,
) -> anyhow::Result<ItemMetadataUpdateReceipt>
```

`PodcastMetadataFill` contains `item_id`, `expected_revision_id`, `request_id`
(all strings), and `source: PodcastRssSource`.

Source fields:

* `feed_url`, `episode_url`, `guid`, `rss_sha256`: strings. SHA-256 is prefixed
  `sha256:`. The intake extractor currently returns bare hex: the worker adds
  the prefix. Episode URL must exactly equal the stored source or canonical URL.
* `source_start_byte`, `source_end_byte`: usize, half-open RSS item byte range.
* `published_date: Option<PartialDate>`, `published_raw: Option<String>`,
  `published_instant: Option<String>`: all present or all absent. Worker copies
  `published_raw` from the extractor's `published_date.raw`. RFC 2822 raw date,
  RFC 3339 instant and the source-offset calendar day must agree.
* `duration_seconds: Option<u32>`, `duration_raw: Option<String>`: both present
  or absent; positive seconds, `mm:ss` or `hh:mm:ss`, checked for overflow.

The trusted worker owns fixed-feed network policy and unambiguous XML parsing
(including episode GUID/link selection). Store does not depend on intake or
fetch a URL. Store checks whole RSS hash, UTF-8 item range, presence of raw
tokens (literal/CDATA or standard escaped text), exact item URL, date and
duration consistency. This is not an arbitrary untrusted RSS parsing API;
numeric XML entity spellings not represented literally are not supported here.

Only absent publication date (both typed and legacy projection absent) and
duration may be filled. Equal existing values retain their original provenance;
different existing values abort the entire request with
`podcast_metadata_conflict:published_date,duration_seconds` (only conflicting
fields appear). No changes returns `metadata_unchanged`; stale CAS returns
`stale_item_revision`; changed payload under an existing request ID returns
`request_id_conflict`. Request IDs share the existing metadata journal namespace.

Values use `IntakeProvided`, provider `memex.podcast-rss`, version `1`, with a
source-record checksum reference, never `OwnerEdit`. Strict activity
`podcast_rss_metadata_fill` preserves title, all other metadata, sensitivity,
body, Evidence and attachments. Reader inheritance admits only this verified
metadata-only transition, with existing document/edition/body equality checks.

Original RSS bytes and the source binding JSON are immutable, independently
retained under `catalog/podcast-rss-sources/{sha256hex}.{rss,json}`. The activity
binds the JSON checksum; JSON binds the RSS checksum and byte range. Chain reads
revalidate both. These files are audit sources, not item attachments/Evidence.
Backup/restore must copy them along with the complete library while writes are
stopped. Revision/current-pointer/receipt atomicity and recovery reuse the owner
revision journal; unreferenced immutable preparation may survive a failed write.

Replay checks current visibility and both historical revision sensitivities
before returning the original receipt. The current chain integrity check still
applies. `duration_seconds` is optional and omitted when absent, preserving old
serialized metadata and revision digests without migration. Older readers that
do not understand the new activity cannot validate newly enriched chains;
rollback requires a pre-enrichment backup or a compatible reader, not deletion
of source records or rewriting revision history.
