# Offline PDF conversion

`memex file convert INPUT.pdf --out NEW_DIRECTORY` is a local, isolated CLI
capability. It uses Microsoft's [MarkItDown](https://github.com/microsoft/markitdown)
(MIT), pinned to 0.1.9, through its explicit PDF converter. It does not initialize
MarkItDown's general dispatcher, Magika classifier, plugins, OCR or LLM clients.
macOS `sandbox-exec` denies networking; other platforms currently fail closed.
There is no resident process, model download or automatic dependency installation.

Install the separate runtime once (or supply `--python` pointing at another pinned
environment):

```sh
uv venv ~/.local/share/memex/file-converter --python 3.10
uv pip install --python ~/.local/share/memex/file-converter/bin/python 'markitdown[pdf]==0.1.9'
```

Output contains the unchanged `original.pdf`, unmodified engine output `raw.md`,
the reading projection `document.md` and checksummed `conversion.json`. Whitespace
reflow preserves paragraph boundaries and Chinese punctuation. Repeated page-edge
headers and numbered footers are omitted only from the reading projection and
counted in the receipt; raw extraction and PDF remain intact. No file paths are
inserted into the reading text. Existing output is never overwritten.

Adapter version 2 honors tagged PDF StructTree `ActualText` through the shared
pdfplumber extraction helper before whitespace reflow. For these PDFs `raw.md`
is the replacement-aware extraction, not the earlier glyph-only MarkItDown text;
the receipt records replacement count and adapter version. Complete document scope
is checked before page extraction. Cross-page or unresolved-page replacements and
marked-content nesting inside a replacement scope are rejected explicitly until
supported; they must not silently duplicate text or leak replaced glyphs. Legacy
adapter 1 bundles remain verifiable, but a valid hash is not an extraction-quality
guarantee. Neither adapter guesses missing words or sends text to a model.

This slice supports text-layer PDFs, not arbitrary scanned/layout-heavy documents.
Image-only pages, unmapped glyphs and empty output fail instead of silently
producing an empty document. Mixed image/text pages and complex tables still need
human quality review. Limits: 100 MiB input, 1,000 pages, 16 MiB text, 120 seconds.
The receipt is conversion evidence, not proof of reading quality.

`memex file apply-conversion --bundle DIR --library LIBRARY --item ID
--expected-revision-id REVISION --max-sensitivity private` explicitly revises an
existing item. It verifies bundle hashes and original PDF identity, preserves
item ID / added_at / source / original Evidence and old revisions, adds Markdown
Evidence, and changes the item type to `document`. It uses the Store CAS and
owner-edit journal. It is not an import, automatic watcher or duplicate creator.

**Deployment order:** first install compatible Store readers, then take a
consistent Library backup, then apply. An old daemon cannot validate the new
activity even when its query cache sees the generation change. The CLI does not
restart services or take a whole-library backup automatically.

Synthetic verification:

```sh
TMPDIR=/private/tmp cargo test -p memex-store reconversion
TMPDIR=/private/tmp cargo build -p memex-cli
MEMEX_BIN=target/debug/memex \
MEMEX_PDF_PYTHON=$HOME/.local/share/memex/file-converter/bin/python \
python3 crates/memex-cli/tests/file_conversion_test.py
```
