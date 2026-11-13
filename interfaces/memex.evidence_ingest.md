# memex.evidence_ingest

## Summary

校验本地 SourceProfile/CaptureEnvelope bundle，并将 Source、CaptureRecord、Evidence、
Item revision、EvidenceLink 与 receipt 幂等写入同一 canonical library。

## Inputs

- `source_path`：bundle 内 `source.json`。
- `capture_path`：bundle 内 `capture.json`。
- `library_root`：本地 library 目录。

## Command

```bash
memex ingest \
  --source <bundle/source.json> \
  --capture <bundle/capture.json> \
  --library <library-root>
```

## Output

stdout 是 `IngestReceipt` JSON，包含 capture/evidence/item/revision IDs 与
`idempotent_replay`。重复提交同一 adapter/external capture key 不创建新对象。

## Guardrails

- source 与 artifact 必须位于 capture bundle root 内。
- 拒绝绝对 locator、`..`、symlink escape、未知 schema、非 RFC3339 时间、size 或
  SHA-256 不符。
- 不调用浏览器、登录态或网络，不打印正文。
- catalog 是最后写入的可达性 commit marker；Phase 1 不提供进程崩溃后的 orphan GC。
