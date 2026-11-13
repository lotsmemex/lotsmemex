# 合成 Phase 2 fixture

仅使用虚构资料。可复制命令：

    cargo run -p memex-cli -- shadow-import-legacy --package-root examples/phase2-legacy-shadow --meta run.meta.json --items source-a.jsonl --items source-b.jsonl --library /tmp/memex-shadow

adapter 不扫描目录，也不读取 meta 内的 file 路径。
