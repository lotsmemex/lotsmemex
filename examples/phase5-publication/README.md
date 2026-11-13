# Phase 5 合成 Publication 示例

本目录只有合成模板，没有真实正文、凭据或绝对路径。先用 Phase 1 fixture ingest，再用 Phase 4 analysis request 创建 Public Analysis。把生成的 Analysis ID 填入 `publication-request.json` 副本；创建后把 receipt 的 Publication ID 填入 `public-release-request.json` 副本。

```bash
memex publication create --library <temporary-library> --request <publication-request.json>
memex publication show --library <temporary-library> --publication <publication-id> --format markdown
memex publication release-public --library <temporary-library> --request <public-release-request.json>
memex publication show-public --library <temporary-library> --release <release-id> --format json
```

这些命令只写本地 immutable record/release 与 derived bundle；不发布 Web、不更新 latest，也不表示公开站或 dashboard 已上线。
