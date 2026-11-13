# 合成 Source Registry 示例

本目录仅包含合成数据。

    cargo run -p memex-cli -- source import-legacy --package-root examples/phase3-source-registry --profile source.json --library /tmp/memex-phase3-registry
    cargo run -p memex-cli -- source list --library /tmp/memex-phase3-registry --format json
    cargo run -p memex-cli -- source show --library /tmp/memex-phase3-registry --source <source_id> --format markdown
