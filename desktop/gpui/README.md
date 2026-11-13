# MemeX desktop (GPUI)

Development client for the approved v4/v4.1 design: reading view (reader above,
conversation below, composer at the bottom; item list over metadata on the right)
and the classic Zotero three-pane view, with 阅读 | 经典 in the toolbar (⌘1/⌘2),
"展开为经典视图" / "返回阅读视图", follow-system / light / dark appearance.

`desktop/macos` (AppKit) stays as the comparison and fallback client.

## Requirements

- macOS 15 or later (GPUI Kit 0.8.1; the AppKit client targeted macOS 13).
- Rust 1.99.1, pinned by `rust-toolchain.toml` (gpui-pre 0.3.10 does not build on 1.96).
- Xcode (Metal shader compiler): `export DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer`.

This is an independent Cargo workspace; it does not link backend crates. The
only business seam is the agent's owner-only Unix socket.

## Modules

| Crate | Owns | Checks |
|---|---|---|
| `memex-client` | UDS framing, DTOs, errors, synthetic `FixtureBackend` | transport, old agent → unsupported, stale revision, context slices, query views |
| `memex-library` | facets, search, sort, windowed rows, selection; folder tree and group edits (`edit.rs`: single-op `group_edit`, request id reuse) | window fetches, stale views, empty/failed states, group_edit wire shape and retries |
| `memex-metadata` | title/tag drafts, save, revision conflict, navigation guard | late results, conflict, guard |
| `memex-reader` | tabs, blocks with UTF-8 offsets, citation jumps; 原文/对照/译文 (`bilingual.rs`), 翻译此文 request and polling (`translate.rs`) | CJK/emoji offsets, translation without mapping, block pairing and mismatch fallback, request state machine |
| `memex-chat` | composer, suggestions toggle, transcript | sample mode never invents replies |
| `memex-shell` | window, layouts, docking, appearance, wiring | prefs |

Modules talk through public events; the shell owns navigation and layout prefs.

## Run

```sh
cargo test --workspace
cargo run -p memex-shell                      # synthetic fixture (default)
MEMEX_AGENT_SOCKET=/path/to/isolated.sock cargo run -p memex-shell
```

- Data location (first match wins):
  1. `MEMEX_DESKTOP_STATE=<file>`: development prefs file; missing means defaults.
  2. Locator `~/Library/Application Support/LotsMemeXDesktopDev/data-location.json`:
     `{"schema_version":1,"settings_path":"/abs/.../Desktop/settings.json"}`
     (no other fields). The settings file it points to must exist with
     `"schema_version":1` and an absolute `"agent_socket"`; the app keeps both and
     any unknown fields when it saves layout (atomic, `0600`).
  3. Neither: development defaults in `LotsMemeXDesktopDev/layout.json`.
- Agent socket: `MEMEX_AGENT_SOCKET`, else the settings file's `agent_socket`, else,
  only when nothing is configured, clearly labelled synthetic data
  (`MEMEX_FIXTURE_ITEMS`, default 128). A broken locator or settings file shows a
  configuration error and connects to nothing; an unreachable configured socket shows
  offline. Neither falls back to synthetic data or to the installed agent.
- Chat is shown as unavailable with a real agent (no conversation interface yet). Old
  agents without `library_query`/`context_read` fall back to paged `list` or report
  unsupported. Responses are capped at 256 KiB, so an oversized `item_read` shows
  "代理返回的数据过大" rather than a partial body.
- Images in Markdown are shown as "待后端供图" placeholders; the reader never loads
  paths or URLs from the text.
- `MEMEX_DESKTOP_DISPLAY=<CGDirectDisplayID>` opens the window on that display
  without taking focus.

## Scripted review states (development only)

```sh
MEMEX_DESKTOP_SNAPSHOT_DIR=/tmp/shots cargo run -p memex-shell --features dev-snapshot
```

`MEMEX_DESKTOP_WALK=bilingual` instead walks 原文/对照/译文, 翻译此文 (queued,
running, done, failed, Chinese source, no model) and group editing.
`MEMEX_DESKTOP_WALK=translationprogress` shows a newer agent's translation
diagnostics (running 1/2 段 · 已用 3 分钟, blocks kept in the original, a failed
part), light and dark. Set `MEMEX_DESKTOP_STATE` to a scratch file so the walk
uses the fixture and never the configured agent socket or settings.

The default walk goes through reading/classic/return, with and without a
conversation, light/dark, citation from translation, revision conflict,
navigation guard, offline and a narrow window; saves GPUI's Metal render of each
state plus `states.tsv`, then quits. It drives the shell's own intents, not OS
input: keyboard, Chinese IME and VoiceOver must be checked by a person.
