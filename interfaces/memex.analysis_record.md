# Analysis Record V0

## Summary

把人或 Agent 明示编写的分析内容绑定到同一 MemeX library 中已验证的 current
`ItemRevision`。JSON record 是不可变 durable fact；Markdown 和 indexes 是由同一
typed record 生成的人工 readback。

## Command

```bash
memex analysis add \
  --library <library> \
  --request <analysis-request.json>
```

查询使用：

```bash
memex analysis list --library <library> --format json|markdown
memex analysis show --library <library> --analysis <analysis_id> --format json|markdown
```

## Input contract

request 必须是调用者显式指定的普通 JSON 文件，不能是 symlink，最大 1 MiB。它包含：

- `analysis_kind`: `claim|summary|note|synthesis|translation`；
- title、Markdown body、topics；
- 1–64 个唯一 current `item_ids`；
- 可选 requested sensitivity；
- RFC3339 时间与 actor/tool/config provenance。

外部生成器提供摘要、标题译文或全文译文时，可以附带 `generated_text`：

- purpose：`summary|title_translation|document_translation`；
- source/target language tag使用有界BCP-47子集：2–8字母primary subtag，后续每段1–8位ASCII字母或数字，总长不超过35；
- 可选 purpose-specific async summary/translation job ID、model ID、可选 model SHA-256、prompt ID/version/SHA-256；
  async writer 必须同时写入exact purpose job ID与model SHA-256，且不得把summary job绑定到translation purpose或反向绑定；
- 摘要必须声明并满足 character/word limit；标题译文必须声明并满足 character limit；
- 全文译文不在 Analysis 层截断，但仍受 request/body 总上限约束。

`generated_text` 是 Analysis identity 的一部分。MemeX 只验证 typed shape、limit、prompt digest
格式和 source revision binding；普通 add/apply seam不调用模型，也不证明内容事实正确。仓库内
冻结的 faithful summary/translation prompt 可由外部 adapter读取并把 exact digest写入
descriptor。`memex.async_translation` 是独立 operational queue/worker seam，最终仍只能通过本接口
的 immutable record authority提交校验后的生成文本。

MemeX 在 prepare 和 durable commit 前验证 current ItemRevision，并冻结 revision ID、
typed revision SHA-256 与 evidence IDs。effective sensitivity 只能提高，不能低于输入。

## Output and replay

stdout 是 privacy-safe `AnalysisReceipt`，只含 ID、`created|replayed`、sensitivity、
input Item IDs 与 projection warning codes；不回显正文。

canonical JSON 位于：

```text
<library>/analysis/records/<analysis_id>.json
```

相同 request 与相同 verified inputs 产生相同 ID 并 exact replay。不同正文、revision
或 provenance 产生新的 immutable record。

## Human and Agent readback

JSON list/show 返回完整 typed record；Markdown list 是摘要索引，Markdown show 包含
analysis ID、input revision/digest、topics、provenance 与正文，便于人工引用和复核。

## Guardrails

- 不运行模型，不自动判断事实，不生成隐式 link；外部模型输出必须先验证再显式提交；
- caller-controlled root、ancestor、request、lock、record 与 projection symlink
  fail closed；
- canonical JSON 成功后 projection 失败只返回明确 warning，不把 durable commit
  误报为失败；
- 不写 Item、Evidence、Registry 或旧 IA/data 项目。

## Missing proof

未定义 analysis edit/supersedes、历史 ItemRevision 引用、真实 intelligence 迁移或生产
Web 授权。
