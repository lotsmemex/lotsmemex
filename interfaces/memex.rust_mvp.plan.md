# memex.rust_mvp.plan

## Summary
为 MemeX Rust CLI/library MVP 输出实施计划、crate 边界与测试门禁。

## When to use
准备实现 MemeX Rust MVP、拆 crate、写 CLI 或安排 TDD 任务时。

## Do not use when
需要直接发布桌面 App、调用外部服务、远端 push 或安装系统依赖时。

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
