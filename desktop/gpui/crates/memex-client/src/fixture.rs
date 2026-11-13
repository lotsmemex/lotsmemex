//! Synthetic in-memory backend for development and module verification.
//!
//! Everything here is invented sample text. It implements the same command
//! semantics the agent documents (50-item cursor pages, revision-bound metadata
//! updates with `stale_item_revision`, bounded UTF-8 `context_read`) so UI
//! modules can be exercised without a real library. The UI must label this mode.

use crate::biblio::*;
use crate::dto::*;
use crate::fixture_biblio as fb;
pub use crate::fixture_book::BOOK_ITEM;
pub use crate::fixture_ai_book::{AI_BOOK_ITEM, AI_CHUNK};
pub use crate::fixture_import::{FixtureImport, ImportControls};
use crate::{interpret, Backend, BackendMode, ClientError, Command, Response};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

const PAGE: usize = 50;

/// Switches for exercising error and edge states.
#[derive(Debug, Clone, Default)]
pub struct FixtureControls {
    pub offline: bool,
    pub legacy_agent: bool,
    pub latency: Duration,
    /// Answer the next lookups with this reason code (e.g. `lookup_rate_limited`).
    pub lookup_error: Option<String>,
    /// Advertise `translation_provider_v1.available: false` (no model configured).
    pub no_translation_provider: bool,
    /// Do not advertise `item_chat_v1` (an agent without the chat command).
    pub no_item_chat: bool,
    /// Answer `library_item_read` without `ai_minutes` (an agent older than minutes).
    pub omit_ai_minutes: bool,
    /// Answer `item_chat` at once instead of after `CHAT_DELAY` (tests).
    pub instant_chat: bool,
    /// An agent without `ai_insights_v1` (no feature, no `ai_insights` field).
    pub no_insights: bool,
    /// `insight_request` answers `no_insight_provider`.
    pub no_insight_provider: bool,
    /// An agent without `agent_version`.
    pub no_agent_version: bool,
    /// `agent_version` answers `binary_sha256: null` (still hashing).
    pub agent_hashing: bool,
    /// `library_query` rejects `sort: "added_at"` (a daemon before that candidate).
    pub no_added_at_sort: bool,
    /// A daemon before receipt-backed origin filtering.
    pub no_origin_filter: bool,
    /// Highlights as an agent before v2: no `original`/`translation` spans, no `sides`.
    pub highlights_v1: bool,
    /// An agent without `dedup_trash_v1` (no trash list or restore).
    pub no_trash: bool,
    /// Opt-in interaction fixture; defaults and the global dedup trash stay unchanged.
    pub group_trash: bool,
    /// An agent without `minutes_request_v1`: `translation_request` makes the minutes.
    pub no_minutes_request: bool,
    /// `minutes_request` answers `no_minutes_provider`.
    pub no_minutes_provider: bool,
    /// Requested minutes finish after 60 ms instead of `MINUTES_DONE` (tests).
    pub fast_minutes: bool,
    /// An agent without `item_chat_history_v1` (conversations are not kept).
    pub no_chat_history: bool,
    /// `conversation_read` takes `SLOW_HISTORY_DELAY` (to show the loading state).
    pub slow_history: bool,
    /// An agent with `reader_v1` but without paragraph alignment or chapter repair.
    pub no_book_alignment: bool,
    /// A daemon without `groups_v1.query_unfiled` (no Inbox query).
    pub no_unfiled_query: bool,
    /// A daemon without `groups_v1.node_delete` (no recoverable folder deletion).
    pub no_node_delete: bool,
    /// Every outstanding deletion preview token has expired (as after a restart).
    pub expire_delete_tokens: bool,
    /// A daemon without `reader_annotations_v1` (five-style marks).
    pub no_reader_annotations: bool,
    /// The next `annotations_edit` (either space) is applied but its reply is lost (a timeout).
    pub lose_next_annotation_reply: bool,
    /// The next `annotations_edit` is committed, then answers `internal_error`
    /// (the directory sync after the rename failed).
    pub fail_after_next_annotation_commit: bool,
    /// Advertise `reader_chapter_translation_v1` with metadata discovery: the AI
    /// book's chapters then have completed AI translations.
    pub chapter_translations: bool,
    /// A daemon without `reader_annotations_ai_translation_v1` (AI-translation marks).
    pub no_ai_annotations: bool,
    /// A daemon without `reader_passes_v1` (reading passes and timing).
    pub no_reader_passes: bool,
    /// The next `passes_edit` is applied but its reply is lost (a timeout).
    pub lose_next_pass_reply: bool,
    /// The next `passes_edit` is committed, then answers `internal_error`.
    pub fail_after_next_pass_commit: bool,
    /// Just before the next `passes_edit`, another device edits the item's passes.
    pub stale_next_pass_edit: bool,
    /// Before the next timing report every live session ends (a daemon restart).
    pub expire_pass_sessions: bool,
    /// The next `add_members` / `remove_members` answers this code without
    /// applying anything (once each).
    pub fail_next_add_members: Option<String>,
    pub fail_next_remove_members: Option<String>,
    /// Advertise and apply `move_members` (`groups_v1.write_ops`), the atomic
    /// same-root folder move of the 2026-11-08 contract.
    pub move_members_op: bool,
    /// Opt-in folder-transfer contract; older fixtures keep refusing it.
    pub transfer_folders_op: bool,
    pub lose_next_folder_reply: bool,
    /// The transfer commits, then the daemon reports an uncertain internal error.
    pub folder_commit_error: bool,
    /// A daemon without `item_cover_v1`: no cover request is ever answered.
    pub no_item_cover: bool,
    /// `item_cover_batch` answers null for every item (no visible cover).
    pub cover_null: bool,
    /// The next cover read sees its cover republished after the first chunk
    /// (`stale_cover_version`, then a new version with new bytes).
    pub cover_stale_once: bool,
    /// Cover chunks carry a flipped byte (their hash no longer matches).
    pub cover_corrupt: bool,
    /// A daemon without the active-UI-destination feature (`ui_active_group_set`).
    pub no_active_group: bool,
    /// The next this many `ui_active_group_set` calls are applied, but their
    /// replies are lost (a timeout: outcome unknown).
    pub lose_active_group_replies: usize,
    /// A daemon without `capture_jobs_recent_v1`.
    pub no_capture_jobs: bool,
}

/// Synthetic AI minutes of one item; a request moves them like a translation.
#[derive(Debug, Clone)]
struct Minutes {
    state: MinutesState,
    reason: Option<String>,
    text: Option<String>,
    started: Option<Instant>,
    pending: String,
    /// The next run ends `failed` (provider_timeout); a retry succeeds.
    fail_once: bool,
}

/// Item whose first requested 纪要 fails; a retry succeeds.
pub const MINUTES_FAIL_ITEM: &str = "item_fixture_000010";

/// Item whose chat answers `no_chat_provider`, and one whose first answer per
/// request is `provider_outcome_unknown` (a retry with the same id succeeds).
pub const CHAT_NO_PROVIDER_ITEM: &str = "item_fixture_000006";
pub const CHAT_UNKNOWN_ITEM: &str = "item_fixture_000007";
/// Item with a stored (synthetic) conversation of two exchanges, and one whose
/// `conversation_read` fails with `internal_error`.
pub const CHAT_HISTORY_ITEM: &str = "item_fixture_000003";
pub const CHAT_HISTORY_FAIL_ITEM: &str = "item_fixture_000004";
/// Time `conversation_read` takes (normal / `slow_history`).
pub const HISTORY_DELAY: Duration = Duration::from_millis(500);
pub const SLOW_HISTORY_DELAY: Duration = Duration::from_secs(6);
/// Turns per synthetic `conversation_read` page.
const HISTORY_PAGE_TURNS: usize = 3;
/// The long English showcase: translated, AI minutes, model keywords, URL.
pub const SHOWCASE_ITEM: &str = "item_fixture_000026";
/// Markdown rendering sample: links (a YouTube one, a `javascript:` one), nested
/// lists, a quote, a table, a rule, code, an image; its minutes are Markdown too.
pub const MARKDOWN_ITEM: &str = "item_fixture_000027";
const MARKDOWN_INDEX: usize = 27;
/// 古籍 sample (简繁 / 竖排 / 寒蝉拙楷): the opening chapters of 道德经 (public
/// domain) in simplified characters, with headings, bold, `<font color=red>`,
/// 「」《》（） and a synthetic note. Chinese, so nothing to translate.
pub const GUJI_ITEM: &str = "item_fixture_000029";
const GUJI_INDEX: usize = 29;
pub const GUJI_TITLE: &str = "道德经（合成古籍样本）";
/// A long synthetic transcript (speakers, timestamped sections, AI highlights,
/// minutes and a stored conversation) for the long-read view.
pub const LONGREAD_ITEM: &str = "item_fixture_000030";
const LONGREAD_INDEX: usize = 30;
/// An English podcast transcript with a finished translation that renders the
/// speakers' names in Chinese (「Thorsten」 → 「托尔斯滕」), some labels in bold.
pub const PODCAST_ITEM: &str = "item_fixture_000032";
const PODCAST_INDEX: usize = 32;
pub const PODCAST_TITLE: &str = "Don't Wait for Your Agent (synthetic podcast)";
const PODCAST_TITLE_ZH: &str = "不要等你的 Agent（合成播客）";
const PODCAST_EN: &str = "# Don't Wait for Your Agent (synthetic podcast)\n\n## 00:00 A weekly token cap\n\n**Quinn:** We set a weekly token cap from the first day, because without one nobody thinks about cost and the bill quietly grows.\n\n**Thorsten:** Best practice from three months ago may already be wrong. You should not sit there waiting for the agent; it should work while you do something else.\n\nThat is not a tool problem, it is a way-of-working problem. You have to rearrange the whole day around it, and that takes a few weeks to feel normal.\n\nQuinn: We look at usage once a week, to see who is wasting tokens and who is really shipping.\n\n## 12:40 Why cheap models cost more\n\n**Thorsten:** A cheap model has a low unit price, but it takes longer routes, so the final bill is higher. Retrieval has to be counted the same way.\n\nSometimes there is simply something bad that should be fixed, and today it is faster to fix than ever before. Nobody needs to defend it as a style.\n\nIf you count the hours you spend re-reading its output, the cheap run is rarely cheap. The total is what matters, not the price per token.\n\n**Quinn:** So we stopped comparing price lists and started comparing finished tasks per week.\n\nThorsten: And the finished tasks went up once people stopped waiting.\n\n(All text is synthetic, for interface checks only.)\n";
const PODCAST_ZH: &str = "# 不要等你的 Agent（合成播客）\n\n## 00:00 每周的 token 上限\n\n**奎因：** 我们从第一天起就定了每周的 token 上限，因为没有上限的时候，没人会去想成本，账单会悄悄涨上去。\n\n**托尔斯滕：** 三个月前的最佳实践，现在可能已经不对了。你不应该坐在那里等 Agent，它应该在你做别的事的时候干活。\n\n这不是工具的问题，是工作方式的问题。你得围绕它重新安排一整天，而这要几周才会觉得自然。\n\n奎因：我们每周看一次用量，看谁在浪费 token，谁在真正交付。\n\n## 12:40 便宜模型为什么更贵\n\n**托尔斯滕：** 便宜的模型单价低，但它要绕更多的路，所以最后的账单更高。检索也要这样算总账。\n\n有时候就是有个糟糕的东西，应该改掉，而且现在改起来比以往都快。没人需要把它说成一种风格。\n\n如果把你反复重读它输出的时间也算上，便宜的那次运行很少真的便宜。要紧的是总账，不是每个 token 的价格。\n\n**奎因：** 所以我们不再比较价目表，而是比较每周完成的任务。\n\n托尔斯滕：大家不再干等之后，完成的任务就多了。\n\n（全部为合成文字，仅供界面检查。）\n";
pub const LONGREAD_TITLE: &str = "长文阅读合成样本：不要等你的 Agent";
const LONGREAD_MINUTES: &str = "（合成纪要）两位主持人讨论 token 上限、模型成本、Orbs 的命名，以及不要等待 Agent 的工作方式。";
const LONGREAD_MARKDOWN: &str = "# 长文阅读合成样本：不要等你的 Agent\n\n## 00:00 开场：每周 €50 的 token 上限\n\nQuinn：我们一开始就定了上限，因为没有上限的时候，大家根本不会去想成本，大规模语言模型的账单会悄悄涨上去。我们想避免一种现象：大家总想在反馈里找到好的一面，连标点挤压这种小问题都被说成风格。\n\n有时候就是有个糟糕的东西，应该改掉，而且现在改起来比以往都快。有时候就是有个糟糕的东西，应该改掉，而且现在改起来比以往都快。\n\nThorsten：三个月前的最佳实践，现在可能已经完全不对了。你不应该坐在那里等 Agent，它应该在你做别的事的时候干活。\n\n我们想避免一种现象：大家总想在反馈里找到好的一面，连标点挤压这种小问题都被说成风格。这不是工具的问题，是工作方式的问题，你得重新安排一天。\n\nQuinn：我们内部每周都会看一次用量，看谁在浪费，谁在真正做事。我们内部每周都会看一次用量，看谁在浪费，谁在真正做事。\n\n我记得在 Orbs 还没成形的时候，我们就把它叫作“orb bad”，这在当时是个研究热点。三个月前的最佳实践，现在可能已经完全不对了。\n\nThorsten：这不是工具的问题，是工作方式的问题，你得重新安排一天。我们一开始就定了上限，因为没有上限的时候，大家根本不会去想成本，大规模语言模型的账单会悄悄涨上去。\n\n## 12:40 便宜模型为什么最后更贵\n\nThorsten：我记得在 Orbs 还没成形的时候，我们就把它叫作“orb bad”，这在当时是个研究热点。便宜的模型单价低，但它要绕更多的路，最后的账单反而更高，检索增强生成也一样要算总账。\n\n这不是工具的问题，是工作方式的问题，你得重新安排一天。我记得在 Orbs 还没成形的时候，我们就把它叫作“orb bad”，这在当时是个研究热点。\n\nQuinn：便宜的模型单价低，但它要绕更多的路，最后的账单反而更高，检索增强生成也一样要算总账。我们想避免一种现象：大家总想在反馈里找到好的一面，连标点挤压这种小问题都被说成风格。\n\n你不应该坐在那里等 Agent，它应该在你做别的事的时候干活。有时候就是有个糟糕的东西，应该改掉，而且现在改起来比以往都快。\n\nThorsten：我们一开始就定了上限，因为没有上限的时候，大家根本不会去想成本，大规模语言模型的账单会悄悄涨上去。你不应该坐在那里等 Agent，它应该在你做别的事的时候干活。\n\n有时候就是有个糟糕的东西，应该改掉，而且现在改起来比以往都快。这不是工具的问题，是工作方式的问题，你得重新安排一天。\n\nQuinn：三个月前的最佳实践，现在可能已经完全不对了。我们内部每周都会看一次用量，看谁在浪费，谁在真正做事。\n\n## 31:05 Orbs 与“orb bad”\n\nQuinn：有时候就是有个糟糕的东西，应该改掉，而且现在改起来比以往都快。三个月前的最佳实践，现在可能已经完全不对了。\n\n三个月前的最佳实践，现在可能已经完全不对了。我们一开始就定了上限，因为没有上限的时候，大家根本不会去想成本，大规模语言模型的账单会悄悄涨上去。\n\nThorsten：我们想避免一种现象：大家总想在反馈里找到好的一面，连标点挤压这种小问题都被说成风格。便宜的模型单价低，但它要绕更多的路，最后的账单反而更高，检索增强生成也一样要算总账。\n\n我们内部每周都会看一次用量，看谁在浪费，谁在真正做事。我记得在 Orbs 还没成形的时候，我们就把它叫作“orb bad”，这在当时是个研究热点。\n\nQuinn：我记得在 Orbs 还没成形的时候，我们就把它叫作“orb bad”，这在当时是个研究热点。我们想避免一种现象：大家总想在反馈里找到好的一面，连标点挤压这种小问题都被说成风格。\n\n这不是工具的问题，是工作方式的问题，你得重新安排一天。有时候就是有个糟糕的东西，应该改掉，而且现在改起来比以往都快。\n\nThorsten：便宜的模型单价低，但它要绕更多的路，最后的账单反而更高，检索增强生成也一样要算总账。你不应该坐在那里等 Agent，它应该在你做别的事的时候干活。\n\n## 48:20 不要等你的 Agent\n\nThorsten：这不是工具的问题，是工作方式的问题，你得重新安排一天。这不是工具的问题，是工作方式的问题，你得重新安排一天。\n\n便宜的模型单价低，但它要绕更多的路，最后的账单反而更高，检索增强生成也一样要算总账。我们内部每周都会看一次用量，看谁在浪费，谁在真正做事。\n\nQuinn：你不应该坐在那里等 Agent，它应该在你做别的事的时候干活。三个月前的最佳实践，现在可能已经完全不对了。\n\n我们一开始就定了上限，因为没有上限的时候，大家根本不会去想成本，大规模语言模型的账单会悄悄涨上去。我们一开始就定了上限，因为没有上限的时候，大家根本不会去想成本，大规模语言模型的账单会悄悄涨上去。\n\nThorsten：有时候就是有个糟糕的东西，应该改掉，而且现在改起来比以往都快。便宜的模型单价低，但它要绕更多的路，最后的账单反而更高，检索增强生成也一样要算总账。\n\n三个月前的最佳实践，现在可能已经完全不对了。我记得在 Orbs 还没成形的时候，我们就把它叫作“orb bad”，这在当时是个研究热点。\n\nQuinn：我们想避免一种现象：大家总想在反馈里找到好的一面，连标点挤压这种小问题都被说成风格。我们想避免一种现象：大家总想在反馈里找到好的一面，连标点挤压这种小问题都被说成风格。\n\n（全部为合成文字，仅供界面演示。）\n";
const GUJI_MARKDOWN: &str = "# 道德经（合成古籍样本）\n\n## 第 01 章\n\n道可道，非常道；名可名，非常名。**无名天地之始，有名万物之母。**故常无欲，以观其妙；常有欲，以观其徼。此两者同出而异名，同谓之玄。<font color=red>玄之又玄，众妙之门。</font>\n\n#### 注\n\n① 合成样本注释（以系统楷体显示，供对照）：此章总言道体：学者当于无名处着眼，于有名处致用，前后一贯，不可执着于言语。\n\n## 第 02 章\n\n天下皆知美之为美，斯恶已；皆知善之为善，斯不善已。故有无相生，难易相成，长短相形，高下相倾，音声相和，前后相随。是以圣人处无为之事，行不言之教；万物作焉而不辞，生而不有，为而不恃，功成而弗居。<font color=red>夫唯弗居，是以不去。</font>\n\n## 第 03 章\n\n不尚贤，使民不争；不贵难得之货，使民不为盗；不见可欲，使民心不乱。是以圣人之治，虚其心，实其腹，弱其志，强其骨。**常使民无知无欲，使夫智者不敢为也。**为无为，则无不治。\n\n《老子》又曰：「知者不言，言者不知。」（见第 56 章）\n";
const MARKDOWN_TITLE: &str = "Markdown Rendering Check (synthetic)";
const MARKDOWN_TITLE_ZH: &str = "Markdown 渲染检查（合成）";
const MARKDOWN_EN: &str = "# Markdown Rendering Check (synthetic)\n\nThis synthetic note exercises **bold**, *emphasis shown upright*, `inline code` and a link to the [project page](https://example.invalid/project). A literal `[x](y)` inside code stays as typed.\n\n## Links\n\n[YouTube: https://www.youtube.com/watch?v=synthetic01](https://www.youtube.com/watch?v=synthetic01)\n\nA refused link: [click here](javascript:alert(1)) stays plain text.\n\n## Lists\n\n- First level item\n  - Second level with **bold**\n    1. Third level, ordered\n    2. Another ordered item\n- Back to the first level\n\n> A quoted line from the synthetic source.\n> > A nested quote.\n\n---\n\n| Model | Context | Note |\n|---|---|---|\n| Alpha | 8k | [docs](https://example.invalid/alpha) |\n| Beta | 32k | `synthetic` |\n\n```rust\nfn main() {\n    println!(\"[not a link](https://example.invalid)\");\n}\n```\n\n![Figure 2: synthetic chart](https://example.invalid/chart.png)\n\nThe last paragraph closes the sample.\n";
const MARKDOWN_ZH: &str = "# Markdown 渲染检查（合成）\n\n这篇合成笔记用来检验 **加粗**、*正体显示的强调*、`行内代码`，以及指向[项目页面](https://example.invalid/project)的链接。代码里的 `[x](y)` 保持原样。\n\n## 链接\n\n[YouTube：https://www.youtube.com/watch?v=synthetic01](https://www.youtube.com/watch?v=synthetic01)\n\n被拒绝的链接：[点这里](javascript:alert(1)) 只显示为文字。\n\n## 列表\n\n- 第一层\n  - 第二层，含 **加粗**\n    1. 第三层，有序\n    2. 另一个有序项\n- 回到第一层\n\n> 引自合成来源的一行。\n> > 嵌套引用。\n\n---\n\n| Model | Context | Note |\n|---|---|---|\n| Alpha | 8k | [docs](https://example.invalid/alpha) |\n| Beta | 32k | `synthetic` |\n\n```rust\nfn main() {\n    println!(\"[not a link](https://example.invalid)\");\n}\n```\n\n![Figure 2: synthetic chart](https://example.invalid/chart.png)\n\n最后一段结束这份样例。\n";
const MARKDOWN_MINUTES: &str = "（合成纪要，Markdown 示例）\n\n## 要点\n\n- **链接**：[项目页面](https://example.invalid/project)，以及 [YouTube 视频](https://www.youtube.com/watch?v=synthetic01)\n  - 嵌套：`inline code` 与 *正体强调*\n- 被拒绝的链接 [不会打开](javascript:alert(1))\n\n> 纪要中的引用。\n\n| 项 | 值 |\n|---|---|\n| 列表 | 已渲染 |\n\n```text\n[x](y) 在代码块里保持原样\n```";
/// Asked with "Markdown" in the question, the synthetic chat answer is Markdown-rich.
const MARKDOWN_CHAT_TAIL: &str = "\n\n## （合成）Markdown 示例\n\n1. 有序项，含 [项目页面](https://example.invalid/project)\n   - 嵌套无序项与 `inline code`\n2. 被拒绝的链接：[不会打开](javascript:alert(1))\n\n> 一段引用。\n\n| 列 | 值 |\n|---|---|\n| A | 1 |\n\n```rust\nlet link = \"[x](y)\"; // 代码里保持原样\n```";
/// English, translated, highlights made before the translation (`original_only`).
pub const ORIGINAL_ONLY_HIGHLIGHTS_ITEM: &str = "item_fixture_000005";
/// English, translated, highlights `unavailable / insight_translation_changed`.
pub const STALE_HIGHLIGHTS_ITEM: &str = "item_fixture_000009";
/// English, not translated, but its title is (`title_only`).
pub const TITLE_ONLY_ITEM: &str = "item_fixture_000002";
/// Time a synthetic chat reply takes.
/// Appended to the synthetic answer of a long question (layout checks only).
const LONG_CHAT_TAIL: &str = "\n\n**（合成）分步说明**\n\n1. 第一步：文章先指出单靠注意力难以同时兼顾长程依赖与计算成本——这是合成文本中的论断，没有真实实验支撑。\n2. 第二步：提出可写入的循环记忆槽，每读一段写入一次、回答前先读取；这里的“证据”只是三个合成数据集上的示意数字。\n3. 第三步：据此认为引用准确率提升且推理开销基本不变；最薄弱之处在于数据集过于简单，且没有与更长上下文窗口在相同计算预算下比较。\n\n**默认但未明说的前提**\n\n- 记忆槽容量固定时，被覆盖的信息不会在后文中再被需要。\n- 合成数据集上的结论能够迁移到真实长文档。\n\n以上内容全部为开发模式下的固定文本，用来检验长回复在对话区里的换行、滚动和与阅读区之间的分隔。";

