# Legacy Shadow Import V0

## Summary

只读地把明确选择的 Legacy Normalized JSONL 转为 MemeX shadow capture；不改变旧 runtime。

## Inputs

package root、相对 meta 路径、一个或多个相对 JSONL 路径，以及隔离 library 路径。

## Command

    cargo run -p memex-cli -- shadow-import-legacy --package-root examples/phase2-legacy-shadow --meta run.meta.json --items source-a.jsonl --items source-b.jsonl --library /tmp/memex-shadow

## Output

stdout reconciliation JSON 仅含 hash、canonical identity、字段名、状态和汇总；不含 title、content、URL、author 或未知字段值。

## Guardrails

不会扫描目录或读取 meta 的 file 字段。所有路径、大小、JSON、来源、identity、URL、时间和嵌套凭据键在首次 store 写入前完成验证。

## Failure and replay

preflight 失败时零 library 写入。store 失败时 stdout 报告 committed、failed 与 unattempted；重跑使用 idempotent replay。

## Ownership

MemeX 持有 adapter、转换、store 与 report；旧 capture runtime 持有路由、登录与原 artifact。

## Missing proof

仅以合成 fixture 验证；不包含真实旧 artifact 或生产 handoff 证明。
