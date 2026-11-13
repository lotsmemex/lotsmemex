//! Wire DTOs mirroring `crates/memex-agent/src/ipc.rs` at 916e67a plus the P1
//! `context_read` contract. Responses ignore unknown fields so newer agents stay
//! readable; requests carry exactly the fields the agent accepts.

use crate::biblio::{AgentCapabilities, Bibliographic, IdentifierDuplicate, IdentifierQuery, LookupPreview, MetadataEdit, MetadataV2Read};
use crate::chat::{ChatTurn, ItemChatReply};
use crate::insights::{AiInsights, InsightKind};
use crate::{tx, txf};
use serde::{Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Command {
    LibraryList { cursor: Option<String> },
    ItemRead { item_id: String },
    ScopedItemRead { item_id: String, max_sensitivity: Sensitivity },
    ImageRead { item_id: String, expected_revision_id: String, target: String, offset: usize, max_sensitivity: Sensitivity },
    LibraryItemRead { item_id: String },
    UpdateItemMetadata { item_id: String, expected_revision_id: String, title: String, manual_tags: Vec<String> },
    List { cursor: Option<String> },
    Retry { job_id: String },
    Cancel { job_id: String },
    Approve { job_id: String },
    ContextRead { item_id: String, expected_revision_id: String, start_byte: usize, end_byte: usize, max_sensitivity: Sensitivity },
    LibraryQuery { query: QuerySpec, max_sensitivity: Sensitivity, offset: usize, limit: usize, expected_view: Option<String> },
    ModelSettingsRead,
    ModelSettingsReplace { expected_revision: u64, configuration: ModelConfiguration },
    ProviderCredentialRead { expected_revision: u64, provider_id: String },
    ProviderCredentialReplace { expected_revision: u64, provider_id: String, expected_generation: u64, api_key: Secret },
    ProviderCredentialClear { expected_revision: u64, provider_id: String, expected_generation: u64 },
    ProviderModelsDiscover { expected_revision: u64, provider_id: String, authorization: DiscoverAuthorization },
    /// Protocol discovery; makes no request elsewhere.
    Capabilities,
    /// `{version, binary_sha256}` of the running agent (2026-11-03 candidate; not
    /// capability-gated: an older agent refuses the command).
    AgentVersion,
    BibliographicMetadataRead { item_id: String, max_sensitivity: Sensitivity },
    BibliographicMetadataEdit { max_sensitivity: Sensitivity, edit: MetadataEdit },
    MarkdownEdit { max_sensitivity: Sensitivity, edit: crate::reading::MarkdownEdit },
    ReadingAnnotationsRead { item_id: String, revision_id: String, max_sensitivity: Sensitivity },
    ReadingAnnotationsSave { expected_version: String, annotations: crate::reading::ReadingAnnotations, max_sensitivity: Sensitivity },
    BibliographicDuplicateCheck { identifiers: Vec<IdentifierQuery>, exclude_item_id: Option<String>, max_sensitivity: Sensitivity },
    /// Explicit owner request: only this item's DOI (Crossref) or ISBN (Open Library) leaves the Mac.
    BibliographicLookupPreview { item_id: String, expected_revision_id: String, identifier: IdentifierQuery, max_sensitivity: Sensitivity },
    /// `groups_v1` folder tree, one page; later pages are bound to `expected_view`.
    GroupTreeRead { max_sensitivity: Sensitivity, offset: usize, limit: usize, expected_view: Option<String> },
    /// `groups_v1` write (`writes: true` only): exactly one op. `expected_version` is
    /// the target node's version (the parent's for `create_folder`) and is absent for
    /// `create_root`. `request_id` makes a retry safe.
    GroupEdit {
        max_sensitivity: Sensitivity,
        request_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        expected_version: Option<String>,
        ops: Vec<GroupEditOp>,
    },
    /// Explicit owner request to translate one item (`translation_provider_v1.available` only).
    TranslationRequest { item_id: String, max_sensitivity: Sensitivity },
    /// Read-only translation state with its reason; never enqueues.
    ScopedTranslationStatus { item_id: String, max_sensitivity: Sensitivity },
    /// `item_chat_v1`: one answer about one item. May take up to ~120 s.
    ItemChat { item_id: String, max_sensitivity: Sensitivity, request_id: String, messages: Vec<ChatTurn> },
    /// `item_chat_history_v1`: one page of the item's stored chat turns, read-only.
    ConversationRead {
        item_id: String,
        max_sensitivity: Sensitivity,
        #[serde(skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
    },
    /// `minutes_request_v1`: explicit owner request for one item's AI 纪要.
    MinutesRequest { item_id: String, max_sensitivity: Sensitivity },
    /// `ai_insights_v1`: explicit owner request for 重点标注 / 金句 / 事实核查.
    InsightRequest { item_id: String, max_sensitivity: Sensitivity, kinds: Vec<InsightKind> },
    /// `dedup_trash_v1`: one page (50) of the recoverable trash, ascending item id.
    TrashList { cursor: Option<String> },
    /// `dedup_trash_v1`: puts a trashed item back into the library. No hard delete exists.
    ItemTrashRestore { item_id: String },
    GroupTrashList { group_id: String, cursor: Option<String>, max_sensitivity: Sensitivity },
    GroupTrashEdit { group_id: String, item_id: String, request_id: String, expected_revision_id: String, restore_plan: Option<String>, max_sensitivity: Sensitivity },
    /// `groups_v1.node_delete`: what deleting a subtree would remove, with a confirmation token.
    GroupDeletePreview { group_id: String, max_sensitivity: Sensitivity },
    /// Deletes the previewed subtree (recoverable; items are kept).
    GroupDelete { group_id: String, confirmation_token: String, request_id: String, max_sensitivity: Sensitivity },
    /// One page (≤ 50) of outstanding deletions; `cursor` is sent as null for the first.
    GroupDeleteList { cursor: Option<String>, max_sensitivity: Sensitivity },
    GroupDeleteRestore { deletion_id: String, request_id: String, max_sensitivity: Sensitivity },
    /// `reader_v1`: a book's table of contents or one bounded chunk of a chapter.
    Reader { max_sensitivity: Sensitivity, request: crate::book::ReaderRequest },
    /// `item_cover_v1`: one nullable cover per requested item (at most 50).
    ItemCoverBatch { item_ids: Vec<String>, max_sensitivity: Sensitivity },
    /// `item_cover_v1`: one chunk (<= 96 KiB) of a cover version's original or thumbnail.
    ItemCoverChunk { item_id: String, version: String, thumbnail: bool, offset: usize, max_sensitivity: Sensitivity },
    /// `ui_active_group_set` (see `active_group.rs`): the owner's explicit
    /// sidebar choice; `group_id` is sent as `null` for a virtual view.
    UiActiveGroupSet { request_id: String, group_id: Option<String>, max_sensitivity: Sensitivity },
    /// `capture_jobs_recent_v1` (see `capture_jobs.rs`): read-only URL capture progress.
    CaptureJobsRecent { max_sensitivity: Sensitivity, limit: u32 },
}

impl Command {
    /// Commands an older (916) agent does not know.
    pub fn is_extension(&self) -> bool {
        !matches!(
            self,
            Command::LibraryList { .. }
                | Command::ItemRead { .. }
                | Command::LibraryItemRead { .. }
                | Command::UpdateItemMetadata { .. }
                | Command::List { .. }
                | Command::Retry { .. }
                | Command::Cancel { .. }
                | Command::Approve { .. }
        )
    }

    /// Sent only after `capabilities` advertised them, so a rejection is the
    /// agent's answer, never "this agent is too old".
    pub fn is_capability_gated(&self) -> bool {
        matches!(
            self,
            Command::BibliographicMetadataRead { .. }
                | Command::BibliographicMetadataEdit { .. }
                | Command::BibliographicDuplicateCheck { .. }
                | Command::BibliographicLookupPreview { .. }
                | Command::GroupTreeRead { .. }
                | Command::GroupEdit { .. }
                | Command::TranslationRequest { .. }
                | Command::ItemChat { .. }
                | Command::ConversationRead { .. }
                | Command::InsightRequest { .. }
                | Command::MinutesRequest { .. }
                | Command::TrashList { .. }
                | Command::ItemTrashRestore { .. }
                | Command::GroupTrashList { .. }
                | Command::GroupTrashEdit { .. }
                | Command::GroupDeletePreview { .. }
                | Command::GroupDelete { .. }
                | Command::GroupDeleteList { .. }
                | Command::GroupDeleteRestore { .. }
                | Command::Reader { .. }
                | Command::ItemCoverBatch { .. }
                | Command::ItemCoverChunk { .. }
                | Command::UiActiveGroupSet { .. }
                | Command::CaptureJobsRecent { .. }
        )
    }
}

/// What the agent reports about itself. `binary_sha256` (a short prefix) is
/// `None` while the agent is still hashing its executable, or cannot read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentVersion {
    pub version: String,
    #[serde(default)]
    pub binary_sha256: Option<String>,
    #[serde(default)]
    pub query_origin: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(default)]
    pub reason_code: Option<String>,
    #[serde(default)]
    pub next_cursor: Option<String>,
    #[serde(default)]
    pub item: Option<CanonicalItem>,
    #[serde(default)]
    pub library_entry: Option<crate::reading::LibraryEntry>,
    #[serde(default)]
    pub image: Option<crate::images::ImageChunk>,
    /// `item_cover_batch` (array) or `item_cover_chunk` (object) result.
    #[serde(default)]
    pub item_cover: Option<serde_json::Value>,
    #[serde(default)]
    pub library_items: Option<Vec<LibraryItem>>,
    #[serde(default)]
    pub library_item: Option<TranslationItem>,
    #[serde(default)]
    pub jobs: Option<Vec<Job>>,
    #[serde(default)]
    pub job: Option<Job>,
    #[serde(default)]
    pub item_update: Option<ItemUpdate>,
    #[serde(default)]
    pub reading_annotations: Option<crate::reading::ReadingAnnotations>,
    #[serde(default)]
    pub context: Option<ContextSlice>,
    #[serde(default)]
    pub library_query: Option<QueryWindow>,
    #[serde(default)]
    pub model_settings: Option<ModelSettings>,
    #[serde(default)]
    pub provider_credential: Option<ProviderCredential>,
    #[serde(default)]
    pub provider_models: Option<ProviderModels>,
    #[serde(default)]
    pub capabilities: Option<AgentCapabilities>,
    #[serde(default)]
    pub bibliographic_metadata: Option<MetadataV2Read>,
    #[serde(default)]
    pub identifier_duplicates: Option<Vec<IdentifierDuplicate>>,
    #[serde(default)]
    pub lookup_preview: Option<LookupPreview>,
    #[serde(default)]
    pub translation: Option<TranslationStatus>,
    #[serde(default)]
    pub group_tree: Option<GroupTreePage>,
    /// The `group_edit` receipt: the written node (the target of a membership
    /// edit) and its version right after that write.
    #[serde(default)]
    pub group_edit: Option<GroupEditReceipt>,
    /// AI minutes next to `translation` in a `translation_request` reply.
    #[serde(default, deserialize_with = "lenient")]
    pub ai_minutes: Option<AiMinutes>,
    #[serde(default)]
    pub item_chat: Option<ItemChatReply>,
    #[serde(default)]
    pub conversation: Option<crate::chat::ConversationPage>,
    #[serde(default, deserialize_with = "lenient")]
    pub ai_insights: Option<AiInsights>,
    #[serde(default)]
    pub trash_items: Option<Vec<crate::trash::TrashEntry>>,
    #[serde(default)]
    pub trash_restore: Option<crate::trash::TrashRestore>,
    #[serde(default)]
    pub agent_version: Option<AgentVersion>,
    /// `reader_v1` reply (`ReaderDocument` or `ReaderChunk`), decoded by `book`.
    #[serde(default)]
    pub reader: Option<serde_json::Value>,
    /// `group_delete_*` replies (flat fields).
    #[serde(flatten)]
    pub group_delete: crate::group_delete::DeleteFields,
    /// `ui_active_group_set` acknowledgement.
    #[serde(default)]
    pub ui_active_group: Option<crate::active_group::ActiveGroupAck>,
    /// `capture_jobs_recent` reply.
    #[serde(default)]
    pub capture_jobs: Option<crate::capture_jobs::CaptureJobsRecent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryItem {
    pub item_id: String,
    pub revision_id: String,
    pub title: String,
    pub item_type: String,
    #[serde(default)]
    pub captured_at: Option<String>,
    #[serde(default)]
    pub site_name: Option<String>,
    /// Additive (2026-11-09): publication day `YYYY-MM-DD` of the current revision;
    /// `None` when omitted (older agent), null, year/month-only or unparsed.
    /// Never a capture date.
    #[serde(default)]
    pub published_at: Option<String>,
    /// Additive (2026-11-09): positive whole seconds of the current revision;
    /// `None` when omitted or null.
    #[serde(default)]
    pub duration_seconds: Option<u32>,
}