pub const CHAT_DELAY: Duration = Duration::from_millis(2500);

/// Synthetic translation state of one item. A requested job is queued briefly,
/// then running, then succeeds with the synthetic translation (`pending`).
#[derive(Debug, Clone)]
struct Translation {
    state: AnalysisState,
    reason: Option<String>,
    title: Option<String>,
    markdown: Option<String>,
    started: Option<Instant>,
    pending: Option<(String, String)>,
    /// Why a request is not queued (as the agent's language gate: `source_is_chinese`).
    skip: Option<&'static str>,
    /// Document batches: (finished before this run, total). A failed run keeps
    /// its finished batches and a new request resumes from them.
    batches: (usize, usize),
    /// The title step already succeeded (the title is reported before the document).
    title_done: bool,
    /// Batches finished before the current run started.
    resume: usize,
    /// Wall-clock start (Unix seconds) of a held run, reported as `started_at`.
    wall_start: Option<i64>,
    /// `failure_detail` a newer agent reports for this failed run.
    failure_detail: Option<String>,
    /// Blocks of a finished translation kept in the original.
    untranslated: Option<usize>,
}

/// A translation state set directly (review walks): newer-agent diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranslationDemo {
    /// Running at `done/total`, started `minutes` ago; it does not move on.
    Running { done: usize, total: usize, minutes: i64 },
    /// Finished, with `untranslated` blocks kept in the original.
    Partial { untranslated: usize },
    /// Failed after `done/total` with this `failure_detail`.
    Failed { done: usize, total: usize, detail: String },
}

/// Time a requested synthetic translation spends queued, then per document batch.
const TRANSLATION_QUEUED: Duration = Duration::from_millis(400);
const TRANSLATION_BATCH: Duration = Duration::from_millis(600);
/// Time requested synthetic minutes take.
const MINUTES_DONE: Duration = Duration::from_secs(5);
/// Batches of an ordinary synthetic translation (about 5 s in all).
const TRANSLATION_BATCHES: usize = 8;
/// English item whose last translation failed after 5 of 18 batches; its title
/// was already translated. A new request resumes from batch 5.
pub const PARTIAL_TRANSLATION_ITEM: &str = "item_fixture_000014";

#[derive(Debug, Clone)]
struct Entry {
    item: LibraryItem,
    creation_origin: ItemOrigin,
    markdown: String,
    tags: Vec<String>,
    revision_number: u32,
    summary: Option<String>,
    translation: Translation,
    biblio: Bibliographic,
    /// Direct folder memberships (many-to-many; the tree describes folders only).
    groups: Vec<String>,
    minutes: Minutes,
    insights: crate::fixture_insights::Insights,
}

/// A folder node of the synthetic tree.
#[derive(Debug, Clone)]
struct Folder {
    id: String,
    name: String,
    kind: String,
    parent: Option<String>,
    /// Node version; every write to the node (or a new child) advances it.
    version: u64,
}

struct State {
    entries: Vec<Entry>,
    /// Owner reader state, toc and five-style marks of ordinary items.
    reader: crate::fixture_reader::ReaderStore,
    /// `reader_passes_v1` of every item.
    passes: crate::fixture_passes::PassesStore,
    reading_annotations: HashMap<String, crate::reading::ReadingAnnotations>,
    markdown_requests: HashMap<String, (crate::reading::MarkdownEdit, ItemUpdate)>,
    controls: FixtureControls,
    generation: u64,
    models: crate::fixture_models::ModelState,
    biblio: fb::BiblioState,
    folders: Vec<Folder>,
    /// Advances with every applied edit (binds tree views).
    groups_version: u64,
    /// request_id → the request it carried (replay returns the same answer).
    group_requests: HashMap<String, (Option<String>, Vec<GroupEditOp>)>,
    folder_receipts: HashMap<String, crate::GroupEditReceipt>,
    next_group: u64,
    /// item_chat request_id → (item, messages) it carried.
    /// item_chat request_id → (item, question, stored reply once answered). As the
    /// agent: a replay returns the stored reply; another item or question is a conflict.
    chat_requests: HashMap<String, (String, String, Option<String>)>,
    /// Stored conversations (`item_chat_history_v1`), ascending `seq` per item.
    chat_history: HashMap<String, Vec<crate::chat::StoredTurn>>,
    /// Trashed items: (position to restore at, entry, trash record). They read as
    /// not found and are left out of every list until restored.
    trash: Vec<(usize, Entry, Trashed)>,
    group_trash: HashMap<(String, String), (crate::trash::TrashEntry, Vec<String>)>,
    group_trash_requests: HashMap<String, (String, String, String, Option<String>)>,
    book: crate::fixture_book::BookState,
    /// The AI-translated book's retranslations and every item's AI-mark ledger.
    ai_book: crate::fixture_ai_book::AiBookState,
    covers: crate::fixture_covers::CoverState,
    /// `groups_v1.node_delete`: outstanding deletions, preview tokens and receipts.
    deletions: Vec<FixtureDeletion>,
    /// token → (group, impact digest at preview).
    delete_tokens: HashMap<String, (String, String)>,
    /// request_id → (command, target, deletion id): exact replay returns the receipt.
    delete_requests: HashMap<String, (&'static str, String, String)>,
    next_deletion: u64,
    /// Every `ui_active_group_set` that reached the fixture: (request_id, group_id).
    active_group_calls: Vec<(String, Option<String>)>,
    /// request_id → the selection it carried (a replay returns the same answer).
    active_group_requests: HashMap<String, Option<String>>,
    /// The last accepted explicit selection.
    active_group: Option<String>,
    /// The synthetic URL capture queue (empty: idle).
    capture_jobs: Vec<crate::capture_jobs::CaptureJob>,
    capture_job_calls: usize,
}

/// One recoverable deletion: the folders it took out (with their slots) and the
/// memberships it removed. Restoring puts back exactly these.
#[derive(Debug, Clone)]
struct FixtureDeletion {
    id: String,
    group_id: String,
    name: String,
    parent: Option<String>,
    deleted_at: String,
    folders: Vec<(usize, Folder)>,
    memberships: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
struct Trashed {
    trashed_at: String,
    kept: String,
    plan_id: String,
}

/// Items with a synthetic duplicate in the trash: (kept item index, minute).
const TRASHED_COPIES: [(usize, usize); 3] = [(1, 12), (5, 40), (9, 3)];

#[derive(Clone)]
pub struct FixtureBackend {
    state: Arc<Mutex<State>>,
}

const SOURCES: [&str; 4] = ["示例期刊网", "arxiv.org", "知识周报", "其他"];
const TYPES: [&str; 4] = ["网页文章", "论文", "PDF", "书籍"];
const TITLES: [&str; 12] = [
    "大规模语言模型在长文档检索增强生成中的上下文压缩方法综述与实证评估",
    "城市更新中的公共空间设计：以三个街区改造为例",
    "Attention Is Not All You Need: Revisiting Recurrent Memory",
    "生成式人工智能在教育中的应用与挑战",
    "从社交媒体数据看城市情绪的时空演化 🌆",
    "A Survey on Retrieval-Augmented Generation",
    "Designing for Human-AI Collaboration",
    "知识管理工具的用户行为与长期使用研究",
    "中文学术写作中的常见问题与改进策略 ✍️",
    "Graph Neural Networks for Knowledge Retrieval",
    "面向未来的图书馆：数字人文与开放科学",
    "社区参与对城市可持续发展的影响研究",
];
const TAGS: [&str; 6] = ["检索增强", "综述", "待读", "城市更新", "公共空间", "方法论"];

fn paragraph(seed: usize, n: usize) -> String {
    format!(
        "这是第 {n} 段合成示例正文（样本 {seed}）。随着大规模语言模型在知识密集型任务中的广泛应用，\
如何在长文档场景下有效地进行检索增强生成成为研究热点。本段用于检验中文换行、标点挤压与 emoji 🌹 的显示，\
并包含英文术语 Retrieval-Augmented Generation (RAG) 以测试混排。"
    )
}

pub(crate) fn markdown_for(index: usize, title: &str) -> String {
    let sections = if index == 0 { 260 } else { 6 + index % 5 };
    let mut out = format!("# {title}\n\n");
    for s in 0..sections {
        if s % 4 == 0 {
            out.push_str(&format!("## {}、合成小节 {}\n\n", s / 4 + 1, s / 4 + 1));
        }
        out.push_str(&paragraph(index, s + 1));
        out.push_str("\n\n");
        // Clippings carry images; the reader shows placeholders, never loads them.
        if index == 0 && s == 1 {
            out.push_str("![合成配图：灰玫瑰书签 🌹](images/fixture-rose.png)\n\n配图说明（合成）：![](https://example.invalid/inline.png)行内图片同样只显示占位。\n\n");
        }
    }
    out
}

// ---------------------------------------------------------------- folder tree
// Two imported Zotero libraries and one local root group ("我的分组"). Imported
// folders are native, editable MemeX state (groups_v1 draft 3): read_only=false.
// `group_edit` creates and renames folders and adds or removes members; nothing moves.

/// (id, name, kind, parent_id)
const GROUPS: [(&str, &str, &str, Option<&str>); 15] = [
    ("grp_z_psych", "心理学研究组", "zotero_group", None),
    ("grp_c_cog", "认知", "collection", Some("grp_z_psych")),
    ("grp_c_bias", "认知偏差", "collection", Some("grp_c_cog")),
    ("grp_c_anchor", "锚定效应", "collection", Some("grp_c_bias")),
    ("grp_c_confirm", "确认偏误", "collection", Some("grp_c_bias")),
    ("grp_c_attention", "注意力", "collection", Some("grp_c_cog")),
    ("grp_c_sleep", "睡眠", "collection", Some("grp_z_psych")),
    ("grp_z_ai", "AI 研究", "zotero_group", None),
    ("grp_c_llm", "大语言模型", "collection", Some("grp_z_ai")),
    ("grp_c_rag", "RAG", "collection", Some("grp_c_llm")),
    ("grp_c_align", "对齐与安全", "collection", Some("grp_c_llm")),
    ("grp_c_multi", "多模态", "collection", Some("grp_z_ai")),
    ("grp_u_mine", "我的分组", "user", None),
    ("grp_c_toread", "待读", "collection", Some("grp_u_mine")),
    ("grp_c_week", "本周", "collection", Some("grp_c_toread")),
];

/// Deterministic memberships. Deliberate overlaps: an item can sit in a folder and
/// in its child (bias/anchor, llm/rag, toread/week), so subtree counts must be a
/// distinct union, never the sum of child counts.
fn memberships(i: usize) -> Vec<String> {
    let mut out = vec![];
    let mut add = |cond: bool, id: &str| {
        if cond {
            out.push(id.to_string());
        }
    };
    add(i % 32 == 0, "grp_z_psych");
    add(i % 20 == 7, "grp_c_cog");
    add(matches!(i % 16, 1 | 9), "grp_c_bias");
    add(matches!(i % 16, 1 | 13), "grp_c_anchor");
    add(i % 16 == 7, "grp_c_confirm");
    add(i % 12 == 3, "grp_c_attention");
    add(i % 10 == 2, "grp_c_sleep");
    add(i % 25 == 11, "grp_z_ai");
    add(i % 9 == 4, "grp_c_llm");
    add(i % 9 == 4 || i % 18 == 8, "grp_c_rag");
    add(i % 14 == 6, "grp_c_align");
    add(i % 15 == 10, "grp_c_multi");
    add(i % 6 == 0, "grp_c_toread");
    add(i % 12 == 0, "grp_c_week");
    out
}

fn root_of(folders: &[Folder], id: &str) -> String {
    let mut current = id.to_string();
    for _ in 0..folders.len() {
        match folders.iter().find(|f| f.id == current).and_then(|f| f.parent.clone()) {
            Some(parent) => current = parent,
            None => break,
        }
    }
    current
}

/// The node and all its descendants.
fn subtree(folders: &[Folder], id: &str) -> Vec<String> {
    let mut out: Vec<String> = folders.iter().filter(|f| f.id == id).map(|f| f.id.clone()).collect();
    let mut i = 0;
    while i < out.len() {
        let parent = out[i].clone();
        out.extend(folders.iter().filter(|f| f.parent.as_deref() == Some(parent.as_str())).map(|f| f.id.clone()));
        i += 1;
    }
    out
}

fn transfer_folders(state: &mut State, ids: &[String], target: &str, expected: &std::collections::BTreeMap<String, String>, moving: bool) -> Result<(), &'static str> {
    use std::collections::{BTreeMap, BTreeSet};
    if !state.controls.transfer_folders_op || ids.is_empty() || ids.len() > crate::MAX_GROUP_EDIT_ITEMS { return Err("invalid_group_edit"); }
    let mut nodes = BTreeSet::new();
    let mut versions = BTreeMap::new();
    let mut tops = vec![];
    for id in ids {
        let mut node = state.folders.iter().find(|f| &f.id == id).ok_or("library_resource_not_found")?;
        if node.parent.is_none() { return Err("invalid_group_edit"); }
        let mut covered = false;
        while let Some(parent) = &node.parent {
            covered |= ids.contains(parent);
            node = state.folders.iter().find(|f| &f.id == parent).ok_or("library_resource_not_found")?;
            versions.insert(node.id.clone(), format!("v{}", node.version));
        }
        if !covered && !tops.contains(id) { tops.push(id.clone()); }
        nodes.extend(subtree(&state.folders, id));
    }
    if nodes.contains(target) { return Err("invalid_group_edit"); }
    for f in state.folders.iter().filter(|f| nodes.contains(&f.id)) { versions.insert(f.id.clone(), format!("v{}", f.version)); }
    if &versions != expected { return Err("stale_group_version"); }
    let items: BTreeSet<_> = state.entries.iter().filter(|e| e.groups.iter().any(|id| nodes.contains(id))).map(|e| e.item.item_id.as_str()).collect();
    if state.group_trash.iter().any(|((root, item), (_, members))| members.iter().any(|id| nodes.contains(id))
        || (root == &root_of(&state.folders, target) && items.contains(item.as_str()))) {
        return Err("item_trashed");
    }
    let mut bump = BTreeSet::from([target.to_owned()]);
    if moving {
        let changed: Vec<_> = tops.iter().filter(|id| state.folders.iter().any(|f| &f.id == *id && f.parent.as_deref() != Some(target))).cloned().collect();
        if changed.is_empty() { return Err("groups_unchanged"); }
        for f in &state.folders { if changed.contains(&f.id) { bump.insert(f.parent.clone().unwrap()); } }
        for f in &mut state.folders {
            if changed.contains(&f.id) { f.parent = Some(target.into()); }
            if nodes.contains(&f.id) { bump.insert(f.id.clone()); }
        }
    } else {
        let identities: BTreeMap<_, _> = nodes.iter().enumerate().map(|(i, id)| (id.clone(), format!("grp_new_{}", state.next_group + i as u64))).collect();
        let copies: Vec<_> = state.folders.iter().filter(|f| nodes.contains(&f.id)).map(|f| Folder {
            id: identities[&f.id].clone(), name: f.name.clone(), kind: "collection".into(),
            parent: Some(if tops.contains(&f.id) { target.into() } else { identities[f.parent.as_ref().unwrap()].clone() }), version: 1,
        }).collect();
        for e in &mut state.entries {
            let added: Vec<_> = e.groups.iter().filter_map(|id| identities.get(id).cloned()).collect();
            e.groups.extend(added);
        }
        state.next_group += copies.len() as u64;
        state.folders.extend(copies);
    }
    for f in &mut state.folders { if bump.contains(&f.id) { f.version += 1; } }
    Ok(())
}

/// Whether `entry` matches a folder scope: a direct member, or (with descendants)
/// a member of any node in the subtree. Each entry is tested once, so an item in
/// several of those folders still counts once.
fn in_scope(entry: &Entry, nodes: &[String]) -> bool {
    entry.groups.iter().any(|g| nodes.contains(g))
}

/// Preorder (roots, then children depth-first; siblings by name, then id).
fn preorder(folders: &[Folder]) -> Vec<&Folder> {
    fn visit<'a>(folders: &'a [Folder], parent: Option<&str>, out: &mut Vec<&'a Folder>) {
        let mut children: Vec<&Folder> = folders.iter().filter(|f| f.parent.as_deref() == parent).collect();
        if parent.is_some() {
            children.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        } else {
            children.sort_by_key(|f| !(f.kind == "user" && f.name.eq_ignore_ascii_case("Main")));
        }
        for child in children {
            out.push(child);
            visit(folders, Some(&child.id), out);
        }
    }
    let mut out = vec![];
    visit(folders, None, &mut out);
    out
}

