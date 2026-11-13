# Source Registry V0

## Summary

将显式 legacy profile 投影为可回放的 typed canonical source registry。

## Ownership

MemeX 只拥有已导入的 registry；旧 registry 保持只读。

## Commands

    memex source import-legacy --package-root <root> --profile <relative.json> --library <library>
    memex source list --library <library> --format json|markdown
    memex source show --library <library> --source <source_id> --format json|markdown

## Inputs

仅接受显式相对 profiles；最多 1000 个，每个最多 1 MiB。

## Canonical layout

只写 library/registry 下的 current、immutable revisions 和 Markdown indexes。

## Import report and status

报告含 input_files、preflight、summary 与 source rows。状态为 created、revised、replayed、failed、unattempted；报告只含摘要与字段名。

## Query

JSON 是 Agent 合同；Markdown 面向人工 readback。

## Guardrails

拒绝 traversal、caller-controlled root/ancestor/component symlink、credential keys、无效 URL/userinfo 和无效列表；macOS 仅 UID0 的 /var→private/var、/tmp→private/tmp、/etc→private/etc 系统 alias 例外。

## Failure and replay

全量 preflight 早于首次写入；current.json 成功写入即 durable commit。其后 source/global index 失败仍返回实际 status，并附 source_index_projection_pending 或 global_index_projection_pending operational warning；partial failure 报告已提交、failed 与 unattempted。replay 先核验 current 与 immutable revision 精确字节一致，再尝试修复 indexes。

## Compatibility

无 runtime cutover、自动扫描或旧 registry 写入。

## Missing proof

未使用真实 profile 或生产 runtime。
