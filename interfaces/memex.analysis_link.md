# Explicit Analysis Link V0

## Summary

保存一条由人或 Agent 明示的 analysis relation。source 固定为 verified
`AnalysisRecord`；target 可以是另一条 analysis，或 verified current ItemRevision。
MemeX 保存 assertion 与 provenance，但不声称关系由系统自动推断。

## Command

```bash
memex analysis link \
  --library <library> \
  --request <link-request.json>
```

## Relations

闭合枚举：

```text
supports
contradicts
corroborates
contextualizes
derived_from
related
```

V0 不生成反向边，也不从正文自动提取关系。

## Endpoint contract

- `from_analysis_id` 必须指向通过 canonical validation 的 analysis record；
- analysis target 不得带 revision ID；
- item target 必须带当前 exact revision ID；
- effective sensitivity 是两个 endpoint 的最大值；
- prepare 与 commit lock 内都会重验 endpoint 和 deterministic identity。

## Output and replay

stdout 是 privacy-safe `LinkReceipt`，包含 link ID、status、sensitivity、source 与 target
IDs；不回显 rationale。

canonical JSON 位于：

```text
<library>/analysis/links/<link_id>.json
```

相同 assertion exact replay；rationale、relation、endpoint 或 provenance 改变都会产生
新的 deterministic link ID。

## Guardrails

- request、analysis/links directory、link file、root/ancestor 与 lock symlink fail closed；
- tampered、noncanonical 或异物 link entry 使 read/list/replay fail closed；
- 不查询模型，不写 Item/AnalysisRecord，不修改旧 graph/runtime；
- Link V0 不进入 Recall candidate，避免把 rationale 当作检索正文。

## Missing proof

未定义 link supersedes/delete、图遍历、真实 graph 迁移或生产并发规模。