fn valid_group_name(name: &str) -> bool {
    let trimmed = name.trim();
    !trimmed.is_empty() && trimmed.len() <= 200 && !trimmed.chars().any(char::is_control)
}

// ---------------------------------------------------------------- translation
// English-titled items carry synthetic English text; their synthetic Chinese
// translation keeps the same blank-line blocks, with code, images and URLs passed
// through unchanged. Chinese items need no translation.

const EN_TITLES_ZH: [(&str, &str); 4] = [
    ("Attention Is Not All You Need: Revisiting Recurrent Memory", "注意力并非全部：重新审视循环记忆"),
    ("A Survey on Retrieval-Augmented Generation", "检索增强生成综述"),
    ("Designing for Human-AI Collaboration", "为人机协作而设计"),
    ("Graph Neural Networks for Knowledge Retrieval", "面向知识检索的图神经网络"),
];

/// (original, translation) block pairs of an English item.
fn english_blocks(seed: usize, title: &str, title_zh: &str) -> Vec<(String, String)> {
    let para = |n: usize| {
        (
            format!("This is synthetic paragraph {n} (sample {seed}). Retrieval-augmented generation pairs a language model with a search step, so that an answer can point to the passages it relies on. The text exists only to test bilingual layout, line wrapping and the 🌹 emoji."),
            format!("这是第 {n} 段合成段落（样本 {seed}）。检索增强生成把语言模型与检索步骤结合起来，使回答能够指向它所依据的段落。本段仅用于检验双语排版、换行与 🌹 表情的显示。"),
        )
    };
    let same = |s: &str| (s.to_string(), s.to_string());
    let mut out = vec![(format!("# {title}"), format!("# {title_zh}")), ("## 1. Introduction".into(), "## 1. 引言".into()), para(1), para(2)];
    out.push(same("```text\nquery -> retrieve(top_k = 8) -> rerank -> generate\n```"));
    out.push(same("![Figure 1: synthetic retrieval pipeline](images/fixture-pipeline.png)"));
    out.push(("## 2. Method".into(), "## 2. 方法".into()));
    out.push(para(3));
    out.push(same("https://example.invalid/synthetic-dataset"));
    out.push(para(4));
    out.push(("## 3. Discussion".into(), "## 3. 讨论".into()));
    out.push(para(5));
    out.push(para(6));
    out
}

/// More sections for the long showcase item.
fn more_english_blocks(seed: usize, out: &mut Vec<(String, String)>) {
    for (n, (en, zh)) in [(4, ("Experiments", "实验")), (5, ("Limitations", "局限")), (6, ("Conclusion", "结论"))] {
        out.push((format!("## {n}. {en}"), format!("## {n}. {zh}")));
        for k in 0..4 {
            let p = n * 10 + k;
            out.push((
                format!("Synthetic paragraph {p} (sample {seed}) describes a recurrent memory slot that is written once per passage and read before every answer. Nothing here reports a real experiment; it only gives the reader a long English text to lay out."),
                format!("合成段落 {p}（样本 {seed}）描述了一个循环记忆槽：每读一段写入一次，每次回答前先读取。这里没有任何真实实验，只为阅读器提供一篇较长的英文文本。"),
            ));
        }
    }
}

const SHOWCASE_MINUTES: &str = "（合成纪要，仅供界面演示）\n\n本文回顾了循环记忆在长序列建模中的作用，认为单靠注意力难以同时兼顾长程依赖与计算成本。\n\n· 方法：在检索增强生成流程中加入可写入的循环记忆槽，按段落更新。\n· 结果：在三个合成数据集上，长文档问答的引用准确率有所提升，推理开销基本不变。\n· 局限：记忆槽容量固定，跨文档迁移尚未验证。\n\n背景\n\n· 长文档问答常把整篇切块后逐段检索，段落之间的指代与前后文关系在切块时丢失。\n· 作者认为加长上下文窗口只能缓解问题，计算量随长度增长，模型仍难稳定利用中段信息。\n\n方法细节\n\n· 每读一段，模型把要点写入固定数量的记忆槽，旧内容按重要度被覆盖。\n· 回答前先读取记忆槽，再与检索到的段落一起作为上下文，引用仍指向原文位置。\n· 记忆槽的写入规则在合成训练集上学习，没有使用任何真实数据。\n\n结果与讨论\n\n· 在需要跨越多个段落的问题上收益最明显，单段即可回答的问题几乎没有变化。\n· 记忆槽越多，引用准确率先升后平，超过一定数量后收益有限。\n· 作者承认合成数据集偏简单，真实长文档上的效果需要进一步验证。\n\n可追问的问题\n\n· 记忆槽被覆盖时，是否会丢掉后文才用得到的信息？\n· 与直接加长上下文相比，在相同计算预算下哪种更划算？\n· 引用准确率的评估是否区分了“引对段落”和“引对句子”？";
const ENGLISH_MINUTES: &str = "（合成纪要）本文介绍了检索增强生成的基本流程：先检索相关段落，再让模型据此作答，并保留可核对的引用。";
const CHINESE_MINUTES: &str = "（合成纪要）本文梳理了相关研究的主要问题与改进思路，并用合成样本说明了长文档检索增强生成的取舍。";

fn site_origin(site: Option<&str>) -> Option<String> {
    match site {
        Some("示例期刊网") => Some("https://journal.example".into()),
        Some("arxiv.org") => Some("https://arxiv.org".into()),
        Some("知识周报") => Some("https://weekly.example".into()),
        Some(other) => Some(other.into()),
        None => None,
    }
}

fn join(blocks: impl Iterator<Item = String>) -> String {
    let mut out = blocks.collect::<Vec<_>>().join("\n\n");
    out.push('\n');
    out
}

// ---------------------------------------------------------------- bibliographic

const PEOPLE: [(&str, &str); 8] = [
    ("Example", "Ada"),
    ("林", "晓"),
    ("Sample", "Ben"),
    ("周", "一然"),
    ("Fixture", "Cara"),
    ("Quinn", "Dana"),
    ("陈", "思远"),
    ("Placeholder", "Eli"),
];
const JOURNALS: [&str; 4] = ["示例心理学报", "Journal of Synthetic Studies", "城市研究（合成）", "Synthetic Review of Cognition"];
const PUBLISHERS: [&str; 3] = ["示例出版社", "Synthetic Press", "样本大学出版社"];
const PLACES: [&str; 3] = ["北京", "London", "上海"];
const KEYWORDS: [&str; 8] = ["judgment", "heuristic", "检索增强", "城市更新", "decision making", "认知偏差", "公共空间", "methodology"];

fn person(i: usize, role: &str) -> Creator {
    let (family, given) = PEOPLE[i % PEOPLE.len()];
    let cjk = !family.is_ascii();
    Creator {
        name_type: Some("person".into()),
        family_name: Some(family.into()),
        given_name: Some(given.into()),
        literal_name: None,
        display_name: if cjk { format!("{family}{given}") } else { format!("{given} {family}") },
        role: role.into(),
        creator_id: Some(format!("creator_fixture_{i}_{role}")),
        institution_ids: vec![],
    }
}


fn issn(stem7: &str) -> String {
    let sum: u32 = stem7.bytes().enumerate().map(|(k, b)| (b - b'0') as u32 * (8 - k as u32)).sum();
    let check = (11 - sum % 11) % 11;
    let c = if check == 10 { 'X' } else { char::from(b'0' + check as u8) };
    format!("{}-{}{c}", &stem7[..4], &stem7[4..])
}

fn date_for(i: usize, first_year: usize, precisions: &[DatePrecision]) -> Option<PartialDate> {
    let year = first_year + (i * 7) % (2026 - first_year);
    let (month, day) = (1 + (i * 5 + 2) % 12, 1 + (i * 11 + 6) % 28);
    // Items of one type share `i % 4`, so the variant follows `i / 4`.
    Some(match precisions[(i / 4) % precisions.len()] {
        DatePrecision::Year => PartialDate { value: Some(format!("{year}")), precision: DatePrecision::Year, raw: Some(format!("{year}")) },
        DatePrecision::Month => PartialDate { value: Some(format!("{year}-{month:02}")), precision: DatePrecision::Month, raw: Some(format!("{year}年{month}月")) },
        DatePrecision::Day => PartialDate { value: Some(format!("{year}-{month:02}-{day:02}")), precision: DatePrecision::Day, raw: Some(format!("{year}-{month:02}-{day:02}")) },
        DatePrecision::Unparsed => PartialDate { value: None, precision: DatePrecision::Unparsed, raw: Some(format!("{year}年春季")) },
    })
}

/// The list summary of the current revision, as the agent projects it: a
/// publication day only at day precision (year/month/unparsed are null, never a
/// capture date), and the duration as stored.
fn summary_dates(b: &Bibliographic) -> (Option<String>, Option<u32>) {
    let day = b.published_date.as_ref().filter(|d| d.precision == DatePrecision::Day).and_then(|d| d.value.clone());
    (day, b.duration_seconds)
}

/// Synthetic bibliographic fields by item type; every value is invented.
fn biblio_for(i: usize, item: &LibraryItem) -> Bibliographic {
    use DatePrecision::*;
    // Items of one type share `i % 4`; pools are indexed by `v` for variety.
    let v = i / 4;
    let language = Some(if item.title.is_ascii() { "en" } else { "zh" }.to_string());
    let keywords = vec![KEYWORDS[i % KEYWORDS.len()].to_string(), KEYWORDS[(i + 3) % KEYWORDS.len()].to_string()];
    // added_at is the (synthetic) commit time, later than the capture and often
    // past local midnight; legacy items without inclusion evidence have none.
    let added_at = (i % 5 != 4 && i % 7 != 3).then(|| item.captured_at.as_ref().map(|c| format!("{}T17:{:02}:00Z", &c[..10], (i * 7) % 60))).flatten();
    let base = Bibliographic { language, keywords, added_at, ..Default::default() };
    match item.item_type.as_str() {
        "论文" => Bibliographic {
            item_type: "journal_article".into(),
            creators: (0..1 + v % 3).map(|k| person(v * 3 + k, "author")).collect(),
            identifiers: vec![
                // Item 49 shares item 1's DOI: a synthetic duplicate for the check.
                Identifier::input("doi", &format!("10.5555/fixture.{:04}", if i == 49 { 1 } else { i }), None),
                Identifier::input("issn", &issn(&format!("{:07}", 1_000_000 + (v % 4) * 4_321)), Some("print".into())),
            ],
            published_date: date_for(i, 1974, &[Day, Year, Month, Unparsed]),
            journal_title: Some(JOURNALS[v % JOURNALS.len()].into()),
            volume: Some(format!("{}", 12 + i % 40)),
            issue: Some(format!("{}", 1 + i % 6)),
            pages: Some(format!("{}–{}", 100 + (i * 7) % 900, 118 + (i * 7) % 900)),
            ..base
        },
        "书籍" => Bibliographic {
            item_type: "book".into(),
            creators: {
                let mut c = vec![person(v * 5 + 1, "author")];
                if (i / 4) % 2 == 1 {
                    c.push(person(v * 5 + 4, "translator"));
                }
                c
            },
            identifiers: vec![Identifier::input("isbn", &fb::isbn13_from_stem(&format!("9787{:08}", 50_000_000 + i * 37)), Some("print".into()))],
            published_date: date_for(i, 1960, &[Month, Year]),
            publisher: Some(PUBLISHERS[v % PUBLISHERS.len()].into()),
            publication_place: Some(PLACES[v % PLACES.len()].into()),
            edition: Some(if (i / 4) % 2 == 0 { "第 1 版".into() } else { "2nd ed.".into() }),
            number_of_pages: Some((180 + (i * 13) % 420) as u32),
            series_title: (v % 3 == 0).then(|| "示例思想丛书".into()),
            series_number: (v % 3 == 0).then(|| format!("{}", 1 + v % 9)),
            ..base
        },
        "网页文章" => Bibliographic {
            item_type: "webpage".into(),
            creators: if (i / 4) % 3 == 0 {
                vec![Creator { name_type: Some("organization".into()), family_name: None, given_name: None, literal_name: Some("示例研究所（合成）".into()), display_name: "示例研究所（合成）".into(), role: "author".into(), creator_id: Some(format!("creator_fixture_{i}_org")), institution_ids: vec![] }]
            } else {
                vec![person(v * 7 + 2, "author")]
            },
            published_date: date_for(i, 2012, &[Day, Unparsed]),
            site_name: item.site_name.clone(),
            ..base
        },
        // Other types (PDF here) are retained verbatim, not recast.
        other => Bibliographic {
            item_type: other.into(),
            creators: vec![person(v * 3 + 5, "author")],
            published_date: date_for(i, 1990, &[Year]),
            ..base
        },
    }
}

