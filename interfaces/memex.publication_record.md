# Publication Record V0

`memex publication create --library <library> --request <publication-request.json>` 创建本地不可变 `PublicationRecord`；`list` 与 `show` 支持 `--format json|markdown`。

Publication crate 是 authored derivative 的 typed owner。canonical JSON 是 durable commit point；Markdown 与 index 是同源 readback。stdout 为仅含 ID、状态、kind、sensitivity、数量和 warning 的 privacy-safe receipt。

创建前会验证冻结输入与 current verified refs。相同 request/inputs 为 exact replay；正文、输入 digest、分类或 provenance 变化会形成新 record。projection warning 表示 durable JSON 已成功、对应 derived 文件待修复。

不调用模型、不写旧 IA/data/Web、不部署。对外站点是后续外部 adapter，不表示已上线。
