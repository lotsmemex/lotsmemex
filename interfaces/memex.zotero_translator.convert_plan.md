# memex.zotero_translator.convert_plan

## Purpose

Plan and scaffold conversion from Zotero translator knowledge into MemeX Markdown translator scripts and Rust-readable manifests.

## Inputs

```yaml
zotero_translators_path: optional local path to Zotero translators repo or export
site_family: optional
output_translator_root: translators
```

## Outputs

- Human/AI-reviewable translator documentation under `translators/<id>/translator.md`
- Machine-readable manifest under `translators/<id>/translator.yaml`
- Rust adapter/test task if implementation is requested

## Current MVP

- `generic.webpage` v2 supports bounded verified HTML Readability/sanitization/Markdown plus the
  V1 plaintext fallback. Markdown documentation is not executable runtime code.
- `weixin.article` v1 is the first executable site translator. It accepts only verified
  `https://mp.weixin.qq.com/s/...` HTML, extracts `#js_content`, restores lazy image references and
  fails closed on challenge pages; fetching/MCP remains outside translator authority.
- Full Zotero library conversion awaits a local Zotero translators checkout/export path.

## Side effects

Repository-relative translator documentation/manifest writes only; no browser, external API,
credential access or runtime translator activation.