impl FixtureBackend {
    /// `count` synthetic items, newest first, deterministic.
    pub fn new(count: usize) -> Self {
        let mut entries = Vec::with_capacity(count);
        for i in 0..count {
            let base = TITLES[i % TITLES.len()];
            let title = if i < TITLES.len() { base.to_string() } else { format!("{base}（合成样本 {i}）") };
            let day = 28 - (i % 28);
            let month = 10 - (i / 28) % 10;
            let mut item = LibraryItem {
                item_id: format!("item_fixture_{i:06}"),
                revision_id: format!("rev_fixture_{i:06}_1"),
                title: title.clone(),
                item_type: TYPES[i % TYPES.len()].into(),
                captured_at: Some(format!("2026-{month:02}-{day:02}T09:{:02}:00Z", i % 60)),
                site_name: if i % 7 == 6 { None } else { Some(SOURCES[i % 3].into()) },
                published_at: None,
                duration_seconds: None,
            };
            let english = EN_TITLES_ZH.iter().find(|(en, _)| *en == base).map(|(_, zh)| *zh);
            let showcase = i == 26;
            if showcase {
                // A captured web article, so the panel shows 网站 and 网址.
                item.item_type = TYPES[0].into();
            }
            let (markdown, translation) = match english {
                Some(zh_base) => {
                    let title_zh = title.replacen(base, zh_base, 1);
                    let mut pairs = english_blocks(i, &title, &title_zh);
                    if showcase {
                        more_english_blocks(i, &mut pairs);
                    }
                    let markdown = join(pairs.iter().map(|p| p.0.clone()));
                    // Mismatched case: the translation lost a block, so it cannot be paired.
                    let lost = usize::from(i % 12 == 6);
                    let translated = join(pairs[..pairs.len() - lost].iter().map(|p| p.1.clone()));
                    let done = Translation { state: AnalysisState::Succeeded, reason: None, title: Some(title_zh.clone()), markdown: Some(translated.clone()), started: None, pending: None, skip: None, batches: (TRANSLATION_BATCHES, TRANSLATION_BATCHES), title_done: true, resume: 0, wall_start: None, failure_detail: None, untranslated: None };
                    let open = Translation { state: AnalysisState::NotRequested, reason: None, title: None, markdown: None, started: None, pending: Some((title_zh, translated)), skip: None, batches: (0, TRANSLATION_BATCHES), title_done: false, resume: 0, wall_start: None, failure_detail: None, untranslated: None };
                    let t = match i % 12 {
                        _ if showcase => done,
                        // Stopped after 5 of 18 batches; the title step had succeeded.
                        2 if i == 14 => Translation { state: AnalysisState::Failed, reason: Some("provider_timeout".into()), batches: (5, 18), title_done: true, ..open },
                        2 if i % 24 == 14 => Translation { state: AnalysisState::Failed, reason: Some("provider_timeout".into()), ..open },
                        // A new capture: only its title was translated (title_only).
                        2 if i == 2 => Translation { title_done: true, ..open },
                        2 => open,
                        _ => done,
                    };
                    (markdown, t)
                }
                None => (
                    markdown_for(i, &title),
                    Translation { state: AnalysisState::NotRequested, reason: None, title: None, markdown: None, started: None, pending: None, skip: Some("source_is_chinese"), batches: (0, 0), title_done: false, resume: 0, wall_start: None, failure_detail: None, untranslated: None },
                ),
            };
            // The Markdown sample (English, translated), for the reader's rendering checks.
            let (markdown, translation) = if i == MARKDOWN_INDEX {
                item.title = MARKDOWN_TITLE.into();
                let done = Translation { state: AnalysisState::Succeeded, reason: None, title: Some(MARKDOWN_TITLE_ZH.into()), markdown: Some(MARKDOWN_ZH.into()), started: None, pending: None, skip: None, batches: (TRANSLATION_BATCHES, TRANSLATION_BATCHES), title_done: true, resume: 0, wall_start: None, failure_detail: None, untranslated: None };
                (MARKDOWN_EN.to_string(), done)
            } else {
                (markdown, translation)
            };
            // The paired book: canonical Markdown is its Chinese translation (as a real
            // paired import); the English original is served only through `reader`.
            let (markdown, translation) = if i == crate::fixture_book::BOOK_INDEX {
                item.title = crate::fixture_book::BOOK_TITLE.into();
                item.item_type = TYPES[3].into();
                let skip = Translation { state: AnalysisState::NotRequested, reason: None, title: Some(crate::fixture_book::BOOK_TITLE_ZH.into()), markdown: None, started: None, pending: None, skip: Some("source_is_chinese"), batches: (0, 0), title_done: true, resume: 0, wall_start: None, failure_detail: None, untranslated: None };
                (crate::fixture_book::canonical_markdown(), skip)
            } else {
                (markdown, translation)
            };
            let (markdown, translation) = if i == GUJI_INDEX {
                item.title = GUJI_TITLE.into();
                (GUJI_MARKDOWN.to_string(), Translation { state: AnalysisState::NotRequested, reason: None, title: None, markdown: None, started: None, pending: None, skip: Some("source_is_chinese"), batches: (0, 0), title_done: false, resume: 0, wall_start: None, failure_detail: None, untranslated: None })
            } else {
                (markdown, translation)
            };
            let (markdown, translation) = if i == LONGREAD_INDEX {
                item.title = LONGREAD_TITLE.into();
                (LONGREAD_MARKDOWN.to_string(), Translation { state: AnalysisState::NotRequested, reason: None, title: None, markdown: None, started: None, pending: None, skip: Some("source_is_chinese"), batches: (0, 0), title_done: false, resume: 0, wall_start: None, failure_detail: None, untranslated: None })
            } else {
                (markdown, translation)
            };
            // The AI-translated book: canonical Markdown is its (Chinese) translation edition.
            let (markdown, translation) = if i == crate::fixture_ai_book::AI_BOOK_INDEX {
                item.title = crate::fixture_ai_book::AI_BOOK_TITLE.into();
                item.item_type = TYPES[3].into();
                let skip = Translation { state: AnalysisState::NotRequested, reason: None, title: Some(crate::fixture_ai_book::AI_BOOK_TITLE_ZH.into()), markdown: None, started: None, pending: None, skip: Some("source_is_chinese"), batches: (0, 0), title_done: true, resume: 0, wall_start: None, failure_detail: None, untranslated: None };
                (crate::fixture_ai_book::canonical_markdown(), skip)
            } else {
                (markdown, translation)
            };
            let (markdown, translation) = if i == PODCAST_INDEX {
                item.title = PODCAST_TITLE.into();
                let done = Translation { state: AnalysisState::Succeeded, reason: None, title: Some(PODCAST_TITLE_ZH.into()), markdown: Some(PODCAST_ZH.into()), started: None, pending: None, skip: None, batches: (TRANSLATION_BATCHES, TRANSLATION_BATCHES), title_done: true, resume: 0, wall_start: None, failure_detail: None, untranslated: None };
                (PODCAST_EN.to_string(), done)
            } else {
                (markdown, translation)
            };
            let english = if i == MARKDOWN_INDEX { Some(MARKDOWN_TITLE_ZH) } else if i == PODCAST_INDEX { Some(PODCAST_TITLE_ZH) }
                else if i == crate::fixture_book::BOOK_INDEX || i == GUJI_INDEX || i == LONGREAD_INDEX || i == crate::fixture_ai_book::AI_BOOK_INDEX { None } else { english };
            let mut biblio = biblio_for(i, &item);
            // Synthetic durations (every value invented): the podcast, and a few
            // others for the formats (under a minute, just under an hour, over one).
            if i == PODCAST_INDEX {
                biblio.published_date = Some(PartialDate { value: Some("2026-10-30".into()), precision: DatePrecision::Day, raw: Some("2026-10-30".into()) });
                biblio.duration_seconds = Some(3626);
            } else if i % 9 == 4 && item.item_type != TYPES[3] {
                biblio.duration_seconds = Some([59, 3599, 1834, 7265][(i / 9) % 4]);
            }
            let host = site_origin(item.site_name.as_deref()).unwrap_or_else(|| "https://example.invalid".into());
            biblio.source_url = Some(format!("{host}/fixture/{i:06}"));
            let minutes_of = |state: MinutesState, reason: Option<&str>, text: Option<&str>, pending: &str| Minutes { state, reason: reason.map(str::to_string), text: text.map(str::to_string), started: None, pending: pending.to_string(), fail_once: i == 10 };
            let minutes = match (&translation.state, english.is_some()) {
                _ if showcase => {
                    // Model enrichment: keywords, creators and date from the synthetic model;
                    // no site name, so the panel shows the URL host as a placeholder.
                    biblio.site_name = None;
                    biblio.language = Some("en".into());
                    biblio.abstract_text = Some("A synthetic abstract: recurrent memory slots are revisited as a complement to attention in retrieval-augmented generation. (Fixture text, not a real paper.)".into());
                    biblio.keywords = vec!["recurrent memory".into(), "retrieval-augmented generation".into(), "长文档".into()];
                    biblio.source_url = Some("https://weekly.example/articles/recurrent-memory-26?ref=synthetic".into());
                    biblio.canonical_url = Some("https://weekly.example/a/recurrent-memory".into());
                    for field in ["keywords", "creators", "published_date"] {
                        biblio.origins.insert(field.into(), "model_enrichment".into());
                    }
                    minutes_of(MinutesState::Succeeded, None, Some(SHOWCASE_MINUTES), SHOWCASE_MINUTES)
                }
                _ if i == MARKDOWN_INDEX => minutes_of(MinutesState::Succeeded, None, Some(MARKDOWN_MINUTES), MARKDOWN_MINUTES),
                _ if i == LONGREAD_INDEX => minutes_of(MinutesState::Succeeded, None, Some(LONGREAD_MINUTES), LONGREAD_MINUTES),
                // The failing-minutes item starts with none, so it can be asked.
                _ if i == 10 => minutes_of(MinutesState::NotRequested, None, None, if english.is_some() { ENGLISH_MINUTES } else { CHINESE_MINUTES }),
                (AnalysisState::Succeeded, true) => minutes_of(MinutesState::Succeeded, None, Some(ENGLISH_MINUTES), ENGLISH_MINUTES),
                (AnalysisState::Failed, true) => minutes_of(MinutesState::Failed, Some("provider_timeout"), None, ENGLISH_MINUTES),
                (_, true) => minutes_of(MinutesState::NotRequested, None, None, ENGLISH_MINUTES),
                (_, false) => minutes_of(MinutesState::NotRequested, None, None, CHINESE_MINUTES),
            };
            use crate::fixture_insights::Start;
            let start = match i {
                _ if showcase || i == LONGREAD_INDEX => Start::Ready,
                5 => Start::ReadyOriginalOnly,
                9 => Start::Stale,
                _ => Start::NotRequested,
            };
            let eventual = translation.markdown.as_deref().or(translation.pending.as_ref().map(|p| p.1.as_str()));
            let insights = crate::fixture_insights::Insights::new(&markdown, eventual, english.is_some(), start, i % 24 == 14);
            entries.push(Entry {
                creation_origin: if i % 3 == 0 { ItemOrigin::Import } else { ItemOrigin::Capture },
                minutes,
                insights,
                biblio,
                groups: memberships(i),
                item,
                tags: vec![TAGS[i % TAGS.len()].into(), TAGS[(i + 1) % TAGS.len()].into()],
                revision_number: 1,
                summary: (i % 4 != 3).then(|| {
                    "本文系统梳理了相关方法，并在多个合成数据集上进行了实证比较。结果表明结构化方法在长文档上更稳定。（合成摘要）".into()
                }),
                translation,
                markdown,
            });
        }
        // Recoverable trash: synthetic second copies of a few items, moved out by a
        // (synthetic) dedup run. Their ids are past the library's own.
        let trash = TRASHED_COPIES
            .iter()
            .enumerate()
            .filter_map(|(k, (kept, minutes))| {
                let original = entries.get(*kept)?;
                let mut e: Entry = original.clone();
                let n = count + k;
                e.item.item_id = format!("item_fixture_{n:06}");
                e.item.revision_id = format!("rev_fixture_{n:06}_1");
                e.biblio.source_url = original.biblio.source_url.as_ref().map(|u| format!("{u}?utm_source=synthetic-dup"));
                e.groups.clear();
                let meta = Trashed { trashed_at: format!("2026-11-01T{:02}:{:02}:00Z", 8 + k, 5 + *minutes), kept: original.item.item_id.clone(), plan_id: "plan_fixture_dedup_1".into() };
                Some((count + k, e, meta))
            })
            .collect();
        let folders = GROUPS.iter().map(|(id, name, kind, parent)| Folder { id: (*id).into(), name: (*name).into(), kind: (*kind).into(), parent: parent.map(str::to_string), version: 1 }).collect();
        Self {
            state: Arc::new(Mutex::new(State {
                entries,
                reading_annotations: HashMap::new(),
                reader: Default::default(),
                passes: Default::default(),
                markdown_requests: HashMap::new(),
                controls: FixtureControls::default(),
                generation: 1,
                models: Default::default(),
                biblio: Default::default(),
                folders,
                groups_version: 1,
                group_requests: HashMap::new(),
                folder_receipts: HashMap::new(),
                next_group: 1,
                chat_requests: HashMap::new(),
                chat_history: HashMap::from([(CHAT_HISTORY_ITEM.to_string(), synthetic_history()), (LONGREAD_ITEM.to_string(), synthetic_history())]),
                trash,
                group_trash: HashMap::new(),
                group_trash_requests: HashMap::new(),
                book: Default::default(),
                ai_book: Default::default(),
                covers: Default::default(),
                deletions: vec![],
                delete_tokens: HashMap::new(),
                delete_requests: HashMap::new(),
                next_deletion: 1,
                active_group_calls: vec![],
                active_group_requests: HashMap::new(),
                active_group: None,
                capture_jobs: vec![],
                capture_job_calls: 0,
            })),
        }
    }

    /// Development: spreads the first items over real-looking sites (sub-sites of
    /// one publication, single-item sources) to show the grouped 来源 sidebar.
    pub fn vary_sources(&self) {
        const SITES: [&str; 32] = [
            "https://36kr.com", "https://www.36kr.com", "https://www.36kr.com", "https://m.36kr.com", "https://36kr.com", "https://www.36kr.com",
            "https://sspai.com", "https://sspai.com", "https://sspai.com", "https://mp.weixin.qq.com", "https://mp.weixin.qq.com", "https://mp.weixin.qq.com",
            "https://mp.weixin.qq.com", "https://news.sina.com.cn", "https://finance.sina.com.cn", "https://www.youtube.com", "https://www.youtube.com",
            "https://youtu.be", "https://github.com", "https://github.com", "https://www.huxiu.com", "https://www.huxiu.com", "https://www.latepost.com",
            "https://blog.one.example", "https://two.example", "https://www.bbc.co.uk", "https://news.ycombinator.com", "https://www.ft.com",
            "https://example.org", "https://notes.example.net", "https://www.zhihu.com", "https://zhuanlan.zhihu.com",
        ];
        let mut state = self.state.lock().unwrap();
        for (e, site) in state.entries.iter_mut().zip(SITES) {
            e.item.site_name = Some(site.into());
        }
        state.generation += 1;
    }

    /// Development: starts from a library with no folders at all.
    pub fn clear_groups(&self) {
        let mut state = self.state.lock().unwrap();
        state.folders.clear();
        for e in &mut state.entries {
            e.groups.clear();
        }
        state.groups_version += 1;
        state.generation += 1;
    }

    /// Synthetic file import (`FixtureImport`): a new item named after the file,
    /// first in the list, in `group` (or Inbox when `None`). Only the name and
    /// type are used; no file is read. Returns its id.
    pub fn add_imported_item(&self, title: &str, item_type: &str, group: Option<&str>) -> Option<String> {
        let mut state = self.state.lock().unwrap();
        let n = state.entries.iter().filter(|e| e.item.item_id.starts_with("item_fixture_import_")).count();
        let mut entry = state.entries.first()?.clone();
        let (id, revision) = (format!("item_fixture_import_{n:03}"), format!("rev_fixture_import_{n:03}_1"));
        entry.item = LibraryItem { item_id: id.clone(), revision_id: revision.clone(), title: title.into(), item_type: item_type.into(), captured_at: Some("2026-11-08T09:30:00Z".into()), site_name: None, published_at: None, duration_seconds: None };
        entry.creation_origin = ItemOrigin::Import;
        entry.markdown = format!("# {title}\n\n合成导入样本：只按文件名和类型生成，没有读取文件内容。");
        entry.tags.clear();
        entry.revision_number = 1;
        entry.summary = None;
        entry.groups = group.map(|g| vec![g.to_string()]).unwrap_or_default();
        entry.biblio.item_id = id.clone();
        entry.biblio.revision_id = revision;
        entry.translation = Translation { state: AnalysisState::NotRequested, reason: None, title: None, markdown: None, started: None, pending: None, skip: None, batches: (0, TRANSLATION_BATCHES), title_done: false, resume: 0, wall_start: None, failure_detail: None, untranslated: None };
        entry.insights = crate::fixture_insights::Insights::new(&entry.markdown, None, false, crate::fixture_insights::Start::NotRequested, false);
        state.entries.insert(0, entry);
        state.groups_version += 1;
        state.generation += 1;
        Some(id)
    }

    /// The next chapter repair job fails (`reader_repair_failed`).
    pub fn fail_next_repair(&self) {
        self.state.lock().unwrap().book.fail_next = true;
    }

    pub fn set_controls(&self, controls: FixtureControls) {
        self.state.lock().unwrap().controls = controls;
    }

    pub fn controls(&self) -> FixtureControls {
        self.state.lock().unwrap().controls.clone()
    }

    /// Puts an item's translation in a newer agent's diagnostic state.
    pub fn set_translation_demo(&self, item_id: &str, demo: TranslationDemo) {
        let mut state = self.state.lock().unwrap();
        let Some(e) = find_mut(&mut state.entries, item_id) else { return };
        let t = &mut e.translation;
        t.started = None;
        t.skip = None;
        t.wall_start = None;
        t.failure_detail = None;
        t.untranslated = None;
        t.title_done = true;
        match demo {
            TranslationDemo::Running { done, total, minutes } => {
                t.state = AnalysisState::Running;
                t.reason = None;
                t.batches = (done, total);
                t.wall_start = Some(crate::dto::unix_now() - minutes * 60);
            }
            TranslationDemo::Partial { untranslated } => {
                t.state = AnalysisState::Succeeded;
                t.reason = None;
                t.batches = (t.batches.1, t.batches.1);
                if let Some((title, markdown)) = t.pending.clone() {
                    t.title.get_or_insert(title);
                    t.markdown.get_or_insert(markdown);
                }
                t.untranslated = Some(untranslated);
            }
            TranslationDemo::Failed { done, total, detail } => {
                t.state = AnalysisState::Failed;
                t.reason = Some("retry_exhausted".into());
                t.batches = (done, total);
                t.failure_detail = Some(detail);
            }
        }
    }

