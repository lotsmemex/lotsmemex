# Phase 4 合成 Analysis / Link / Recall 示例

本目录只包含合成 request 模板，不包含真实情报、凭据或绝对用户路径。模板中的
`item_replace_after_ingest`、`revision_replace_after_ingest` 与全零 analysis ID 必须替换
为同一临时 MemeX library 中真实生成的 synthetic IDs。

先使用 Phase 1 fixture 建立合成 Item：

```bash
cargo run -p memex-cli -- ingest \
  --source examples/phase1-evidence/source.json \
  --capture examples/phase1-evidence/capture.json \
  --library /tmp/memex-phase4
```

将 receipt 中的 Item ID 写入 `analysis-request.json` 的副本，然后运行：

```bash
cargo run -p memex-cli -- analysis add \
  --library /tmp/memex-phase4 \
  --request /tmp/analysis-request.json

cargo run -p memex-cli -- analysis list \
  --library /tmp/memex-phase4 \
  --format markdown

cargo run -p memex-cli -- recall \
  --library /tmp/memex-phase4 \
  --query synthetic \
  --max-sensitivity public \
  --limit 20 \
  --format json
```

再把 Analysis ID、Item ID 与 current Revision ID 写入 `link-request.json` 的副本：

```bash
cargo run -p memex-cli -- analysis link \
  --library /tmp/memex-phase4 \
  --request /tmp/link-request.json
```

这些命令不调用模型、不读取旧 IA 正文，也不发布到 Web。
