# memex.item_read

## Summary

通过稳定 `item_id` 读取 canonical Item 与当前 revision。JSON 与 Markdown 均来自同一
revision，不建立第二套事实源。

## Command

```bash
memex show \
  --library <library-root> \
  --item <item-id> \
  --format json
```

`--format markdown` 返回对应 `index.md`。

`--format portable-json` 返回同目录的 verified `index.json` projection。它不复制 Markdown
正文，只保存 `index.md` 的相对路径、byte size 与 SHA-256，以及 title、source、captured time、
translator、typed metadata/provenance 和 enriched attachment refs。Canonical authority 仍是
`item.json + revisions/<revision_id>.json + Evidence`；`index.md/index.json` 都可由 exact replay
重建。每个 portable item directory 还固定包含 ordinary `attachments/` directory；无本地附件时
该目录为空，Evidence-backed 图片使用 `attachments/sha256-<digest>.<ext>`。

## Revision-bound context (local trial)

`memex context-read --library <root> --item <id> --expected-revision-id <revision>
--start-byte <start> --end-byte <end> --max-sensitivity <public|internal|private|restricted>`
returns one JSON `ContextSlice`. It does not change `show`, `recall`, or their output formats.
The owner-only UDS also accepts these fields, using `item_id` instead of the CLI's `--item`:

```json
{"command":"context_read","item_id":"item_example","expected_revision_id":"rev_example","start_byte":1,"end_byte":8,"max_sensitivity":"public"}
```

- The range is nonempty, half-open, measured in UTF-8 **bytes of the revision's
  `content_markdown`**, not characters, rendered DOM, UTF-16, original HTML/PDF, or a translation.
  Both endpoints must be code-point boundaries. The maximum range is 16 KiB; no silent truncation.
- `input` carries `item_id`, `revision_id`, SHA-256 of the serialized revision and Evidence IDs.
  `content_sha256` covers the **whole Markdown**, `total_bytes` its length; `content_markdown`
  is the exact requested substring. `source_url`, `canonical_url`, and effective `sensitivity`
  accompany it. `origin=machine_projection`, `coordinate_space=revision_markdown_utf8_v1` and
  `schema_version=1` prevent confusing derived text with raw Evidence.
- The current revision is verified under the Store snapshot lock. Pending Store journals may be
  recovered before this read and the lock file may be created; unlike `show`, this is not a
  filesystem-no-write operation. No new content revision is created by the context operation.
  A successful response identifies the revision at that snapshot, not a lease against later edits.
- A missing or above-ceiling item returns `context_resource_not_found`. The ceiling is mandatory
  and checked before revision mismatch: denied callers cannot discover the current revision.
  An authorized but superseded reference returns `stale_item_revision`, never new text under an
  old anchor. Re-reading historical revisions and translated coordinate mappings are not implemented.
- Other reason codes: `invalid_context_request` (empty identifiers/range or reversed range),
  `context_range_too_large`, `invalid_context_range` (out-of-bounds/non-UTF-8 boundary),
  `context_read_failed` (storage/integrity failure), `result_too_large` (JSON including source
  metadata exceeds 192 KiB). CLI errors produce no partial stdout and a nonzero exit; UDS uses
  `ok=false`/`reason_code`. Malformed UDS JSON retains the existing sanitized error behavior.
- Successful UDS responses add `context`; it is **absent**, not null, on errors and old commands
  so existing strict response decoders keep working. New clients against an old daemon must treat
  unsupported `context_read` as unavailable, not fall back to an unbounded or unverified read.
- The caller-selected ceiling only narrows a local owner's access. It is not remote authentication.
  No HTTP/MCP/provider adapter is exposed by this increment; any future remote adapter must derive
  the ceiling from its authorization, not trust a request-supplied value. URLs and source text may
  be sensitive and must not be forwarded to models, logs, or remote services without authorization.

## Guardrails

- `item_id` 只接受 MemeX 生成的安全 `item_*` ID。
- 拒绝路径分隔符、`..` 与控制字符。
- 只读 library，不访问外部 raw/blob 或网络。
- `portable-json` package read 拒绝 missing、tampered 或 symlink projection，以及 unsafe/missing
  `attachments/` directory；`show` read path 不自动修复（`context-read` 的 Store journal recovery 例外见上文）。
- `index.json` 是 local read projection，不是 public export 或 sync authorization。它保留 source /
  attachment URL 与 metadata provenance；private/restricted package 不得因为存在 `sensitivity`
  字段就同步到不受信任的 WebDAV、runner 或第三方服务。
