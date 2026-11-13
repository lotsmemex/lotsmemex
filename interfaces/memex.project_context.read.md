# memex.project_context.read

## Summary
读取 MemeX 项目定位、边界、状态、文档入口与上下游关系。

## When to use
需要判断 MemeX 是否承接某个知识管理、translator、schema、Rust MVP 或资料入库任务时。

## Do not use when
需要真实抓取网页、调用外部 API、读取用户私有资料库或修改运行态服务时。

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
- `none`。

## Approval boundary
- approval_required: `false`。
- 涉及真实抓取、外部 API、用户私有 Zotero/附件库、远端 push、删除旧目录时必须另行确认。

## Canonical docs
- `README.md`
- `PROJECT_STATE.md`
- `docs/specs/memex-dev-system-rebuild-2026-04-30.md`
- `docs/architecture/translator-orchestrator-bridge-v1.md`
- `docs/plans/2026-04-30-rust-mvp-plan.md`