impl LibraryItem {
    pub fn source(&self) -> &str {
        self.site_name.as_deref().unwrap_or(OTHER_SOURCE)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalItem {
    pub item_id: String,
    pub revision_id: String,
    pub title: String,
    pub markdown: String,
    #[serde(default)]
    pub manual_tags: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisState {
    NotRequested,
    Queued,
    Running,
    Succeeded,
    Failed,
    Unavailable,
}

impl AnalysisState {
    pub fn label(self) -> &'static str {
        crate::tx(match self {
            AnalysisState::NotRequested => "未请求",
            AnalysisState::Queued => "排队中",
            AnalysisState::Running => "进行中",
            AnalysisState::Succeeded => "已完成",
            AnalysisState::Failed => "失败",
            AnalysisState::Unavailable => "不可用",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranslationItem {
    pub item_id: String,
    pub revision_id: String,
    pub summary_state: AnalysisState,
    #[serde(default)]
    pub summary: Option<String>,
    pub translation_state: AnalysisState,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub markdown: Option<String>,
    /// Why translation failed or is unavailable. Filled from
    /// `scoped_translation_status` when the agent provides it; never invented.
    #[serde(default)]
    pub translation_reason: Option<String>,
    /// AI 纪要. `None`: an older agent without the field (the feature is hidden).
    #[serde(default, deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub ai_minutes: Option<AiMinutes>,
    /// 重点标注 / 金句 / 事实核查. `None`: an agent without `ai_insights_v1`.
    #[serde(default, deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub ai_insights: Option<AiInsights>,
    /// The translated title as soon as the title step succeeded, even when the
    /// document did not (`title` stays success-only). Absent from older agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translated_title: Option<String>,
    /// Document batches finished while queued, running or failed; omitted when
    /// there is nothing to report.
    #[serde(default, deserialize_with = "lenient", skip_serializing_if = "Option::is_none")]
    pub translation_progress: Option<TranslationProgress>,
    /// With `translated_title`: `title_only` (the capture's title-only request) or
    /// `with_context` (the full translation's title step).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_source: Option<String>,
    /// Fixed diagnostic of the last failed attempt (`batch_3_of_5:count_mismatch`),
    /// from `scoped_translation_status` of newer agents; never invented.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translation_failure_detail: Option<String>,
    /// Blocks of a finished translation kept in the original language (newer agents).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub untranslated_blocks: Option<usize>,
}

impl TranslationItem {
    /// 中译标题: `translated_title`, or `title` of a finished translation; `None`
    /// when empty or the same as `original`.
    pub fn chinese_title(&self, original: &str) -> Option<String> {
        let succeeded = self.title.as_ref().filter(|_| self.translation_state == AnalysisState::Succeeded);
        self.translated_title.as_ref().or(succeeded).map(|t| t.trim().to_string()).filter(|t| !t.is_empty() && t != original.trim())
    }
}

/// First line of a translated block the agent kept in the original language.
pub const UNTRANSLATED_BLOCK_MARKER: &str = "<!-- memex:untranslated -->";

/// `{done_batches, total_batches}` of a document translation, and when the
/// running attempt started (newer agents, RFC 3339 on the wire, Unix seconds here).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranslationProgress {
    pub done_batches: usize,
    pub total_batches: usize,
    #[serde(default, deserialize_with = "rfc3339_seconds", serialize_with = "seconds_rfc3339", skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
}

/// Unix seconds of an RFC 3339 time (`2026-11-09T17:34:12.5+00:00` / `Z`); a
/// value this desktop cannot read is absent.
fn rfc3339_seconds<'de, D: Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    let value = Option::<String>::deserialize(d).ok().flatten();
    Ok(value.as_deref().and_then(parse_rfc3339))
}

fn seconds_rfc3339<S: serde::Serializer>(value: &Option<i64>, s: S) -> Result<S::Ok, S::Error> {
    match value {
        Some(secs) => {
            let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
            let (y, m, d) = civil_from_days(days);
            s.serialize_str(&format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}+00:00", rem / 3600, rem / 60 % 60, rem % 60))
        }
        None => s.serialize_none(),
    }
}

pub fn parse_rfc3339(value: &str) -> Option<i64> {
    let b = value.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| value.get(r)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (num(0..4)?, num(5..7)?, num(8..10)?, num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    let mut rest = &value[19..];
    if let Some(frac) = rest.strip_prefix('.') {
        let digits = frac.bytes().take_while(u8::is_ascii_digit).count();
        rest = &frac[digits..];
    }
    let offset = match rest {
        "Z" | "z" => 0,
        _ if rest.len() == 6 && matches!(rest.as_bytes()[0], b'+' | b'-') && rest.as_bytes()[3] == b':' => {
            let sign = if rest.starts_with('-') { -1 } else { 1 };
            sign * (rest[1..3].parse::<i64>().ok()? * 3600 + rest[4..6].parse::<i64>().ok()? * 60)
        }
        _ => return None,
    };
    Some(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + se - offset)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

pub(crate) fn unix_now() -> i64 {
    now_seconds()
}

fn now_seconds() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

impl TranslationProgress {
    /// `5/18`; `None` when the agent reports no batches.
    pub fn fraction(&self) -> Option<String> {
        (self.total_batches > 0).then(|| format!("{}/{}", self.done_batches.min(self.total_batches), self.total_batches))
    }
}

/// 翻译中 / 翻译中 5/18; with a newer agent's start time 翻译中 1/2 段 · 已用 3 分钟.
pub fn translating_label(progress: Option<&TranslationProgress>) -> String {
    translating_label_at(progress, now_seconds())
}

pub fn translating_label_at(progress: Option<&TranslationProgress>, now: i64) -> String {
    let minutes = progress.and_then(|p| p.started_at).map(|start| (now - start).max(0) / 60);
    match (progress.and_then(TranslationProgress::fraction), minutes) {
        (Some(f), Some(0)) => txf("翻译中 {0} 段 · 刚开始", &[&f]),
        (Some(f), Some(m)) => txf("翻译中 {0} 段 · 已用 {1} 分钟", &[&f, &m]),
        (Some(f), None) => txf("翻译中 {0}", &[&f]),
        (None, Some(m)) if m > 0 => txf("翻译中 · 已用 {0} 分钟", &[&m]),
        (None, _) => tx("翻译中").into(),
    }
}

/// Plain words for a `failure_detail` (`batch_3_of_5:count_mismatch` →
/// 第 3 段：模型返回块数不对); `None` for a code this desktop does not read.
pub fn failure_detail_label(detail: &str) -> Option<String> {
    let (scope, check) = detail.split_once(':')?;
    let what = tx(match check {
        "count_mismatch" => "模型返回块数不对",
        "json_parse" => "模型返回的不是有效 JSON",
        "incomplete_length" => "输出过长被截断",
        "incomplete_finish" => "模型没有正常结束",
        "invalid_response" | "empty" => "模型返回了空内容或无法解析的内容",
        "model_mismatch" => "返回的模型与设置不一致",
        "outcome_unknown" => "连接中断，结果未知",
        "rejected" | "provider_failure" => "翻译服务拒绝了请求",
        "timeout" => "翻译服务响应超时",
        "cjk_ratio" => "译文里中文太少",
        "identical" | "block_identical" | "contains_source" => "译文与原文相同",
        "too_many_untranslated" => "未能翻译的段落太多",
        "too_short" => "译文过短",
        "too_long" => "译文过长",
        "unbalanced_fence" | "code_fence" => "代码块不完整",
        "lease_expired" => "翻译进程中断",
        "malformed" | "invalid_untranslated_mark" | "multiline" | "heading" | "control_chars" | "too_large" => "译文格式不对",
        _ => return None,
    });
    let batch = scope.strip_prefix("batch_").map(|rest| rest.split("_of_").next().unwrap_or(rest));
    Some(match (scope, batch) {
        (_, Some(n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => txf("第 {0} 段：{1}", &[&n, &what]),
        ("title", _) => txf("标题：{0}", &[&what]),
        ("document", _) => txf("正文：{0}", &[&what]),
        ("glossary", _) => txf("术语表：{0}", &[&what]),
        _ => what.to_string(),
    })
}

/// 翻译未完成（已完成 5/18）for a failed translation that got part of the way;
/// `None` when no batch was finished (nothing to resume).
pub fn unfinished_label(progress: Option<&TranslationProgress>) -> Option<String> {
    progress.filter(|p| p.done_batches > 0).and_then(TranslationProgress::fraction).map(|f| txf("翻译未完成（已完成 {0}）", &[&f]))
}

/// An additive field that must never fail the read it rides on: a shape this
/// desktop does not understand is treated as absent.
fn lenient<'de, D: Deserializer<'de>, T: serde::de::DeserializeOwned>(d: D) -> Result<Option<T>, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(value).ok())
}

/// State of the item's AI 纪要 (`ai_minutes.state`); values a newer agent adds
/// are kept as `Other`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum MinutesState {
    NotRequested,
    Queued,
    Running,
    Succeeded,
    Failed,
    Unavailable,
    Other(String),
}

impl From<String> for MinutesState {
    fn from(s: String) -> Self {
        match s.as_str() {
            "not_requested" => Self::NotRequested,
            "queued" | "leased" => Self::Queued,
            "running" => Self::Running,
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            "unavailable" => Self::Unavailable,
            _ => Self::Other(s),
        }
    }
}

impl From<MinutesState> for String {
    fn from(s: MinutesState) -> Self {
        match s {
            MinutesState::NotRequested => "not_requested".into(),
            MinutesState::Queued => "queued".into(),
            MinutesState::Running => "running".into(),
            MinutesState::Succeeded => "succeeded".into(),
            MinutesState::Failed => "failed".into(),
            MinutesState::Unavailable => "unavailable".into(),
            MinutesState::Other(s) => s,
        }
    }
}

/// `ai_minutes {state, reason, text}`; `text` only counts when succeeded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiMinutes {
    pub state: MinutesState,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
}

/// Feature advertising `minutes_request`: minutes are made on their own request,
/// and `translation_request` no longer makes them.
pub const MINUTES_REQUEST_FEATURE: &str = "minutes_request_v1";

impl AiMinutes {
    pub fn new(state: MinutesState, reason: Option<&str>, text: Option<&str>) -> Self {
        Self { state, reason: reason.map(str::to_string), text: text.map(str::to_string) }
    }

    /// Queued or running: follow it until it ends.
    pub fn is_pending(&self) -> bool {
        matches!(self.state, MinutesState::Queued | MinutesState::Running)
    }

    /// The minutes text, only when the agent says they succeeded.
    pub fn text(&self) -> Option<&str> {
        match self.state {
            MinutesState::Succeeded => self.text.as_deref().filter(|t| !t.trim().is_empty()),
            _ => None,
        }
    }

    /// One line for a state without text; `None` when nothing should be shown.
    pub fn state_line(&self) -> Option<String> {
        match &self.state {
            MinutesState::Queued | MinutesState::Running => Some(crate::tx("纪要生成中…").into()),
            MinutesState::Failed => Some(crate::txf("纪要生成失败：{0}", &[&minutes_reason_label(self.reason.as_deref())])),
            MinutesState::Succeeded if self.text().is_none() => Some(crate::tx("纪要为空").into()),
            _ => None,
        }
    }
}

/// Minutes text as Markdown for display: lines starting with a middle dot or
/// bullet character become list items (agents often write lists that way).
pub fn minutes_markdown(text: &str) -> String {
    text.lines()
        .map(|line| {
            let t = line.trim_start();
            match t.strip_prefix('·').or_else(|| t.strip_prefix('•')).or_else(|| t.strip_prefix('・')) {
                Some(rest) => format!("- {}", rest.trim_start()),
                None => line.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Why minutes failed, in plain words; unknown codes are shown as given.
pub fn minutes_reason_label(reason: Option<&str>) -> String {
    match reason {
        None => crate::tx("代理未说明原因").into(),
        Some("no_minutes_provider" | "no_summary_provider" | "minutes_unavailable") => crate::tx("还没有配置纪要模型").into(),
        Some("source_too_large") => crate::tx("原文过长，暂不支持生成纪要").into(),
        Some("provider_timeout" | "minutes_provider_timeout") => crate::tx("模型服务响应超时").into(),
        Some("provider_failure" | "minutes_provider_failure") => crate::tx("模型服务返回错误").into(),
        Some(other) => translation_reason_label(other),
    }
}

/// `translation {state, reason, revision_id, ...}` from `translation_request` and
/// `scoped_translation_status`. States: queued, leased, running, succeeded,
/// failed, not_requested, unavailable (kept as strings: newer agents may add more).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TranslationStatus {
    pub state: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub revision_id: Option<String>,
    /// AI minutes the same request started (`translation_request` also asks for
    /// them); filled from the reply's `ai_minutes` by `Client::translation_request`.
    #[serde(default, deserialize_with = "lenient")]
    pub ai_minutes: Option<AiMinutes>,
    /// The translated title once the title step succeeded (newer agents).
    #[serde(default)]
    pub translated_title: Option<String>,
    /// Document batches finished so far (newer agents; `null` when none to report).
    #[serde(default, deserialize_with = "lenient")]
    pub progress: Option<TranslationProgress>,
    /// Fixed diagnostic of a failed attempt (newer agents).
    #[serde(default, deserialize_with = "lenient")]
    pub failure_detail: Option<String>,
    /// Blocks of a succeeded translation kept in the original (newer agents).
    #[serde(default, deserialize_with = "lenient")]
    pub untranslated_blocks: Option<usize>,
}

/// Plain-words reason for a translation state; unknown codes are shown as given.
pub fn translation_reason_label(reason: &str) -> String {
    let key = match reason {
        "source_is_chinese" => "原文为中文，无需翻译",
        "no_translation_provider" | "translation_unavailable" => "还没有配置翻译模型",
        "provider_timeout" | "translation_provider_timeout" => "翻译服务响应超时",
        "provider_failure" | "translation_provider_failure" => "翻译服务返回错误",
        "retry_exhausted" => "多次重试后仍未成功",
        "malformed_model_output" => "模型返回的译文格式不对",
        "provider_model_mismatch" => "翻译模型与设置不一致",
        "invalid_provider_configuration" => "翻译模型的设置有误",
        "stale_revision" | "stale_item_revision" => "原文已修改，译文已过期",
        "translation_lease_lost" | "worker_recovered" => "翻译中断，已重新排队",
        "capture_hook_pending" => "等待排入翻译队列",
        "analysis_conflict" => "译文与已有结果冲突，未保存",
        "unsupported_item_type" => "这类条目暂不支持翻译",
        "challenge_or_login" => "抓取到的是验证或登录页，无法翻译",
        "source_too_large" => "原文过长，暂不支持翻译",
        "source_language_unknown" => "无法判断原文语言",
        other => return crate::txf("代理原因：{0}", &[&other]),
    };
    crate::tx(key).to_string()
}

/// Label for an `item_type`: known wire values in the interface language, a
/// known interface label (fixture data) re-labelled, anything else as given.
pub fn item_type_label(item_type: &str) -> String {
    // Zotero's camelCase names (journalArticle) and snake_case read the same.
    let mut snake = String::with_capacity(item_type.len() + 4);
    for c in item_type.trim().chars() {
        if c.is_ascii_uppercase() {
            snake.push('_');
        }
        snake.push(c.to_ascii_lowercase());
    }
    let key = match snake.replace(['-', ' '], "_").as_str() {
        "webpage" => "网页",
        "journal_article" | "article" => "期刊论文",
        "book" => "书籍",
        "book_section" => "书籍章节",
        "conference_paper" => "会议论文",
        "thesis" => "学位论文",
        "report" => "报告",
        "preprint" => "预印本",
        "magazine_article" => "杂志文章",
        "newspaper_article" => "报纸文章",
        "blog_post" => "博客文章",
        "document" => "文档",
        "manuscript" => "手稿",
        "presentation" => "演示文稿",
        "video" => "视频",
        "podcast" => "播客",
        "pdf" => "PDF",
        "web_page" | "web_article" => "网页",
        "note" | "evernote_note" => "笔记",
        "attachment" => "附件",
        "letter" => "信件",
        "interview" => "访谈",
        "email" => "电子邮件",
        "forum_post" => "论坛帖子",
        "encyclopedia_article" => "百科条目",
        "dictionary_entry" => "词典条目",
        "patent" => "专利",
        "film" | "video_recording" => "视频",
        "audio_recording" => "音频",
        "artwork" => "艺术作品",
        "computer_program" => "软件",
        "dataset" => "数据集",
        "standard" => "标准",
        "map" => "地图",
        "statute" => "法规",
        "case" => "案例",
        "hearing" => "听证",
        "instant_message" => "即时消息",
        "tv_broadcast" | "radio_broadcast" => "广播节目",
        "social_media_post" | "tweet" => "社交媒体帖子",
        "other" => "其他类型",
        _ => return crate::relabel(item_type),
    };
    crate::tx(key).to_string()
}

/// Canonical and analysis projections of one item at one revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemDetail {
    pub canonical: CanonicalItem,
    pub analysis: TranslationItem,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Job {
    pub job_id: String,
    pub state: String,
    pub stage: String,
    pub updated_at: String,
    #[serde(default)]
    pub reason_code: Option<String>,
    #[serde(default)]
    pub item_id: Option<String>,
}

impl Job {
    pub fn can_retry(&self) -> bool {
        matches!(self.state.as_str(), "failed" | "cancelled")
    }
    pub fn can_cancel(&self) -> bool {
        matches!(self.state.as_str(), "queued" | "leased" | "artifact_ready" | "needs_user" | "retry_wait")
    }
    pub fn can_approve(&self) -> bool {
        self.state == "needs_user"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobAction {
    Retry,
    Cancel,
    Approve,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ItemUpdate {
    pub item_id: String,
    pub revision_id: String,
    #[serde(default)]
    pub previous_revision_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sensitivity {
    Public,
    Internal,
    Private,
    Restricted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextRequest {
    pub item_id: String,
    pub expected_revision_id: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub max_sensitivity: Sensitivity,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RevisionRef {
    pub item_id: String,
    pub revision_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ContextSlice {
    pub schema_version: u32,
    pub input: RevisionRef,
    pub origin: String,
    pub coordinate_space: String,
    pub content_sha256: String,
    pub total_bytes: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub content_markdown: String,
    #[serde(default)]
    pub source_url: Option<String>,
    #[serde(default)]
    pub canonical_url: Option<String>,
    pub sensitivity: Sensitivity,
}

/// Maximum context range accepted by the agent.
pub const MAX_CONTEXT_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LibraryPage {
    pub items: Vec<LibraryItem>,
    pub next_cursor: Option<String>,
}

/// Sort keys accepted by `library_query`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortKey {
    Title,
    Source,
    CapturedAt,
    ItemType,
    /// 2026-11-03 candidate: the canonical time the item entered the library;
    /// unknown values sort last. Not a captured-vs-imported filter.
    AddedAt,
}

/// Creation provenance from Store receipts, never inferred by the desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemOrigin {
    Capture,
    Import,
}

/// Query options; a view binds all of them plus the ceiling.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct QuerySpec {
    pub text: String,
    pub source: Option<String>,
    pub item_type: Option<String>,
    pub sort: SortKey,
    pub descending: bool,
    /// `groups_v1` draft 3: a folder scope. Absent from the wire when no folder is
    /// selected, so queries to agents without groups keep their current shape; when
    /// present, `include_descendants` is always sent explicitly.
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub group: Option<GroupScope>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<ItemOrigin>,
    /// Inbox: items in no visible library or folder (`groups_v1.query_unfiled`).
    /// Never on the wire when false, so agents without it see today's shape.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub unfiled: bool,
}

impl Default for QuerySpec {
    fn default() -> Self {
        Self { text: String::new(), source: None, item_type: None, sort: SortKey::CapturedAt, descending: true, group: None, origin: None, unfiled: false }
    }
}

/// `group_id` plus `include_descendants` (the backend selects the deduplicated
/// union of the node and its descendants; the client never merges child queries).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct GroupScope {
    pub group_id: String,
    pub include_descendants: bool,
}

/// One folder-tree node (`groups_v1` draft 3). Roots have `parent_id: None` and
/// use their own id as `root_group_id`. Counts are distinct items, never sums of
/// child counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupNode {
    pub id: String,
    pub name: String,
    /// `zotero_group | collection | user`.
    pub kind: String,
    pub parent_id: Option<String>,
    pub root_group_id: String,
    pub item_count: usize,
    pub subtree_item_count: usize,
    /// The server's effective write permission. Imported origin does not imply it.
    pub read_only: bool,
    /// Version token a `group_edit` on this node must name.
    pub version: String,
}

/// One `group_edit` operation (`groups_v1`). No move or delete exists (a
/// recoverable delete will come as its own preview/delete/restore commands).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum GroupEditOp {
    CreateRoot { name: String },
    CreateFolder { parent_id: String, name: String },
    Rename { id: String, name: String },
    ReorderRoot { group_id: String, before_id: Option<String> },
    AddMembers { group_id: String, item_ids: Vec<String> },
    RemoveMembers { group_id: String, item_ids: Vec<String> },
    /// Same-root folder-to-folder move in one write; only where
    /// `groups_v1.write_ops` lists `move_members`. The top-level version is the
    /// target's.
    MoveMembers { source_group_id: String, target_group_id: String, expected_source_version: String, item_ids: Vec<String> },
    /// Atomic folder-subtree transfer. Mapping creates independent directory
    /// structure with the same canonical items, not a synchronized alias.
    /// `expected_versions` freezes source subtrees and their ancestors; the
    /// outer expected_version belongs to the destination parent.
    TransferFolders { source_ids: Vec<String>, target_parent_id: String, expected_versions: std::collections::BTreeMap<String, String>, moving: bool },
}

/// `group_edit` reply's receipt (`group_edit: {schema_version, group_id, version}`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GroupEditReceipt {
    pub group_id: String,
    pub version: String,
}

/// Most distinct items one `add_members` / `remove_members` may name (the
/// Store's `MAX_GROUP_EDIT_ITEMS`); larger selections are sent in batches.
pub const MAX_GROUP_EDIT_ITEMS: usize = 256;

/// One page of `group_tree` (preorder). `view` binds later pages.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GroupTreePage {
    #[serde(default)]
    pub schema_version: u32,
    pub view: String,
    pub total: usize,
    pub offset: usize,
    #[serde(default)]
    pub next_offset: Option<usize>,
    pub nodes: Vec<GroupNodeView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GroupNodeView {
    pub id: String,
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub root_group_id: String,
    pub item_count: usize,
    pub subtree_item_count: usize,
    #[serde(default)]
    pub read_only: bool,
    /// Version token (`v3`) a write to this node must name.
    #[serde(default)]
    pub version: String,
}

impl From<GroupNodeView> for GroupNode {
    fn from(n: GroupNodeView) -> Self {
        Self { id: n.id, name: n.name, kind: n.kind, parent_id: n.parent_id, root_group_id: n.root_group_id, item_count: n.item_count, subtree_item_count: n.subtree_item_count, read_only: n.read_only, version: n.version }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Facet {
    pub value: String,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct QueryItem {
    pub item_id: String,
    pub revision_id: String,
    pub title: String,
    #[serde(default)]
    pub source: Option<String>,
    pub item_type: String,
    #[serde(default)]
    pub captured_at: Option<String>,
    /// Additive (2026-11-03 candidate): when the item entered the library (RFC 3339);
    /// absent when unknown, never a stand-in date.
    #[serde(default)]
    pub added_at: Option<String>,
    /// Additive (2026-11-03 candidate): Chinese display title; `title` stays the original.
    #[serde(default)]
    pub translated_title: Option<String>,
    /// With `translated_title`: `title_only` | `with_context`.
    #[serde(default)]
    pub title_source: Option<String>,
    /// Additive (2026-11-09): as on `LibraryItem`; omitted and null are unknown.
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub duration_seconds: Option<u32>,
    /// Author/year/container for the classic table. Not read from the agent wire:
    /// no list projection of the bibliographic fields exists yet, so only the
    /// synthetic fixture fills it and agent rows show "—".
    #[serde(skip)]
    pub bibliographic: Option<Bibliographic>,
}

impl QueryItem {
    /// Display label: the host of a URL origin, or the opaque identifier as given.
    pub fn source_label(&self) -> String {
        source_label(self.source.as_deref())
    }
}

/// Between a translated title and its original, as on the phone.
pub const TITLE_SEPARATOR: &str = " ｜ ";

/// `(translated, original)`: the translation leads when there is one that
/// differs from the original; otherwise the original alone.
pub fn title_parts<'a>(title: &'a str, translated: Option<&'a str>) -> (Option<&'a str>, &'a str) {
    let t = translated.map(str::trim).filter(|t| !t.is_empty() && *t != title.trim());
    (t, title)
}

/// One line: `译名 ｜ Original`, or the original alone.
pub fn display_title(title: &str, translated: Option<&str>) -> String {
    match title_parts(title, translated) {
        (Some(t), original) => format!("{t}{TITLE_SEPARATOR}{original}"),
        (None, original) => original.to_string(),
    }
}

/// A duration as `h:mm:ss`, or `mm:ss` under an hour; `None` when unknown.
pub fn duration_label(seconds: Option<u32>) -> Option<String> {
    let s = seconds?;
    let (h, m, s) = (s / 3600, s % 3600 / 60, s % 60);
    Some(if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m:02}:{s:02}") })
}

/// `发布 2026-10-30 · 1:00:26` (either part alone when the other is unknown);
/// `None` when both are.
pub fn published_line(published_at: Option<&str>, duration_seconds: Option<u32>) -> Option<String> {
    let date = published_at.map(|d| crate::txf("发布 {0}", &[&d]));
    match (date, duration_label(duration_seconds)) {
        (Some(d), Some(t)) => Some(format!("{d} · {t}")),
        (d, t) => d.or(t),
    }
}

/// Grouping label for items without a source. Rows and facets keep this value;
/// views show it through `source_display`.
pub const OTHER_SOURCE: &str = "其他";

/// A source label as shown: the "other" bucket in the interface language, any
/// real source unchanged.
pub fn source_display(label: &str) -> &str {
    if label == OTHER_SOURCE { crate::tx(OTHER_SOURCE) } else { label }
}

pub fn source_label(source: Option<&str>) -> String {
    match source {
        None | Some("") => OTHER_SOURCE.into(),
        Some(s) => s.split("://").nth(1).unwrap_or(s).trim_end_matches('/').to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct QueryWindow {
    pub schema_version: u32,
    pub generation: String,
    pub view: String,
    pub total: usize,
    pub offset: usize,
    #[serde(default)]
    pub next_offset: Option<usize>,
    pub items: Vec<QueryItem>,
    pub sources: Vec<Facet>,
    pub item_types: Vec<Facet>,
}

/// Maximum `library_query` window.
pub const MAX_QUERY_LIMIT: usize = 200;

/// A citation anchored in `revision_markdown_utf8_v1` coordinates of one revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Citation {
    pub label: String,
    pub item_id: String,
    pub revision_id: String,
    pub start_byte: usize,
    pub end_byte: usize,
}

// ---------------------------------------------------------------- model settings
// `interfaces/memex.model_settings.md`: non-secret configuration, separate
// credential commands, explicit discovery. Configuration is not execution.

/// A user-entered key on its way to the agent. Never logged: `Debug` is redacted,
/// and nothing in the desktop persists it.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: String) -> Self {
        Self(value)
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    Chat,
    Suggestions,
    Translation,
    Summary,
    Classification,
}

/// Reasoning effort values the desktop knows how to label. The wire field stays a
/// string so a value added by a newer agent is kept verbatim instead of being
/// dropped on save.
pub const REASONING_EFFORTS: [&str; 6] = ["none", "minimal", "low", "medium", "high", "xhigh"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provider {
    pub id: String,
    pub label: String,
    pub api_base: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelCard {
    pub id: String,
    pub label: String,
    pub provider_id: String,
    pub model_id: String,
}

// Configuration is saved back whole, so an unknown field or purpose from a newer
// agent fails the read closed instead of being silently dropped on the next save.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub purpose: Purpose,
    pub model: String,
    /// Absent means unspecified, and stays absent on save.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelConfiguration {
    pub providers: Vec<Provider>,
    pub models: Vec<ModelCard>,
    pub selections: Vec<Selection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ModelSettings {
    pub schema_version: u32,
    pub revision: u64,
    pub configuration: ModelConfiguration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialState {
    Missing,
    Saved,
    ScopeChanged,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ProviderCredential {
    pub settings_revision: u64,
    pub provider_id: String,
    pub api_base: String,
    pub generation: u64,
    pub state: CredentialState,
    #[serde(default)]
    pub masked_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum DiscoverAuthorization {
    SavedKey { expected_generation: u64 },
    Credentialless,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ProviderModels {
    pub schema_version: u32,
    pub settings_revision: u64,
    pub provider_id: String,
    pub api_base: String,
    #[serde(default)]
    pub credential_generation: Option<u64>,
    pub model_ids: Vec<String>,
}
