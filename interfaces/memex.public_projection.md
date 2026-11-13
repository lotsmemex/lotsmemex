# Public Projection V0

`memex publication release-public --library <library> --request <public-release-request.json>` 创建本地 immutable release；`show-public --release <release_id> --format json|markdown` 从同一 typed projection readback。

release 前对 Publication、direct/transitive refs、敏感级别、冻结 digest 和 allowlist 做完整 gate。完全相同的已存 request 走 historical exact replay：只验证 immutable publication/projection parity，不因后续 current ref 漂移破坏历史 release，并修复 derived bundle。warning 表示 release JSON 已 durable、对应 manifest/JSON/Markdown projection 待修复。

只允许固定公开字段，不生成 latest、不部署。对外站点仍是后续外部 adapter，不在本能力范围内，也不表示已上线。