    /// Every `ui_active_group_set` received, in order: (request_id, group_id).
    pub fn active_group_calls(&self) -> Vec<(String, Option<String>)> {
        self.state.lock().unwrap().active_group_calls.clone()
    }

    /// The selection the agent would file captures into.
    pub fn active_group(&self) -> Option<String> {
        self.state.lock().unwrap().active_group.clone()
    }

    /// Replaces the synthetic URL capture queue (already in reply order).
    pub fn set_capture_jobs(&self, jobs: Vec<crate::capture_jobs::CaptureJob>) {
        self.state.lock().unwrap().capture_jobs = jobs;
    }

    /// How many `capture_jobs_recent` reached the fixture.
    pub fn capture_job_calls(&self) -> usize {
        self.state.lock().unwrap().capture_job_calls
    }

    /// Makes every stored lookup preview expired (as after its TTL).
    pub fn expire_lookup_previews(&self) {
        self.state.lock().unwrap().biblio.expire_previews();
    }

    /// Development and tests: a clearly synthetic reading history for `item_id`
    /// (a 补记, a completed pass, an unfinished one with time from two devices).
    pub fn seed_reading_passes(&self, item_id: &str) {
        let mut state = self.state.lock().unwrap();
        let Some(text) = state.entries.iter().find(|e| e.item.item_id == item_id).map(pass_text) else { return };
        state.passes.seed(item_id, &text);
    }

    /// Simulates an edit made elsewhere (for example on the phone): bumps the revision.
    pub fn simulate_external_edit(&self, item_id: &str) {
        let mut state = self.state.lock().unwrap();
        if let Some(entry) = state.entries.iter_mut().find(|e| e.item.item_id == item_id) {
            bump(entry);
            entry.item.title.push_str("（他处已修改）");
        }
        state.generation += 1;
    }

    /// A retranslation of the AI book's `chapter` publishes (new version and hash):
    /// AI marks with a segment there turn stale.
    pub fn retranslate_ai_chapter(&self, chapter: &str) {
        self.state.lock().unwrap().ai_book.retranslate(chapter);
    }

    /// The AI book and AI-mark requests, kept out of `handle` (see `handle_passes`).
    #[inline(never)]
    fn handle_ai_book(&self, command: &Command) -> Option<Response> {
        let Command::Reader { max_sensitivity, request } = command else { return None };
        if !crate::fixture_ai_book::handles(request) { return None; }
        let mut state = self.state.lock().unwrap();
        if state.controls.legacy_agent { return Some(Response { ok: false, reason_code: Some("invalid_request".into()), ..Default::default() }); }
        if *max_sensitivity < Sensitivity::Private { return Some(Response { ok: false, reason_code: Some("library_resource_not_found".into()), ..Default::default() }); }
        let st = &mut *state;
        let entries = &st.entries;
        let revision = |id: &str| entries.iter().find(|e| e.item.item_id == id).map(|e| e.item.revision_id.clone());
        Some(st.ai_book.handle(request, &revision, st.controls.chapter_translations, !st.controls.no_ai_annotations))
    }

    /// `reader_passes_v1`, kept out of `handle`: that function's frame is already
    /// near the stack of a background dispatch thread.
    #[inline(never)]
    fn handle_passes(&self, command: &Command) -> Option<Response> {
        use crate::book::ReaderRequest as R;
        let Command::Reader { max_sensitivity, request } = command else { return None };
        if !matches!(request, R::PassesList { .. } | R::PassesStats { .. } | R::PassesEdit { .. } | R::PassesTimingStart { .. } | R::PassesTimingReport { .. }) { return None; }
        let err = |code: &str| Response { ok: false, reason_code: Some(code.into()), ..Default::default() };
        let mut state = self.state.lock().unwrap();
        if state.controls.legacy_agent || state.controls.no_reader_passes { return Some(err("invalid_request")); }
        if *max_sensitivity < Sensitivity::Private { return Some(err("permission_denied")); }
        let st = &mut *state;
        let entries = &st.entries;
        let text = |id: &str| entries.iter().find(|e| e.item.item_id == id).map(pass_text);
        st.passes.handle(request, &text)
    }

    /// `item_cover_v1`, kept out of `handle` (see `handle_passes`).
    #[inline(never)]
    fn handle_covers(&self, command: &Command) -> Option<Response> {
        if !matches!(command, Command::ItemCoverBatch { .. } | Command::ItemCoverChunk { .. }) { return None; }
        let mut state = self.state.lock().unwrap();
        if state.controls.legacy_agent || state.controls.no_item_cover {
            return Some(Response { ok: false, reason_code: Some("invalid_request".into()), ..Default::default() });
        }
        let st = &mut *state;
        let entries = &st.entries;
        let revision = |id: &str| entries.iter().find(|e| e.item.item_id == id).map(|e| e.item.revision_id.clone());
        let mut faults = crate::fixture_covers::CoverFaults { all_null: st.controls.cover_null, stale_once: st.controls.cover_stale_once, corrupt: st.controls.cover_corrupt };
        let out = st.covers.handle(command, &revision, &mut faults);
        st.controls.cover_stale_once = faults.stale_once;
        out
    }

