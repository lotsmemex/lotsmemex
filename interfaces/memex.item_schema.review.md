# memex.item_schema.review

## Summary
复核 MemeX item metadata / Markdown representation / export 兼容字段。

## When to use
需要检查 item schema、Markdown 表示、CSL/BibTeX/RIS/Zotero 兼容字段是否合理时。

## Do not use when
需要真实迁移 Zotero 数据库、读取附件库或导出用户私有资料时。

## Inputs
- `query`：任务问题或目标。
- `artifact_path`：可选，capture artifact / schema / plan 文件路径。
- `output_path`：可选，需要写入计划或复核报告时的目标路径。

## Output
- `status`：`ok` / `needs_clarification` / `blocked`。
- `matched_docs`：读取或命中的事实源。
- `recommendations`：建议或下一步。
- `written_paths`：若有写入，列出本地路径。

## Side effects
- `file_write`。

## Approval boundary
- approval_required: `false`。
- 涉及真实抓取、外部 API、用户私有 Zotero/附件库、远端 push、删除旧目录时必须另行确认。

## Canonical docs
- `README.md`
- `PROJECT_STATE.md`
- `docs/specs/memex-dev-system-rebuild-2026-04-30.md`
- `docs/architecture/translator-orchestrator-bridge-v1.md`
- `docs/plans/2026-04-30-rust-mvp-plan.md`
