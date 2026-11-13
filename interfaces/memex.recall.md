# Sensitivity-aware Lexical Recall V0

## Summary

从同一 MemeX library 的 verified current ItemRevision 与 validated AnalysisRecord 中返回
透明、可复现的关键词命中。接口明确标记 `retrieval_mode = lexical_v0`；它不是语义
搜索。

## Command

```bash
memex recall \
  --library <library> \
  --query <text> \
  --max-sensitivity public|internal|private|restricted \
  --limit <1..100> \
  --format json|markdown
```

`max-sensitivity` 必填；默认 limit 是 20。

## Fixed scoring

- normalized exact phrase 在 title、topic 或 body 出现：100；
- 每个命中 title 的 unique term：20；
- 每个命中任一 topic 的 unique term：10；
- 每个命中 body 的 unique term：2；
- score 是四项之和，0 分不返回；
- matched terms 保留 query 首次出现顺序；
- 排序为 score 降序，再按 target kind 与 ID 升序；
- snippet 是 body 前 320 个 Unicode scalar values。

JSON 返回每个分项；Markdown 从同一 typed `RecallReport` 渲染。

## Sensitivity boundary

Item 使用 Item/Revision sensitivity 的最大值；Analysis 使用 record sensitivity。超过
ceiling 的对象在访问 title/body、评分、snippet 与计数前被排除。Link rationale 不属于
Recall candidate。

此 ceiling 只是一条 library query 规则，不等于 Web 身份授权。公开站点仍必须经过 P5
的显式 publication allowlist；私密后台仍必须经过 P6/P7 的鉴权 adapter。

## Guardrails

- list/read 任一 canonical Item 或 Analysis 损坏、symlink、noncanonical 时 fail closed；
- 不信任 derived indexes；
- 不持久化 query/hits，不调用网络、模型、embedding 或 vector database；
- query 最大 4096 UTF-8 bytes，limit 为 1–100。

## Missing proof

未证明中文分词质量、语义召回率、生产规模性能、跨 library 搜索或 Web authorization。