    fn handle(&self, command: &Command) -> Response {
        if let Some(out) = self.handle_passes(command) { return out; }
        if let Some(out) = self.handle_covers(command) { return out; }
        if let Some(out) = self.handle_ai_book(command) { return out; }
        let mut state = self.state.lock().unwrap();
        let omit_minutes = state.controls.omit_ai_minutes;
        let fast_minutes = state.controls.fast_minutes;
        let manual_minutes = !state.controls.no_minutes_request && !state.controls.legacy_agent && !omit_minutes;
        let no_insights = state.controls.no_insights || state.controls.legacy_agent;
        let highlights_v1 = state.controls.highlights_v1;
        let ok = |r: Response| Response { ok: true, ..r };
        let err = |code: &str| Response { ok: false, reason_code: Some(code.into()), ..Default::default() };
        match command {
            Command::MarkdownEdit { edit, .. } => {
                let Some(index) = state.entries.iter().position(|e| e.item.item_id == edit.item_id) else { return err("library_resource_not_found") };
                if let Some((prior, receipt)) = state.markdown_requests.get(&edit.request_id) {
                    return if prior == edit { ok(Response { item_update: Some(receipt.clone()), ..Default::default() }) } else { err("request_id_conflict") };
                }
                let entry = &mut state.entries[index];
                if entry.item.revision_id != edit.expected_revision_id { return err("stale_item_revision") }
                if edit.markdown.trim().is_empty() { return err("invalid_markdown_edit") }
                let previous_revision_id = entry.item.revision_id.clone();
                entry.markdown = edit.markdown.clone();
                bump(entry);
                let receipt = ItemUpdate { item_id: edit.item_id.clone(), previous_revision_id: Some(previous_revision_id), revision_id: entry.item.revision_id.clone() };
                state.generation += 1;
                state.markdown_requests.insert(edit.request_id.clone(), (edit.clone(), receipt.clone()));
                ok(Response { item_update: Some(receipt), ..Default::default() })
            }
            Command::ReadingAnnotationsRead { item_id, revision_id, .. } => {
                let Some(entry) = find(&state.entries, item_id) else { return err("library_resource_not_found") };
                if &entry.item.revision_id != revision_id { return err("stale_item_revision") }
                let empty = crate::reading::ReadingAnnotations::empty(item_id.clone(), revision_id.clone(), &entry.markdown);
                let marks = state.reading_annotations.get(revision_id).cloned().unwrap_or(empty);
                ok(Response { reading_annotations: Some(marks), ..Default::default() })
            }
            Command::ReadingAnnotationsSave { expected_version, annotations, .. } => {
                let Some(entry) = find(&state.entries, &annotations.item_id) else { return err("library_resource_not_found") };
                if entry.item.revision_id != annotations.revision_id { return err("stale_item_revision") }
                let empty = crate::reading::ReadingAnnotations::empty(annotations.item_id.clone(), annotations.revision_id.clone(), &entry.markdown);
                let current = state.reading_annotations.get(&annotations.revision_id).unwrap_or(&empty);
                if current != annotations && &current.version() != expected_version { return err("stale_reading_annotations") }
                state.reading_annotations.insert(annotations.revision_id.clone(), annotations.clone());
                ok(Response { reading_annotations: Some(annotations.clone()), ..Default::default() })
            }
            Command::ScopedItemRead { item_id, .. } => {
                let Some(e) = find(&state.entries, item_id) else { return err("library_resource_not_found") };
                let mut end = e.markdown.len().min(MAX_CONTEXT_BYTES);
                while !e.markdown.is_char_boundary(end) { end -= 1; }
                ok(Response { library_entry: Some(crate::reading::LibraryEntry {
                    item_id: item_id.clone(), revision_id: e.item.revision_id.clone(), title: e.item.title.clone(), manual_tags: e.tags.clone(),
                    coordinate_space: "revision_markdown_utf8_v1".into(), content_sha256: format!("sha256:{:x}", Sha256::digest(e.markdown.as_bytes())),
                    total_bytes: e.markdown.len(), markdown: e.markdown[..end].into(), next_byte: (end < e.markdown.len()).then_some(end),
                }), ..Default::default() })
            }
            Command::ImageRead { .. } => err("image_resource_not_found"),
            // Answered by `handle_covers`.
            Command::ItemCoverBatch { .. } | Command::ItemCoverChunk { .. } => err("invalid_request"),
            Command::LibraryList { cursor } => {
                let start = match cursor {
                    None => 0,
                    Some(c) => match state.entries.iter().position(|e| &e.item.item_id == c) {
                        Some(p) => p + 1,
                        None => return err("invalid_cursor"),
                    },
                };
                let end = (start + PAGE).min(state.entries.len());
                let items: Vec<_> = state.entries[start..end].iter().map(|e| {
                    let (published_at, duration_seconds) = summary_dates(&e.biblio);
                    LibraryItem { published_at, duration_seconds, ..e.item.clone() }
                }).collect();
                let next = (end < state.entries.len()).then(|| items.last().unwrap().item_id.clone());
                ok(Response { library_items: Some(items), next_cursor: next, ..Default::default() })
            }
            Command::ItemRead { item_id } => match find(&state.entries, item_id) {
                Some(e) if e.markdown.len() > 240 * 1024 => err("result_too_large"),
                Some(e) => ok(Response {
                    item: Some(CanonicalItem {
                        item_id: e.item.item_id.clone(),
                        revision_id: e.item.revision_id.clone(),
                        title: e.item.title.clone(),
                        markdown: e.markdown.clone(),
                        manual_tags: Some(e.tags.clone()),
                    }),
                    ..Default::default()
                }),
                None => err("item_not_found"),
            },
            Command::LibraryItemRead { item_id } => match find_mut(&mut state.entries, item_id) {
                Some(e) => {
                    advance(&mut e.translation);
                    advance_minutes(&mut e.minutes, fast_minutes);
                    let ai_minutes = (!omit_minutes).then(|| minutes_wire(&e.minutes));
                    e.insights.advance();
                    let ai_insights = (!no_insights).then(|| e.insights.wire(highlights_v1));
                    let t = &e.translation;
                    ok(Response {
                        library_item: Some(TranslationItem {
                            item_id: e.item.item_id.clone(),
                            revision_id: e.item.revision_id.clone(),
                            summary_state: if e.summary.is_some() { AnalysisState::Succeeded } else { AnalysisState::NotRequested },
                            summary: e.summary.clone(),
                            translation_state: t.state,
                            title: t.title.clone(),
                            markdown: t.markdown.clone(),
                            // As the agent's projection: no reason here.
                            translation_reason: None,
                            ai_minutes,
                            ai_insights,
                            translated_title: translated_title(t),
                            translation_progress: progress(t),
                            title_source: title_source(t),
                            translation_failure_detail: None,
                            untranslated_blocks: None,
                        }),
                        ..Default::default()
                    })
                }
                None => err("library_resource_not_found"),
            },
            Command::ScopedTranslationStatus { item_id, max_sensitivity: _ } => {
                if state.controls.legacy_agent {
                    return err("invalid_request");
                }
                match find_mut(&mut state.entries, item_id) {
                    Some(e) => {
                        advance(&mut e.translation);
                        ok(Response { translation: Some(status(e)), ..Default::default() })
                    }
                    None => err("library_resource_not_found"),
                }
            }
            Command::MinutesRequest { item_id, max_sensitivity: _ } => {
                if !manual_minutes {
                    return err("invalid_request");
                }
                let no_provider = state.controls.no_minutes_provider;
                let Some(e) = find_mut(&mut state.entries, item_id) else { return err("library_resource_not_found") };
                if no_provider {
                    return err("no_minutes_provider");
                }
                advance_minutes(&mut e.minutes, fast_minutes);
                if matches!(e.minutes.state, MinutesState::NotRequested | MinutesState::Failed | MinutesState::Unavailable) {
                    e.minutes.state = MinutesState::Queued;
                    e.minutes.reason = None;
                    e.minutes.started = Some(Instant::now());
                }
                ok(Response { ai_minutes: Some(minutes_wire(&e.minutes)), ..Default::default() })
            }
            Command::TranslationRequest { item_id, max_sensitivity: _ } => {
                let no_provider = state.controls.no_translation_provider;
                let Some(e) = find_mut(&mut state.entries, item_id) else { return err("library_resource_not_found") };
                advance(&mut e.translation);
                let t = &mut e.translation;
                // An agent without `minutes_request_v1` makes the minutes in the same
                // request (Chinese sources too); a newer one leaves them alone.
                advance_minutes(&mut e.minutes, fast_minutes);
                if manual_minutes {
                    // Reported as they are, never started here.
                } else if !no_provider && matches!(e.minutes.state, MinutesState::NotRequested | MinutesState::Failed) {
                    e.minutes.state = MinutesState::Queued;
                    e.minutes.reason = None;
                    e.minutes.started = Some(Instant::now());
                }
                let minutes = (!omit_minutes).then(|| minutes_wire(&e.minutes));
                let answer = |state: &str, reason: &str| ok(Response { translation: Some(TranslationStatus { state: state.into(), reason: Some(reason.into()), revision_id: Some(e.item.revision_id.clone()), ai_minutes: None, translated_title: None, progress: None, failure_detail: None, untranslated_blocks: None }), ai_minutes: minutes.clone(), ..Default::default() });
                // As the agent: the language gate first, then the provider check.
                if let Some(reason) = t.skip {
                    return answer("not_requested", reason);
                }
                if matches!(t.state, AnalysisState::NotRequested | AnalysisState::Failed | AnalysisState::Unavailable) {
                    if no_provider {
                        return answer("unavailable", "no_translation_provider");
                    }
                    t.state = AnalysisState::Queued;
                    t.reason = None;
                    t.started = Some(Instant::now());
                    // A failed run resumes from its stored batches.
                    t.resume = t.batches.0;
                }
                ok(Response { translation: Some(status(e)), ai_minutes: minutes, ..Default::default() })
            }
            Command::GroupTreeRead { max_sensitivity: _, offset, limit, expected_view } => {
                if *limit == 0 || *limit > 200 || (*offset > 0 && expected_view.is_none()) {
                    return err("invalid_group_window");
                }
                let page = tree_page(&state, *offset, *limit);
                if expected_view.as_ref().is_some_and(|v| v != &page.view) {
                    return err("stale_group_view");
                }
                if *offset > page.total {
                    return err("invalid_group_window");
                }
                ok(Response { group_tree: Some(page), ..Default::default() })
            }
            Command::GroupEdit { max_sensitivity: _, request_id, expected_version, ops } => {
                let body = (expected_version.clone(), ops.clone());
                if let Some(previous) = state.group_requests.get(request_id) {
                    // A retry of a request that already applied gets the same answer.
                    let transfer = matches!(ops.as_slice(), [GroupEditOp::TransferFolders { .. }]);
                    return if *previous == body { ok(Response { group_tree: (!transfer).then(|| tree_page(&state, 0, 200)), group_edit: state.folder_receipts.get(request_id).cloned().or_else(|| receipt(&state, &ops[0])), ..Default::default() }) } else { err("request_id_conflict") };
                }
                // Single-op writes, as the agent: a batch would not be atomic.
                let [op] = ops.as_slice() else { return err("invalid_group_edit") };
                let target = match op {
                    GroupEditOp::CreateRoot { .. } => None,
                    GroupEditOp::ReorderRoot { group_id, .. } => Some(group_id.clone()),
                    GroupEditOp::TransferFolders { target_parent_id, .. } => Some(target_parent_id.clone()),
                    GroupEditOp::CreateFolder { parent_id: id, .. } | GroupEditOp::Rename { id, .. } | GroupEditOp::AddMembers { group_id: id, .. } | GroupEditOp::RemoveMembers { group_id: id, .. } | GroupEditOp::MoveMembers { target_group_id: id, .. } => Some(id.clone()),
                };
                match (&target, expected_version) {
                    (None, None) => {}
                    (Some(id), Some(v)) => match state.folders.iter().find(|f| &f.id == id) {
                        None => return err("library_resource_not_found"),
                        Some(f) if &format!("v{}", f.version) != v => return err("stale_group_version"),
                        Some(_) => {}
                    },
                    _ => return err("invalid_group_edit"),
                }
                let next = state.next_group;
                let bump = |state: &mut State, id: &str| {
                    if let Some(f) = state.folders.iter_mut().find(|f| f.id == id) {
                        f.version += 1;
                    }
                };
                match op {
                    GroupEditOp::ReorderRoot { group_id, before_id } => {
                        let index = state.folders.iter().position(|f| &f.id == group_id).unwrap();
                        let main = |f: &Folder| f.parent.is_none() && f.kind == "user" && f.name.eq_ignore_ascii_case("Main");
                        if state.folders[index].parent.is_some() || main(&state.folders[index]) || before_id.as_ref() == Some(group_id) {
                            return err("invalid_group_edit");
                        }
                        if let Some(id) = before_id {
                            if !state.folders.iter().any(|f| &f.id == id && f.parent.is_none() && !main(f)) {
                                return err("invalid_group_edit");
                            }
                        }
                        let mut folder = state.folders.remove(index);
                        folder.version += 1;
                        let at = before_id.as_ref().and_then(|id| state.folders.iter().position(|f| &f.id == id)).unwrap_or(state.folders.len());
                        state.folders.insert(at, folder);
                    }
                    GroupEditOp::CreateRoot { name } | GroupEditOp::CreateFolder { name, .. } | GroupEditOp::Rename { name, .. } if !valid_group_name(name) => return err("invalid_group_edit"),
                    GroupEditOp::CreateRoot { name } => {
                        state.folders.push(Folder { id: format!("grp_new_{next}"), name: name.trim().into(), kind: "user".into(), parent: None, version: 1 });
                        state.next_group += 1;
                    }
                    GroupEditOp::CreateFolder { parent_id, name } => {
                        state.folders.push(Folder { id: format!("grp_new_{next}"), name: name.trim().into(), kind: "collection".into(), parent: Some(parent_id.clone()), version: 1 });
                        state.next_group += 1;
                        bump(&mut state, parent_id);
                    }
                    GroupEditOp::Rename { id, name } => {
                        if state.folders.iter().any(|f| &f.id == id && f.name == name.trim()) {
                            return err("groups_unchanged");
                        }
                        if let Some(f) = state.folders.iter_mut().find(|f| &f.id == id) {
                            f.name = name.trim().into();
                        }
                        bump(&mut state, id);
                    }
                    GroupEditOp::TransferFolders { source_ids, target_parent_id, expected_versions, moving } => {
                        if let Err(code) = transfer_folders(&mut state, source_ids, target_parent_id, expected_versions, *moving) { return err(code); }
                    }
                    GroupEditOp::MoveMembers { .. } if !state.controls.move_members_op => return err("invalid_group_edit"),
                    GroupEditOp::MoveMembers { source_group_id, target_group_id, expected_source_version, item_ids } => {
                        let folder = |id: &str| state.folders.iter().find(|f| f.id == id).cloned();
                        let (Some(source), Some(target_folder)) = (folder(source_group_id), folder(target_group_id)) else { return err("library_resource_not_found") };
                        let mut distinct = item_ids.clone();
                        distinct.sort();
                        distinct.dedup();
                        if source.id == target_folder.id || source.parent.is_none() || target_folder.parent.is_none() || root_of(&state.folders, &source.id) != root_of(&state.folders, &target_folder.id)
                            || distinct.is_empty() || distinct.len() > crate::MAX_GROUP_EDIT_ITEMS {
                            return err("invalid_group_edit");
                        }
                        if &format!("v{}", source.version) != expected_source_version {
                            return err("stale_group_version");
                        }
                        if !distinct.iter().all(|id| find(&state.entries, id).is_some_and(|e| e.groups.contains(source_group_id))) {
                            return err("stale_group_membership");
                        }
                        for e in state.entries.iter_mut().filter(|e| distinct.contains(&e.item.item_id)) {
                            e.groups.retain(|g| g != source_group_id);
                            if !e.groups.contains(target_group_id) {
                                e.groups.push(target_group_id.clone());
                            }
                        }
                        bump(&mut state, source_group_id);
                        bump(&mut state, target_group_id);
                    }
                    GroupEditOp::AddMembers { group_id, item_ids } | GroupEditOp::RemoveMembers { group_id, item_ids } => {
                        let mut distinct = item_ids.clone();
                        distinct.sort();
                        distinct.dedup();
                        if distinct.is_empty() || distinct.len() > crate::MAX_GROUP_EDIT_ITEMS {
                            return err("invalid_group_edit");
                        }
                        let fail = if matches!(op, GroupEditOp::AddMembers { .. }) { state.controls.fail_next_add_members.take() } else { state.controls.fail_next_remove_members.take() };
                        if let Some(code) = fail {
                            return err(&code);
                        }
                        if item_ids.iter().any(|id| find(&state.entries, id).is_none()) {
                            return err("library_resource_not_found");
                        }
                        let add = matches!(op, GroupEditOp::AddMembers { .. });
                        let mut changed = false;
                        for e in state.entries.iter_mut().filter(|e| item_ids.contains(&e.item.item_id)) {
                            let had = e.groups.contains(group_id);
                            if add && !had {
                                e.groups.push(group_id.clone());
                                changed = true;
                            } else if !add && had {
                                e.groups.retain(|g| g != group_id);
                                changed = true;
                            }
                        }
                        if !changed {
                            return err("groups_unchanged");
                        }
                        bump(&mut state, group_id);
                    }
                }
                state.groups_version += 1;
                state.generation += 1;
                state.group_requests.insert(request_id.clone(), body);
                if matches!(op, GroupEditOp::TransferFolders { .. }) {
                    let receipt = receipt(&state, op).unwrap();
                    state.folder_receipts.insert(request_id.clone(), receipt);
                }
                ok(Response { group_tree: (!matches!(op, GroupEditOp::TransferFolders { .. })).then(|| tree_page(&state, 0, 200)), group_edit: receipt(&state, op), ..Default::default() })
            }
            Command::UpdateItemMetadata { item_id, expected_revision_id, title, manual_tags } => {
                let Some(entry) = state.entries.iter_mut().find(|e| &e.item.item_id == item_id) else {
                    return err("item_not_found");
                };
                if &entry.item.revision_id != expected_revision_id {
                    return err("stale_item_revision");
                }
                let title = title.trim();
                if title.is_empty() || title.len() > 1024 {
                    return err("invalid_metadata_update");
                }
                let mut tags: Vec<String> = manual_tags.iter().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
                tags.sort();
                tags.dedup();
                let mut current = entry.tags.clone();
                current.sort();
                if title == entry.item.title && tags == current {
                    return err("metadata_unchanged");
                }
                let previous = entry.item.revision_id.clone();
                entry.item.title = title.into();
                entry.tags = tags;
                bump(entry);
                let revision_id = entry.item.revision_id.clone();
                state.generation += 1;
                ok(Response {
                    item_update: Some(ItemUpdate {
                        item_id: item_id.clone(),
                        revision_id,
                        previous_revision_id: Some(previous),
                    }),
                    ..Default::default()
                })
            }
            Command::LibraryQuery { query, max_sensitivity: _, offset, limit, expected_view } => {
                if state.controls.legacy_agent || (state.controls.no_added_at_sort && query.sort == SortKey::AddedAt)
                    || (state.controls.no_origin_filter && query.origin.is_some()) || (state.controls.no_unfiled_query && query.unfiled) {
                    return err("invalid_request");
                }
                if *limit == 0 || *limit > MAX_QUERY_LIMIT {
                    return err("invalid_query_window");
                }
                let view = view_id(state.generation, query);
                match expected_view {
                    None if *offset != 0 => return err("invalid_query_window"),
                    Some(v) if v != &view => return err("stale_query_view"),
                    _ => {}
                }
                let scope: Option<Vec<String>> = match &query.group {
                    None => None,
                    Some(g) if !state.folders.iter().any(|f| f.id == g.group_id) => return err("group_not_found"),
                    Some(g) if g.include_descendants => Some(subtree(&state.folders, &g.group_id)),
                    Some(g) => Some(vec![g.group_id.clone()]),
                };
                let needle = query.text.to_lowercase();
                let mut rows: Vec<&Entry> = state
                    .entries
                    .iter()
                    .filter(|e| needle.is_empty() || e.item.title.to_lowercase().contains(&needle) || e.markdown.to_lowercase().contains(&needle))
                    .filter(|e| query.source.as_ref().is_none_or(|s| origin(e).as_deref() == Some(s.as_str())))
                    .filter(|e| query.item_type.as_ref().is_none_or(|t| &e.item.item_type == t))
                    .filter(|e| scope.as_ref().is_none_or(|nodes| in_scope(e, nodes)))
                    .filter(|e| query.origin.is_none_or(|o| e.creation_origin == o))
                    // Inbox: in no folder that still exists.
                    .filter(|e| !query.unfiled || !e.groups.iter().any(|g| state.folders.iter().any(|f| &f.id == g)))
                    .collect();
                rows.sort_by(|a, b| {
                    let key = |e: &Entry| -> Option<String> {
                        match query.sort {
                            SortKey::Title => Some(e.item.title.clone()),
                            SortKey::Source => origin(e),
                            SortKey::CapturedAt => e.item.captured_at.clone(),
                            SortKey::ItemType => Some(e.item.item_type.clone()),
                            SortKey::AddedAt => e.biblio.added_at.clone(),
                        }
                    };
                    let order = match (key(a), key(b)) {
                        (Some(x), Some(y)) => if query.descending { y.cmp(&x) } else { x.cmp(&y) },
                        (None, Some(_)) => std::cmp::Ordering::Greater,
                        (Some(_), None) => std::cmp::Ordering::Less,
                        (None, None) => std::cmp::Ordering::Equal,
                    };
                    order.then_with(|| a.item.item_id.cmp(&b.item.item_id))
                });
                let facet = |f: &dyn Fn(&Entry) -> Option<String>| {
                    let mut map = std::collections::BTreeMap::<String, usize>::new();
                    for e in &rows {
                        if let Some(v) = f(e) {
                            *map.entry(v).or_default() += 1;
                        }
                    }
                    map.into_iter().map(|(value, count)| Facet { value, count }).collect::<Vec<_>>()
                };
                let sources = facet(&origin);
                let item_types = facet(&|e| Some(e.item.item_type.clone()));
                let total = rows.len();
                let end = (*offset + *limit).min(total);
                let items = rows.get(*offset..end).unwrap_or_default().iter().map(|e| QueryItem {
                    item_id: e.item.item_id.clone(),
                    revision_id: e.item.revision_id.clone(),
                    title: e.item.title.clone(),
                    source: origin(e),
                    item_type: e.item.item_type.clone(),
                    captured_at: e.item.captured_at.clone(),
                    added_at: e.biblio.added_at.clone(),
                    translated_title: translated_title(&e.translation),
                    title_source: title_source(&e.translation),
                    published_at: summary_dates(&e.biblio).0,
                    duration_seconds: e.biblio.duration_seconds,
                    bibliographic: Some(e.biblio.clone()),
                }).collect();
                ok(Response {
                    library_query: Some(QueryWindow {
                        schema_version: 1,
                        generation: format!("gen_{}", state.generation),
                        view,
                        total,
                        offset: *offset,
                        next_offset: (end < total).then_some(end),
                        items,
                        sources,
                        item_types,
                    }),
                    ..Default::default()
                })
            }
            Command::List { .. } => ok(Response {
                jobs: Some(vec![
                    Job { job_id: "job_fixture_translate".into(), state: "leased".into(), stage: "translation".into(), updated_at: "2026-11-01T09:00:00Z".into(), reason_code: None, item_id: Some("item_fixture_000001".into()) },
                    Job { job_id: "job_fixture_summary".into(), state: "needs_user".into(), stage: "summary".into(), updated_at: "2026-11-01T08:55:00Z".into(), reason_code: None, item_id: Some("item_fixture_000002".into()) },
                ]),
                ..Default::default()
            }),
            Command::Retry { .. } | Command::Cancel { .. } | Command::Approve { .. } => ok(Response::default()),
            Command::ModelSettingsRead
            | Command::ModelSettingsReplace { .. }
            | Command::ProviderCredentialRead { .. }
            | Command::ProviderCredentialReplace { .. }
            | Command::ProviderCredentialClear { .. }
            | Command::ProviderModelsDiscover { .. } => {
                if state.controls.legacy_agent {
                    return err("internal_error");
                }
                state.models.handle(command)
            }
            Command::Reader { max_sensitivity, request } => {
                if state.controls.legacy_agent {
                    return err("invalid_request");
                }
                {
                    let st = &mut *state;
                    let book_toc = matches!(request, crate::book::ReaderRequest::Toc { item_id, .. } if item_id == crate::fixture_book::BOOK_ITEM);
                    let entries = &st.entries;
                    let lookup = |id: &str| entries.iter().find(|e| e.item.item_id == id).map(|e| (e.item.revision_id.clone(), e.markdown.clone()));
                    if !book_toc && *max_sensitivity >= Sensitivity::Private {
                        if let Some(out) = st.reader.handle(request, &lookup, !st.controls.no_reader_annotations) { return out; }
                    }
                }
                let aligned = !state.controls.no_book_alignment;
                if !aligned && !matches!(request, crate::book::ReaderRequest::Toc { .. } | crate::book::ReaderRequest::Chunk { .. }) {
                    return err("invalid_request");
                }
                let st = &mut *state;
                let Some(entry) = st.entries.iter_mut().find(|e| e.item.item_id == crate::fixture_book::BOOK_ITEM) else { return err("library_resource_not_found") };
                let mut next = entry.clone();
                bump(&mut next);
                let out = crate::fixture_book::handle_with(&mut st.book, request, *max_sensitivity, &entry.item.revision_id, &next.item.revision_id, &entry.markdown);
                if out.bump { bump(entry); st.generation += 1; }
                out.response
            }
            Command::AgentVersion => {
                if state.controls.legacy_agent || state.controls.no_agent_version {
                    return err("invalid_request");
                }
                // Clearly synthetic; the hash is `null` while "hashing" (FixtureControls).
                let binary_sha256 = (!state.controls.agent_hashing).then(|| "f1x7ure0c0de".to_string());
                ok(Response { agent_version: Some(AgentVersion { version: "0.0.0-synthetic".into(), binary_sha256, query_origin: !state.controls.no_origin_filter }), ..Default::default() })
            }
            Command::Capabilities => {
                if state.controls.legacy_agent {
                    return err("invalid_request");
                }
                let mut caps = fb::capabilities();
                caps["features"].as_array_mut().unwrap().extend(["markdown_edit_v1".into(), "reading_annotations_v1".into(), crate::book::READER_FEATURE.into()]);
                if !state.controls.no_reader_passes {
                    caps["features"].as_array_mut().unwrap().push(crate::passes::PASSES_FEATURE.into());
                }
                if !state.controls.no_reader_annotations {
                    caps["features"].as_array_mut().unwrap().push(crate::reader_state::ANNOTATIONS_FEATURE.into());
                }
                if !state.controls.no_ai_annotations {
                    caps["features"].as_array_mut().unwrap().push(crate::ai_marks::AI_ANNOTATIONS_FEATURE.into());
                }
                if state.controls.chapter_translations {
                    caps["features"].as_array_mut().unwrap().extend([crate::book::TRANSLATION_FEATURE.into(), crate::book::TRANSLATION_METADATA_FEATURE.into(), crate::book::TRANSLATION_ALIGNMENT_FEATURE.into()]);
                }
                if !state.controls.no_book_alignment {
                    caps["features"].as_array_mut().unwrap().extend([crate::book::ALIGNMENT_FEATURE.into(), crate::book::REPAIR_FEATURE.into()]);
                }
                if !state.controls.no_item_chat {
                    caps["features"].as_array_mut().map(|f| f.push(crate::chat::ITEM_CHAT_FEATURE.into()));
                    if !state.controls.no_chat_history {
                        caps["features"].as_array_mut().map(|f| f.push(crate::chat::ITEM_CHAT_HISTORY_FEATURE.into()));
                    }
                }
                if manual_minutes {
                    caps["features"].as_array_mut().map(|f| f.push(crate::dto::MINUTES_REQUEST_FEATURE.into()));
                }
                if !state.controls.no_trash {
                    caps["features"].as_array_mut().map(|f| f.push(crate::trash::TRASH_FEATURE.into()));
                }
                if !state.controls.no_item_cover {
                    caps["features"].as_array_mut().map(|f| f.push(crate::covers::COVER_FEATURE.into()));
                }
                if !state.controls.no_active_group {
                    caps["features"].as_array_mut().map(|f| f.push(crate::active_group::ACTIVE_GROUP_FEATURE.into()));
                }
                if !state.controls.no_capture_jobs {
                    caps["features"].as_array_mut().map(|f| f.push(crate::capture_jobs::CAPTURE_JOBS_FEATURE.into()));
                }
                if !state.controls.no_insights {
                    caps["features"].as_array_mut().map(|f| f.push(crate::insights::INSIGHTS_FEATURE.into()));
                    caps["ai_insights_v1"] = serde_json::json!({"highlights": true, "quotes": true, "fact_check": true, "basis": "model_knowledge"});
                }
                let mut write_ops = vec!["reorder_root"];
                if state.controls.move_members_op { write_ops.push("move_members"); }
                if state.controls.transfer_folders_op { write_ops.push("transfer_folders"); }
                caps["groups_v1"] = serde_json::json!({"tree_page_limit": 200, "query_group_filter": true, "query_group_facets": false, "writes": true, "moves": false, "write_ops": write_ops, "query_unfiled": !state.controls.no_unfiled_query, "node_delete": !state.controls.no_node_delete, "trash": state.controls.group_trash});
                caps["translation_provider_v1"] = serde_json::json!({
                    "available": !state.controls.no_translation_provider,
                    "minutes": true,
                    "metadata": true,
                    "purpose_routes": {
                        "translation": if state.controls.no_translation_provider { serde_json::Value::Null } else { "fixture-model-alpha".into() },
                        "summary": serde_json::Value::Null,
                        "chat": "fixture-model-alpha"
                    }
                });
                ok(Response { capabilities: serde_json::from_value(caps).ok(), ..Default::default() })
            }
            Command::BibliographicMetadataRead { .. }
            | Command::BibliographicMetadataEdit { .. }
            | Command::BibliographicDuplicateCheck { .. }
            | Command::BibliographicLookupPreview { .. }
                if state.controls.legacy_agent =>
            {
                err("invalid_request")
            }
            Command::BibliographicMetadataRead { item_id, max_sensitivity: _ } => match find(&state.entries, item_id) {
                Some(e) => {
                    let value = fb::read(&e.biblio, &e.item.item_id, &e.item.revision_id, &e.item.title, e.item.captured_at.as_deref());
                    match serde_json::from_value(value) {
                        Ok(read) => ok(Response { bibliographic_metadata: Some(read), ..Default::default() }),
                        Err(_) => err("internal_error"),
                    }
                }
                None => err("library_resource_not_found"),
            },
            Command::BibliographicMetadataEdit { max_sensitivity: _, edit } => {
                let State { entries, biblio, generation, .. } = &mut *state;
                let Some(entry) = entries.iter_mut().find(|e| e.item.item_id == edit.item_id) else {
                    return err("library_resource_not_found");
                };
                match fb::edit(biblio, &entry.biblio, &entry.item.title, &entry.item.revision_id, edit) {
                    Err(code) => err(&code),
                    Ok(fb::EditOutcome::Replay(receipt)) => ok(Response { item_update: Some(receipt), ..Default::default() }),
                    Ok(fb::EditOutcome::Changed { biblio: next, title }) => {
                        let previous = entry.item.revision_id.clone();
                        entry.biblio = next;
                        entry.item.title = title;
                        bump(entry);
                        *generation += 1;
                        let receipt = ItemUpdate { item_id: edit.item_id.clone(), revision_id: entry.item.revision_id.clone(), previous_revision_id: Some(previous) };
                        fb::record(biblio, edit, &receipt);
                        ok(Response { item_update: Some(receipt), ..Default::default() })
                    }
                }
            }
            Command::BibliographicDuplicateCheck { identifiers, exclude_item_id, max_sensitivity: _ } => {
                let mut keys = vec![];
                for id in identifiers {
                    match fb::comparison_key(&id.kind, &id.value) {
                        Ok(Some(key)) => keys.push((id.kind.clone(), key)),
                        Ok(None) => {}
                        Err(code) => return err(&code),
                    }
                }
                let mut found = vec![];
                for e in state.entries.iter().filter(|e| Some(&e.item.item_id) != exclude_item_id.as_ref()) {
                    for (kind, key) in &keys {
                        if e.biblio.identifiers.iter().any(|i| &i.kind == kind && fb::comparison_key(kind, &i.value).ok().flatten().as_ref() == Some(key)) {
                            found.push(crate::biblio::IdentifierDuplicate { item_id: e.item.item_id.clone(), revision_id: e.item.revision_id.clone(), kind: kind.clone(), key: key.clone() });
                        }
                    }
                }
                ok(Response { identifier_duplicates: Some(found), ..Default::default() })
            }
            Command::BibliographicLookupPreview { item_id, expected_revision_id, identifier, max_sensitivity: _ } => {
                if let Some(code) = state.controls.lookup_error.clone() {
                    return err(&code);
                }
                let State { entries, biblio, .. } = &mut *state;
                let Some((seed, e)) = entries.iter().enumerate().find(|(_, e)| &e.item.item_id == item_id) else {
                    return err("library_resource_not_found");
                };
                if &e.item.revision_id != expected_revision_id {
                    return err("stale_item_revision");
                }
                match fb::lookup(biblio, &e.biblio, &e.item.title, item_id, expected_revision_id, &identifier.kind, &identifier.value, seed) {
                    Ok(preview) => ok(Response { lookup_preview: Some(preview), ..Default::default() }),
                    Err(code) => err(&code),
                }
            }
            Command::GroupDeletePreview { .. } | Command::GroupDelete { .. } | Command::GroupDeleteList { .. } | Command::GroupDeleteRestore { .. } => {
                if state.controls.no_node_delete {
                    return err("invalid_request");
                }
                group_delete(&mut state, command)
            }
            Command::CaptureJobsRecent { limit, .. } => {
                if state.controls.no_capture_jobs || state.controls.legacy_agent {
                    return err("invalid_request");
                }
                state.capture_job_calls += 1;
                let counts = crate::capture_jobs::CaptureCounts::of(&state.capture_jobs);
                let jobs = state.capture_jobs.iter().take((*limit).clamp(1, 100) as usize).cloned().collect();
                ok(Response { capture_jobs: Some(crate::capture_jobs::CaptureJobsRecent { schema_version: crate::capture_jobs::CAPTURE_JOBS_SCHEMA, jobs, counts }), ..Default::default() })
            }
            Command::UiActiveGroupSet { request_id, group_id, .. } => {
                if state.controls.no_active_group {
                    return err("invalid_request");
                }
                state.active_group_calls.push((request_id.clone(), group_id.clone()));
                match state.active_group_requests.get(request_id) {
                    // A replay acknowledges again without becoming the selection again.
                    Some(prior) if prior != group_id => return err("request_identity_conflict"),
                    Some(_) => {}
                    None => {
                        if group_id.as_ref().is_some_and(|id| !state.folders.iter().any(|f| &f.id == id)) {
                            return err("library_resource_not_found");
                        }
                        state.active_group_requests.insert(request_id.clone(), group_id.clone());
                        state.active_group = group_id.clone();
                    }
                }
                let ack = crate::active_group::ActiveGroupAck { schema_version: crate::active_group::ACTIVE_GROUP_SCHEMA, request_id: request_id.clone(), group_id: group_id.clone(), updated_at: "2026-11-08T12:00:00Z".into() };
                ok(Response { ui_active_group: Some(ack), ..Default::default() })
            }
            Command::GroupTrashList { group_id, cursor, .. } => {
                if !state.controls.group_trash || state.controls.legacy_agent { return err("invalid_request"); }
                let mut items: Vec<_> = state.group_trash.iter().filter(|((root, _), _)| root == group_id).map(|(_, (entry, _))| entry.clone()).collect();
                items.sort_by(|a, b| a.item_id.cmp(&b.item_id));
                let start = match cursor {
                    None => 0,
                    Some(cursor) => match items.iter().position(|e| &e.item_id == cursor) {
                        Some(i) => i + 1,
                        None => return err("invalid_cursor"),
                    },
                };
                let end = (start + PAGE).min(items.len());
                let next_cursor = (end < items.len()).then(|| items[end - 1].item_id.clone());
                ok(Response { trash_items: Some(items[start..end].to_vec()), next_cursor, ..Default::default() })
            }
            Command::GroupTrashEdit { group_id, item_id, expected_revision_id, request_id, restore_plan, .. } => {
                if !state.controls.group_trash || state.controls.legacy_agent { return err("invalid_request"); }
                let identity = (group_id.clone(), item_id.clone(), expected_revision_id.clone(), restore_plan.clone());
                if let Some(old) = state.group_trash_requests.get(request_id) {
                    return if old == &identity { ok(Response::default()) } else { err("request_identity_conflict") };
                }
                if !state.folders.iter().any(|g| &g.id == group_id && g.parent.is_none()) { return err("group_not_found"); }
                let Some(ix) = state.entries.iter().position(|e| &e.item.item_id == item_id) else { return err("library_resource_not_found"); };
                if state.entries[ix].item.revision_id != *expected_revision_id { return err("stale_item_revision"); }
                let key = (group_id.clone(), item_id.clone());
                if let Some(plan) = restore_plan {
                    let Some((trash, members)) = state.group_trash.get(&key) else { return err("item_not_trashed"); };
                    if trash.plan_id.as_ref() != Some(plan) { return err("invalid_restore_plan"); }
                    let members = members.clone();
                    for member in members {
                        if state.folders.iter().any(|f| f.id == member) && !state.entries[ix].groups.contains(&member) {
                            state.entries[ix].groups.push(member);
                        }
                    }
                    state.group_trash.remove(&key);
                } else {
                    if state.group_trash.contains_key(&key) { return err("item_already_trashed"); }
                    let members: Vec<_> = state.entries[ix].groups.iter().filter(|g| root_of(&state.folders, g) == *group_id).cloned().collect();
                    if members.is_empty() { return err("item_not_in_group"); }
                    let e = &state.entries[ix];
                    let trash = crate::trash::TrashEntry { item_id: item_id.clone(), revision_id: expected_revision_id.clone(), title: e.item.title.clone(), source_url: e.biblio.source_url.clone().unwrap_or_default(),
                        trashed_at: "2026-11-11T08:00:00Z".into(), reason: "manual".into(), kept_item_id: None, plan_id: Some(format!("fixture-trash-{request_id}")) };
                    state.entries[ix].groups.retain(|g| !members.contains(g));
                    state.group_trash.insert(key, (trash, members));
                }
                state.group_trash_requests.insert(request_id.clone(), identity);
                state.generation += 1;
                state.groups_version += 1;
                ok(Response::default())
            }
            Command::TrashList { cursor } => {
                if state.controls.legacy_agent || state.controls.no_trash {
                    return err("invalid_request");
                }
                let mut items: Vec<_> = state.trash.iter().map(|(_, e, t)| trash_wire(e, t)).collect();
                items.sort_by(|a, b| a.item_id.cmp(&b.item_id));
                let start = match cursor {
                    None => 0,
                    Some(c) => match items.iter().position(|i| &i.item_id == c) {
                        Some(p) => p + 1,
                        None => return err("invalid_cursor"),
                    },
                };
                let end = (start + 50).min(items.len());
                let page = items[start..end].to_vec();
                let next = (end < items.len()).then(|| page.last().map(|i| i.item_id.clone())).flatten();
                ok(Response { trash_items: Some(page), next_cursor: next, ..Default::default() })
            }
            Command::ItemTrashRestore { item_id } => {
                if state.controls.legacy_agent || state.controls.no_trash {
                    return err("invalid_request");
                }
                let Some(k) = state.trash.iter().position(|(_, e, _)| &e.item.item_id == item_id) else {
                    return if find(&state.entries, item_id).is_some() { err("item_not_trashed") } else { err("library_resource_not_found") };
                };
                let (at, entry, _) = state.trash.remove(k);
                let revision_id = entry.item.revision_id.clone();
                let at = at.min(state.entries.len());
                state.entries.insert(at, entry);
                state.generation += 1;
                ok(Response { trash_restore: Some(crate::trash::TrashRestore { item_id: item_id.clone(), revision_id, restored: true }), ..Default::default() })
            }
            Command::InsightRequest { item_id, max_sensitivity: _, kinds } => {
                if no_insights {
                    return err("invalid_request");
                }
                if kinds.is_empty() {
                    return err("invalid_insight_request");
                }
                let no_provider = state.controls.no_insight_provider;
                let Some(e) = find_mut(&mut state.entries, item_id) else { return err("library_resource_not_found") };
                if no_provider {
                    return err("no_insight_provider");
                }
                e.insights.advance();
                let translated = e.translation.state == AnalysisState::Succeeded;
                e.insights.request(kinds, translated);
                ok(Response { ai_insights: Some(e.insights.wire(highlights_v1)), ..Default::default() })
            }
            Command::ItemChat { item_id, max_sensitivity: _, request_id, messages } => {
                if state.controls.legacy_agent || state.controls.no_item_chat {
                    return err("invalid_request");
                }
                let Some(e) = find(&state.entries, item_id) else { return err("library_resource_not_found") };
                let id_ok = (16..=128).contains(&request_id.len()) && request_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
                if !id_ok || !crate::chat::valid_messages(messages) {
                    return err("invalid_chat_request");
                }
                let title = e.item.title.clone();
                let question = messages.last().map(|m| m.content.trim().to_string()).unwrap_or_default();
                let replay = match state.chat_requests.get(request_id) {
                    Some((item, asked, _)) if item != item_id || *asked != question => return err("request_identity_conflict"),
                    Some((_, _, Some(reply))) => {
                        let reply = reply.clone();
                        return ok(Response { item_chat: Some(crate::chat::ItemChatReply { request_id: request_id.clone(), reply, model: Some("fixture-model-alpha".into()), finish: "stop".into() }), ..Default::default() });
                    }
                    Some(_) => true,
                    None => false,
                };
                if item_id == CHAT_NO_PROVIDER_ITEM {
                    return err("no_chat_provider");
                }
                state.chat_requests.insert(request_id.clone(), (item_id.clone(), question.clone(), None));
                if item_id == CHAT_UNKNOWN_ITEM && !replay {
                    return err("provider_outcome_unknown");
                }
                let turns = messages.iter().filter(|m| m.role == crate::chat::ChatRole::User).count();
                let reply = format!(
                    "（合成回复，非模型生成）你问的是：**{question}**\n\n这是开发模式下的固定回答，用来检验对话排版：\n\n- 条目：《{title}》\n- 本次对话共 {turns} 个提问\n\n真实回答需要连接代理并配置对话模型。"
                );
                // Long questions get a long synthetic answer, to exercise wrapping and scrolling.
                let reply = if question.chars().count() > 60 { format!("{reply}{LONG_CHAT_TAIL}") } else { reply };
                let reply = if question.contains("Markdown") { format!("{reply}{MARKDOWN_CHAT_TAIL}") } else { reply };
                // The agent stores each answered exchange once: the question and its answer.
                if let Some(entry) = state.chat_requests.get_mut(request_id) {
                    entry.2 = Some(reply.clone());
                }
                if !state.controls.no_chat_history {
                    let stored = state.chat_history.entry(item_id.clone()).or_default();
                    let seq = stored.last().map_or(1, |t| t.seq.max(0) + 1);
                    let turn = |seq: i64, role, content: &str, model: Option<&str>| crate::chat::StoredTurn {
                        seq,
                        role,
                        content: content.into(),
                        request_id: request_id.clone(),
                        created_at_ms: 1_793_000_000_000 + seq,
                        model: model.map(str::to_string),
                        origin: None,
                        truncated: false,
                    };
                    stored.push(turn(seq, crate::chat::ChatRole::User, &question, None));
                    stored.push(turn(seq + 1, crate::chat::ChatRole::Assistant, &reply, Some("fixture-model-alpha")));
                }
                ok(Response { item_chat: Some(crate::chat::ItemChatReply { request_id: request_id.clone(), reply, model: Some("fixture-model-alpha".into()), finish: "stop".into() }), ..Default::default() })
            }
            Command::ConversationRead { item_id, max_sensitivity: _, cursor } => {
                if state.controls.legacy_agent || state.controls.no_item_chat || state.controls.no_chat_history {
                    return err("invalid_request");
                }
                if find(&state.entries, item_id).is_none() {
                    return err("library_resource_not_found");
                }
                if item_id == CHAT_HISTORY_FAIL_ITEM {
                    return err("internal_error");
                }
                let after = match cursor.as_deref().map(str::parse::<i64>) {
                    None => i64::MIN,
                    Some(Ok(seq)) => seq,
                    Some(Err(_)) => return err("invalid_conversation_request"),
                };
                let all = state.chat_history.get(item_id).cloned().unwrap_or_default();
                let version = format!("v1:{}:{}", all.len(), all.iter().map(|t| t.seq).max().unwrap_or(0));
                let rest: Vec<_> = all.into_iter().filter(|t| t.seq > after).collect();
                // Small pages (the agent's are 200 turns / 48 KiB), so paging is exercised.
                let more = rest.len() > HISTORY_PAGE_TURNS;
                let turns: Vec<_> = rest.into_iter().take(HISTORY_PAGE_TURNS).collect();
                let next_cursor = more.then(|| turns.last().map(|t| t.seq.to_string())).flatten();
                ok(Response { conversation: Some(crate::chat::ConversationPage { item_id: item_id.clone(), turns, version, next_cursor }), ..Default::default() })
            }
            Command::ContextRead { item_id, expected_revision_id, start_byte, end_byte, max_sensitivity: _ } => {
                if state.controls.legacy_agent {
                    return err("invalid_request");
                }
                let Some(e) = find(&state.entries, item_id) else { return err("context_resource_not_found") };
                if &e.item.revision_id != expected_revision_id {
                    return err("stale_item_revision");
                }
                if start_byte >= end_byte {
                    return err("invalid_context_request");
                }
                if end_byte - start_byte > MAX_CONTEXT_BYTES {
                    return err("context_range_too_large");
                }
                let md = &e.markdown;
                if *end_byte > md.len() || !md.is_char_boundary(*start_byte) || !md.is_char_boundary(*end_byte) {
                    return err("invalid_context_range");
                }
                ok(Response {
                    context: Some(ContextSlice {
                        schema_version: 1,
                        input: RevisionRef { item_id: item_id.clone(), revision_id: e.item.revision_id.clone() },
                        origin: "machine_projection".into(),
                        coordinate_space: "revision_markdown_utf8_v1".into(),
                        content_sha256: format!("sha256:{:x}", Sha256::digest(md.as_bytes())),
                        total_bytes: md.len(),
                        start_byte: *start_byte,
                        end_byte: *end_byte,
                        content_markdown: md[*start_byte..*end_byte].to_string(),
                        source_url: Some("https://example.invalid/fixture".into()),
                        canonical_url: None,
                        sensitivity: Sensitivity::Private,
                    }),
                    ..Default::default()
                })
            }
        }
    }
}

fn trash_wire(e: &Entry, t: &Trashed) -> crate::trash::TrashEntry {
    crate::trash::TrashEntry {
        item_id: e.item.item_id.clone(),
        revision_id: e.item.revision_id.clone(),
        title: e.item.title.clone(),
        source_url: e.biblio.source_url.clone().unwrap_or_default(),
        trashed_at: t.trashed_at.clone(),
        reason: "duplicate".into(),
        kept_item_id: Some(t.kept.clone()),
        plan_id: Some(t.plan_id.clone()),
    }
}

fn origin(e: &Entry) -> Option<String> {
    site_origin(e.item.site_name.as_deref())
}

/// Moves requested synthetic minutes along by elapsed time.
fn advance_minutes(m: &mut Minutes, fast: bool) {
    let Some(started) = m.started else { return };
    let elapsed = started.elapsed();
    let (running, done) = if fast { (Duration::from_millis(20), Duration::from_millis(60)) } else { (TRANSLATION_QUEUED, MINUTES_DONE) };
    if elapsed >= done && m.fail_once {
        m.started = None;
        m.fail_once = false;
        m.state = MinutesState::Failed;
        m.reason = Some("provider_timeout".into());
    } else if elapsed >= done {
        m.started = None;
        m.state = MinutesState::Succeeded;
        m.text = Some(m.pending.clone());
    } else if elapsed >= running {
        m.state = MinutesState::Running;
    }
}

fn minutes_wire(m: &Minutes) -> AiMinutes {
    AiMinutes { state: m.state.clone(), reason: m.reason.clone(), text: (m.state == MinutesState::Succeeded).then(|| m.text.clone()).flatten() }
}

fn view_id(generation: u64, query: &QuerySpec) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    query.hash(&mut hasher);
    format!("view_{generation}_{:x}", hasher.finish())
}

fn find<'a>(entries: &'a [Entry], id: &str) -> Option<&'a Entry> {
    entries.iter().find(|e| e.item.item_id == id)
}

fn find_mut<'a>(entries: &'a mut [Entry], id: &str) -> Option<&'a mut Entry> {
    entries.iter_mut().find(|e| e.item.item_id == id)
}

/// Moves a requested synthetic translation along by elapsed time.
fn advance(t: &mut Translation) {
    let Some(started) = t.started else { return };
    let elapsed = started.elapsed();
    if elapsed < TRANSLATION_QUEUED {
        return;
    }
    // The title step comes first, then one batch at a time from where the last run stopped.
    t.state = AnalysisState::Running;
    t.title_done = true;
    let total = t.batches.1;
    let ran = ((elapsed - TRANSLATION_QUEUED).as_millis() / TRANSLATION_BATCH.as_millis()) as usize;
    if t.resume + ran >= total {
        t.started = None;
        t.state = AnalysisState::Succeeded;
        t.batches = (total, total);
        if let Some((title, markdown)) = t.pending.clone() {
            t.title = Some(title);
            t.markdown = Some(markdown);
        }
    } else {
        t.batches.0 = t.resume + ran;
    }
}

fn status(e: &Entry) -> TranslationStatus {
    let state = serde_json::to_value(e.translation.state).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
    let t = &e.translation;
    let failure_detail = t.failure_detail.clone().filter(|_| t.state == AnalysisState::Failed);
    let untranslated_blocks = t.untranslated.filter(|_| t.state == AnalysisState::Succeeded);
    TranslationStatus { state, reason: t.reason.clone(), revision_id: Some(e.item.revision_id.clone()), ai_minutes: None, translated_title: translated_title(t), progress: progress(t), failure_detail, untranslated_blocks }
}

/// As the agent: the title once its step succeeded, before the document.
fn translated_title(t: &Translation) -> Option<String> {
    match t.state {
        AnalysisState::Succeeded => t.title.clone(),
        AnalysisState::NotRequested | AnalysisState::Queued | AnalysisState::Running | AnalysisState::Failed if t.title_done => t.pending.as_ref().map(|p| p.0.clone()),
        _ => None,
    }
}

/// `with_context` once a full translation ran its title step; `title_only`
/// for a capture's title-only request.
fn title_source(t: &Translation) -> Option<String> {
    translated_title(t).map(|_| if t.state == AnalysisState::NotRequested { "title_only" } else { "with_context" }.into())
}

/// Batches finished, while queued, running or failed (as the agent reports them).
fn progress(t: &Translation) -> Option<TranslationProgress> {
    let (done, total) = t.batches;
    (matches!(t.state, AnalysisState::Queued | AnalysisState::Running | AnalysisState::Failed) && total > 0).then_some(TranslationProgress { done_batches: done, total_batches: total, started_at: t.wall_start.filter(|_| t.state == AnalysisState::Running) })
}

/// One page of the synthetic tree with direct and distinct subtree counts.
/// What a delete of `id` would touch now (bound into its preview token).
fn delete_impact(state: &State, id: &str) -> (Vec<String>, Vec<(String, String)>, String) {
    let nodes = subtree(&state.folders, id);
    let members: Vec<(String, String)> = state.entries.iter().flat_map(|e| e.groups.iter().filter(|g| nodes.contains(g)).map(|g| (e.item.item_id.clone(), g.clone()))).collect();
    let versions: Vec<String> = state.folders.iter().filter(|f| nodes.contains(&f.id)).map(|f| format!("{}:{}:{}", f.id, f.version, f.name)).collect();
    let digest = format!("{nodes:?}|{members:?}|{versions:?}");
    (nodes, members, digest)
}

/// The four `group_delete_*` commands, as the owner-local daemon answers them.
fn group_delete(state: &mut State, command: &Command) -> Response {
    let ok = |r: Response| Response { ok: true, ..r };
    let err = |code: &str| Response { ok: false, reason_code: Some(code.into()), ..Default::default() };
    let fields = |f: crate::group_delete::DeleteFields| ok(Response { group_delete: f, ..Default::default() });
    match command {
        Command::GroupDeletePreview { group_id, .. } => {
            let Some(folder) = state.folders.iter().find(|f| &f.id == group_id).cloned() else { return err("library_resource_not_found") };
            let (nodes, members, digest) = delete_impact(state, group_id);
            static TOKENS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
            let token = format!("dtok_{}", TOKENS.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
            state.delete_tokens.insert(token.clone(), (group_id.clone(), digest));
            let items: std::collections::HashSet<&String> = members.iter().map(|(i, _)| i).collect();
            fields(crate::group_delete::DeleteFields {
                group_id: Some(group_id.clone()), name: Some(folder.name), confirmation_token: Some(token),
                expires_at: Some("2026-11-08T12:05:00Z".into()), affected_node_count: Some(nodes.len()),
                affected_membership_count: Some(members.len()), distinct_item_count: Some(items.len()),
                includes_descendants: Some(true), items_retained: Some(true), recoverable: Some(true), ..Default::default()
            })
        }
        Command::GroupDelete { group_id, confirmation_token, request_id, .. } => {
            // Exact replay comes before the token check (the token may be gone).
            if let Some((kind, target, deletion)) = state.delete_requests.get(request_id) {
                return if *kind == "delete" && target == group_id {
                    fields(crate::group_delete::DeleteFields { group_id: Some(group_id.clone()), deletion_id: Some(deletion.clone()), outcome: Some("deleted".into()), ..Default::default() })
                } else { err("request_id_conflict") };
            }
            if state.controls.expire_delete_tokens {
                state.delete_tokens.clear();
            }
            let Some((target, digest)) = state.delete_tokens.get(confirmation_token).cloned() else { return err("group_delete_preview_expired") };
            if &target != group_id || !state.folders.iter().any(|f| &f.id == group_id) {
                return err("stale_group_delete_preview");
            }
            let (nodes, members, now) = delete_impact(state, group_id);
            if now != digest {
                return err("stale_group_delete_preview");
            }
            state.delete_tokens.remove(confirmation_token);
            let folder = state.folders.iter().find(|f| &f.id == group_id).cloned().unwrap();
            let taken: Vec<(usize, Folder)> = state.folders.iter().enumerate().filter(|(_, f)| nodes.contains(&f.id)).map(|(i, f)| (i, f.clone())).collect();
            state.folders.retain(|f| !nodes.contains(&f.id));
            for e in state.entries.iter_mut() {
                e.groups.retain(|g| !nodes.contains(g));
            }
            let id = format!("gdel_{}", state.next_deletion);
            state.next_deletion += 1;
            state.deletions.push(FixtureDeletion { id: id.clone(), group_id: group_id.clone(), name: folder.name, parent: folder.parent, deleted_at: format!("2026-11-08T10:{:02}:00Z", state.next_deletion % 60), folders: taken, memberships: members });
            state.delete_requests.insert(request_id.clone(), ("delete", group_id.clone(), id.clone()));
            state.groups_version += 1;
            state.generation += 1;
            fields(crate::group_delete::DeleteFields { group_id: Some(group_id.clone()), deletion_id: Some(id), outcome: Some("deleted".into()), ..Default::default() })
        }
        Command::GroupDeleteList { cursor, .. } => {
            let start = match cursor { None => 0, Some(c) => match c.strip_prefix("dcur_").and_then(|n| n.parse::<usize>().ok()) { Some(n) => n, None => return err("invalid_request") } };
            let page: Vec<crate::group_delete::Deletion> = state.deletions.iter().skip(start).take(50).map(|d| crate::group_delete::Deletion { deletion_id: d.id.clone(), group_id: d.group_id.clone(), name: d.name.clone(), parent_id: d.parent.clone(), deleted_at: d.deleted_at.clone() }).collect();
            let next = (start + page.len() < state.deletions.len()).then(|| format!("dcur_{}", start + page.len()));
            ok(Response { next_cursor: next, group_delete: crate::group_delete::DeleteFields { group_deletions: Some(page), ..Default::default() }, ..Default::default() })
        }
        Command::GroupDeleteRestore { deletion_id, request_id, .. } => {
            if let Some((kind, target, group)) = state.delete_requests.get(request_id) {
                return if *kind == "restore" && target == deletion_id {
                    fields(crate::group_delete::DeleteFields { group_id: Some(group.clone()), deletion_id: Some(deletion_id.clone()), outcome: Some("restored".into()), ..Default::default() })
                } else { err("request_id_conflict") };
            }
            let Some(index) = state.deletions.iter().position(|d| &d.id == deletion_id) else { return err("library_resource_not_found") };
            // A parent still deleted: rejected, nothing consumed.
            if let Some(parent) = &state.deletions[index].parent {
                if !state.folders.iter().any(|f| &f.id == parent) {
                    return err("group_delete_restore_conflict");
                }
            }
            let d = state.deletions.remove(index);
            for (slot, folder) in d.folders {
                let at = slot.min(state.folders.len());
                state.folders.insert(at, folder);
            }
            for (item, group) in d.memberships {
                if let Some(e) = state.entries.iter_mut().find(|e| e.item.item_id == item) {
                    if !e.groups.contains(&group) { e.groups.push(group); }
                }
            }
            state.delete_requests.insert(request_id.clone(), ("restore", deletion_id.clone(), d.group_id.clone()));
            state.groups_version += 1;
            state.generation += 1;
            fields(crate::group_delete::DeleteFields { group_id: Some(d.group_id), deletion_id: Some(deletion_id.clone()), outcome: Some("restored".into()), ..Default::default() })
        }
        _ => err("invalid_request"),
    }
}

/// The receipt of a write on an existing node (the target of a membership edit).
fn receipt(state: &State, op: &GroupEditOp) -> Option<crate::GroupEditReceipt> {
    let id = match op {
        GroupEditOp::TransferFolders { target_parent_id, .. } => target_parent_id,
        GroupEditOp::Rename { id, .. } | GroupEditOp::AddMembers { group_id: id, .. } | GroupEditOp::RemoveMembers { group_id: id, .. } | GroupEditOp::MoveMembers { target_group_id: id, .. } => id,
        _ => return None,
    };
    state.folders.iter().find(|f| &f.id == id).map(|f| crate::GroupEditReceipt { group_id: f.id.clone(), version: format!("v{}", f.version) })
}

fn tree_page(state: &State, offset: usize, limit: usize) -> GroupTreePage {
    let ordered = preorder(&state.folders);
    let nodes: Vec<GroupNodeView> = ordered
        .iter()
        .skip(offset)
        .take(limit)
        .map(|f| {
            let nodes = subtree(&state.folders, &f.id);
            GroupNodeView {
                id: f.id.clone(),
                name: f.name.clone(),
                kind: f.kind.clone(),
                parent_id: f.parent.clone(),
                root_group_id: root_of(&state.folders, &f.id),
                item_count: state.entries.iter().filter(|e| e.groups.contains(&f.id)).count(),
                subtree_item_count: state.entries.iter().filter(|e| in_scope(e, &nodes)).count(),
                read_only: false,
                version: format!("v{}", f.version),
            }
        })
        .collect();
    let next = offset + nodes.len();
    GroupTreePage {
        schema_version: 1,
        view: format!("gview_{}_{}", state.groups_version, state.generation),
        total: ordered.len(),
        offset,
        next_offset: (next < ordered.len()).then_some(next),
        nodes,
    }
}

/// The stored conversation of `CHAT_HISTORY_ITEM` (synthetic): one exchange restored
/// from a screenshot (negative `seq`, sorted first), then one asked on the phone.
fn synthetic_history() -> Vec<crate::chat::StoredTurn> {
    use crate::chat::{ChatRole, StoredTurn, RESTORED_FROM_USER_SCREENSHOT};
    let turn = |seq: i64, role, content: &str, request_id: &str, origin: Option<&str>| StoredTurn {
        seq,
        role,
        content: content.into(),
        request_id: request_id.into(),
        created_at_ms: 1_792_000_000_000 + seq,
        model: (role == ChatRole::Assistant && origin.is_none()).then(|| "fixture-model-alpha".into()),
        origin: origin.map(str::to_string),
        truncated: false,
    };
    let restored = Some(RESTORED_FROM_USER_SCREENSHOT);
    vec![
        turn(-2, ChatRole::User, "（合成记录）这篇文章主要解决什么问题？", "restored:fixture:00", restored),
        turn(-1, ChatRole::Assistant, "（合成记录，非模型生成）这是从截图恢复的一段示例对话：文章讨论长文档检索中段落之间的指代如何保留。", "restored:fixture:01", restored),
        turn(1, ChatRole::User, "（合成记录）作者的方法有什么局限？", "chat-fixture-history-0001", None),
        turn(2, ChatRole::Assistant, "（合成记录，非模型生成）示例回答：记忆容量固定，跨文档迁移没有验证。继续提问时，这两轮会作为上下文一并发送。", "chat-fixture-history-0001", None),
    ]
}

fn bump(entry: &mut Entry) {
    entry.revision_number += 1;
    let base = entry.item.item_id.trim_start_matches("item_fixture_");
    entry.item.revision_id = format!("rev_fixture_{base}_{}", entry.revision_number);
}

impl Backend for FixtureBackend {
    fn send(&self, command: &Command) -> Result<Response, ClientError> {
        let controls = self.controls();
        if !controls.latency.is_zero() {
            thread::sleep(controls.latency);
        }
        if controls.offline {
            return Err(ClientError::Unavailable);
        }
        // A model answer takes a while; the state lock is not held meanwhile.
        if matches!(command, Command::ItemChat { .. }) && !controls.instant_chat {
            thread::sleep(CHAT_DELAY);
        }
        if matches!(command, Command::ConversationRead { .. }) && !controls.instant_chat {
            thread::sleep(if controls.slow_history { SLOW_HISTORY_DELAY } else { HISTORY_DELAY });
        }
        let pass_edit = matches!(command, Command::Reader { request: crate::book::ReaderRequest::PassesEdit { .. }, .. });
        if let Command::Reader { request: crate::book::ReaderRequest::PassesEdit { item_id, .. }, .. } = command {
            if controls.stale_next_pass_edit {
                self.state.lock().unwrap().passes.touch(item_id);
                self.set_controls(FixtureControls { stale_next_pass_edit: false, ..controls.clone() });
            }
        }
        if controls.expire_pass_sessions && matches!(command, Command::Reader { request: crate::book::ReaderRequest::PassesTimingReport { .. }, .. }) {
            self.state.lock().unwrap().passes.expire_sessions();
            self.set_controls(FixtureControls { expire_pass_sessions: false, ..self.controls() });
        }
        let reply = interpret(command, self.handle(command));
        if (controls.lose_next_folder_reply || controls.folder_commit_error) && reply.is_ok()
            && matches!(command, Command::GroupEdit { ops, .. } if matches!(ops.as_slice(), [GroupEditOp::TransferFolders { .. }])) {
            self.set_controls(FixtureControls { lose_next_folder_reply: false, folder_commit_error: false, ..self.controls() });
            if controls.folder_commit_error { return Err(ClientError::Daemon("internal_error".into())); }
            return Err(ClientError::Timeout);
        }
        if matches!(command, Command::UiActiveGroupSet { .. }) && controls.lose_active_group_replies > 0 {
            let mut state = self.state.lock().unwrap();
            state.controls.lose_active_group_replies = state.controls.lose_active_group_replies.saturating_sub(1);
            return Err(ClientError::Timeout);
        }
        if pass_edit && controls.lose_next_pass_reply {
            self.set_controls(FixtureControls { lose_next_pass_reply: false, ..self.controls() });
            return Err(ClientError::Timeout);
        }
        if pass_edit && controls.fail_after_next_pass_commit && reply.is_ok() {
            self.set_controls(FixtureControls { fail_after_next_pass_commit: false, ..self.controls() });
            return Err(ClientError::Daemon("internal_error".into()));
        }
        let annotation_edit = matches!(command, Command::Reader { request: crate::book::ReaderRequest::AnnotationsEdit { .. } | crate::book::ReaderRequest::AiAnnotationsEdit { .. }, .. });
        if controls.lose_next_annotation_reply && annotation_edit {
            self.set_controls(FixtureControls { lose_next_annotation_reply: false, ..controls });
            return Err(ClientError::Timeout);
        }
        if controls.fail_after_next_annotation_commit && annotation_edit && reply.is_ok() {
            self.set_controls(FixtureControls { fail_after_next_annotation_commit: false, ..controls });
            return Err(ClientError::Daemon("internal_error".into()));
        }
        reply
    }

    fn mode(&self) -> BackendMode {
        BackendMode::SyntheticFixture
    }
}

/// What a reading pass reads of a synthetic item: revision, source and a finished translation.
fn pass_text(e: &Entry) -> crate::fixture_passes::ItemText {
    crate::fixture_passes::ItemText {
        rev: e.item.revision_id.clone(),
        source: e.markdown.clone(),
        translation: e.translation.markdown.clone().filter(|_| e.translation.state == AnalysisState::Succeeded),
    }
}
