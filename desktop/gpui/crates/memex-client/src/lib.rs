//! Backend client for the MemeX desktop.
//!
//! The owner-only agent Unix socket is the only business seam. This crate owns the
//! framed transport, the request/response DTOs, the mapping of daemon reason codes to
//! user-facing states, and the request generation guard that keeps late responses from
//! crossing items. A clearly labelled synthetic fixture backend lets every UI module be
//! developed and verified without a real library.

pub mod active_group;
pub mod ai_marks;
pub mod biblio;
pub mod blocks;
pub mod capture_jobs;
pub mod book;
pub mod chat;
pub mod covers;
pub mod file_import;
pub mod format;
pub mod group_delete;
mod dto;
pub mod insights;
pub mod markdown;
pub mod images;
pub mod reading;
pub mod reader_state;
pub mod passes;
pub mod reading_stats;
pub mod trash;
pub mod typography;
pub mod opencc;
pub mod fixture;
mod fixture_biblio;
mod fixture_book;
mod fixture_ai_book;
mod fixture_covers;
mod fixture_import;
mod fixture_reader;
mod fixture_passes;
mod fixture_insights;
mod fixture_models;
pub mod i18n;
mod transport;

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

pub use biblio::{Bibliographic, Capabilities, Creator, DatePrecision, Identifier, PartialDate};
pub use chat::{ChatRole, ChatTurn, ItemChatReply, ItemChatRequest};
pub use insights::{AiInsights, InsightKind};
pub use dto::*;
pub use i18n::{relabel, tx, txf, Lang, LanguagePref};

/// Folder under ~/Library/Application Support that holds the agent socket; private builds set
/// MEMEX_DATA_DIR_NAME when they compile to keep an existing data directory.
pub const DATA_DIR_NAME: &str = match option_env!("MEMEX_DATA_DIR_NAME") { Some(name) => name, None => "LotsMemeX" };

/// Names the UI shows for the product and its assistant. Public builds show "Lots MemeX" and
/// "MemeX"; a private build sets MEMEX_BRAND_NAME and MEMEX_ASSISTANT_NAME when it compiles.
pub const BRAND_NAME: &str = match option_env!("MEMEX_BRAND_NAME") { Some(name) => name, None => "Lots MemeX" };
pub const ASSISTANT_NAME: &str = match option_env!("MEMEX_ASSISTANT_NAME") { Some(name) => name, None => "MemeX" };
pub use typography::{is_note_text, note_font, reading_family, reading_font, set_kai_family, set_note_font, set_reading_font, set_speaker_names, speaker_names, ItemStyle, NoteFont, ReadingFont, ReadingSize, SpeakerNames};
pub use transport::{is_safe_owner_socket, Framing, MAX_RESPONSE};

/// Errors a caller can act on. Daemon reason codes stay opaque strings except the
/// handful the UI must distinguish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientError {
    /// The agent socket is missing, refused the connection, or closed early.
    Unavailable,
    /// The socket exists but is not an owner-only `0600` socket of this user.
    UnsafeSocket,
    /// Response framing or JSON was invalid.
    InvalidFrame,
    /// The response exceeded the bounded frame size.
    TooLarge,
    /// The agent did not answer within the deadline.
    Timeout,
    /// The running agent does not implement this command (older daemon).
    Unsupported,
    /// The command reached the agent and it answered `ok=false`.
    Daemon(String),
}

impl ClientError {
    pub fn is_stale_view(&self) -> bool {
        matches!(self, ClientError::Daemon(code) if code == "stale_query_view")
    }

    /// The folder tree changed since it was read; refresh before editing again.
    pub fn is_stale_group(&self) -> bool {
        matches!(self, ClientError::Daemon(code) if code == "stale_group_view" || code == "stale_group_version")
    }

    /// The agent's query slot is busy (`library_loading`): never a sign of bad
    /// data. Reads were already retried; keep caches, drafts and request ids.
    pub fn is_busy(&self) -> bool {
        matches!(self, ClientError::Daemon(code) if code == "library_loading")
    }

    pub fn is_stale_revision(&self) -> bool {
        matches!(self, ClientError::Daemon(code) if code == "stale_item_revision")
    }

    /// The request may or may not have reached the agent: retrying the same save
    /// must reuse its request identity.
    pub fn is_transport(&self) -> bool {
        matches!(self, ClientError::Unavailable | ClientError::Timeout | ClientError::InvalidFrame | ClientError::TooLarge)
            || matches!(self, ClientError::Daemon(code) if code == "item_update_in_progress")
            // The agent contract does not say a busy reply proves a write was not admitted.
            || self.is_busy()
    }

    /// A field-scoped edit rejection: `(field, reason)` from
    /// `invalid_metadata_field:<field>[:<reason>]`, `readonly_metadata_field:<field>`,
    /// `unknown_metadata_field:<field>`, or `candidate_dependency_missing:<field>:<missing>`
    /// (reason `requires:<missing>`).
    pub fn field_error(&self) -> Option<(String, String)> {
        let ClientError::Daemon(code) = self else { return None };
        if let Some(rest) = code.strip_prefix("invalid_metadata_field:") {
            let (field, reason) = rest.split_once(':').unwrap_or((rest, "invalid"));
            return Some((field.into(), reason.into()));
        }
        if let Some(field) = code.strip_prefix("readonly_metadata_field:") {
            return Some((field.into(), "readonly".into()));
        }
        if let Some(field) = code.strip_prefix("unknown_metadata_field:") {
            return Some((field.into(), "unknown_field".into()));
        }
        let rest = code.strip_prefix("candidate_dependency_missing:")?;
        let (field, missing) = rest.split_once(':')?;
        Some((field.into(), format!("requires:{missing}")))
    }

    /// Short explanation shown in the UI, in the interface language.
    pub fn message(&self) -> String {
        match self {
            ClientError::Unavailable => tx("无法连接本机代理").into(),
            ClientError::UnsafeSocket => tx("代理套接字权限不安全，已拒绝连接").into(),
            ClientError::InvalidFrame => tx("代理返回的数据不完整或无法解析").into(),
            ClientError::TooLarge => tx("代理返回的数据过大").into(),
            ClientError::Timeout => tx("代理响应超时").into(),
            ClientError::Unsupported => tx("当前代理未提供此功能（可能是旧版本）").into(),
            ClientError::Daemon(code) => match code.as_str() {
                "library_loading" => tx("Mac 正忙，稍后重试").into(),
                "stale_item_revision" => tx("此条目已在别处更新，当前修改无法保存。请取消并重新载入。").into(),
                "item_not_found" | "library_resource_not_found" | "context_resource_not_found" => {
                    tx("找不到该条目，或当前权限不可见").into()
                }
                "stale_book_manifest" => tx("书的版本已变化，请刷新").into(),
                "stale_chapter_alignment" => tx("本章的段落对照已更新，请重新载入").into(),
                "invalid_reader_alignment_request" => tx("段落对照请求无效").into(),
                "reader_job_in_progress" => tx("这本书另有一章正在整理，请等它完成").into(),
                "reader_job_not_found" => tx("找不到这次整理任务").into(),
                "no_low_dial_provider" => tx("Mac 上还没有配置 Low dial 模型，无法整理").into(),
                "reader_repair_budget_exceeded" => tx("这一章超出了整理预算，部分段落仍未匹配").into(),
                "reader_repair_failed" => tx("整理没有成功").into(),
                "invalid_reader_anchor" => tx("选中的位置已失效，请重新选择").into(),
                "reader_annotation_limit_exceeded" => tx("这篇的标记已达上限").into(),
                "reader_annotation_not_found" => tx("这条标记已被删除").into(),
                "reader_annotations_unavailable" => tx("标记暂时读不出来").into(),
                "invalid_reader_edit" => tx("这一章的内容不能保存：不能为空、只有空白或含有不允许的字符").into(),
                "reader_edit_too_large" => tx("这一章超过 16 MiB，不能保存").into(),
                "reader_edit_not_found" => tx("找不到这次上传，请重新保存").into(),
                "reader_edit_upload_conflict" => tx("这次上传与先前的不一致，未保存。请重新保存").into(),
                "invalid_reader_translation_request" => tx("翻译请求无效").into(),
                "reader_translation_not_needed" => tx("这一章已有译文，不需要翻译").into(),
                "reader_translation_language_unsupported" => tx("只能翻译英文原文的章节").into(),
                "reader_translation_budget_exceeded" => tx("这一章太长，超出了单章翻译预算").into(),
                "reader_translation_quote_stale" => tx("确认已过期或设置已变化，请重新确认").into(),
                "reader_translation_unavailable" => tx("翻译模型暂不可用，或没有给出可用的结果").into(),
                "stale_reader_translation" => tx("AI 译文已变化，请重新载入").into(),
                "revision_text_unavailable" => tx("这个版本的文本不可读取：不在当前版本链上，或当前权限不可见").into(),
                "no_translation_provider" => tx("Mac 上还没有配置翻译模型").into(),
                "invalid_content_format_request" => tx("格式整理请求无效").into(),
                "content_format_unavailable" => tx("格式整理模型暂不可用，或没有给出可用的结果").into(),
                "content_format_preview_not_found" | "content_format_quote_stale" => tx("确认已过期或设置已变化，请重新确认").into(),
                "content_format_quote_consumed" => tx("这次确认已经用过，请重新预览").into(),
                "content_format_batch_not_found" => tx("找不到这次格式整理任务").into(),
                "content_format_changes_stale" => tx("修改记录已变化，请重新打开").into(),
                "content_format_restore_conflict" => tx("条目在整理后又被修改，没有退回；新的修改保留。").into(),
                "content_format_invalid_result" => tx("模型给出的整理结果没有通过检查，正文没有改动").into(),
                "metadata_unchanged" => tx("没有需要保存的修改").into(),
                "unsupported_metadata_schema_version" => tx("代理不支持此版本的书目字段").into(),
                "request_id_conflict" => tx("这次保存与先前的请求不一致，未写入。请重新载入后再保存").into(),
                "request_identity_conflict" => tx("这个请求编号已用于另一个问题，未发送。请重新提问").into(),
                "item_update_in_progress" => tx("条目正在别处保存，请稍后重试").into(),
                "item_revision_limit_exceeded" => tx("该条目的修订次数已达上限").into(),
                "preview_expired" => tx("补全预览已过期，请重新查询").into(),
                "preview_unavailable" | "invalid_lookup_preview" => tx("补全预览不可用，请重新查询").into(),
                "invalid_candidate_selection" => tx("所选补全项无效，请重新查询").into(),
                "lookup_rate_limited" => tx("查询过于频繁，已暂缓请求，请稍后再试").into(),
                "lookup_not_found" => tx("服务方没有找到这个标识符（这不代表标识符无效）").into(),
                "lookup_identifier_invalid" => tx("标识符未通过本地校验，未发出查询").into(),
                "lookup_identifier_unsupported" => tx("此类标识符不支持补全").into(),
                "lookup_identifier_not_on_item" | "lookup_identifier_mismatch" => tx("该标识符已不在此条目上，请重新载入").into(),
                "lookup_unavailable" => tx("暂时无法连接服务方，未生成预览").into(),
                "lookup_policy_rejected" | "lookup_redirect_rejected" => tx("服务方的响应不符合安全规则，已拒绝").into(),
                "lookup_response_too_large" => tx("服务方返回的数据过大，未生成预览").into(),
                "invalid_lookup_response" => tx("服务方返回的数据无法识别").into(),
                "lookup_preview_limit_exceeded" => tx("待处理的补全预览过多，请稍后再试").into(),
                "invalid_lookup_request" => tx("查询请求无效").into(),
                _ if self.field_error().is_some() => {
                    let (field, _) = self.field_error().unwrap_or_default();
                    txf("字段 {0} 的值未被接受", &[&field])
                }
                "invalid_metadata_update" => tx("标题或标签不符合要求").into(),
                "invalid_cursor" | "stale_query_view" => tx("列表已变化，请重新载入").into(),
                "stale_group_view" | "stale_group_version" => tx("分组已在别处变化，已刷新，请再试一次").into(),
                "group_read_only" => tx("这个分组是只读的").into(),
                "groups_unchanged" => tx("没有变化：名称相同，或条目已在这个分组中").into(),
                "invalid_group_edit" => tx("名称为空、过长或含有不允许的字符").into(),
                "group_not_found" => tx("找不到这个分组，或当前权限不可见").into(),
                "stale_model_settings" => tx("设置已在别处更新，请重新载入后再保存").into(),
                "stale_provider_credential" => tx("密钥状态已变化，请重新载入").into(),
                "invalid_model_settings" => tx("设置内容不符合要求（编号、地址或引用有误）").into(),
                "credential_store_unavailable" => tx("钥匙串暂不可用，密钥未更改").into(),
                "provider_unauthorized" => tx("服务方拒绝了该密钥").into(),
                "provider_redirect_refused" => tx("服务方要求重定向，已拒绝").into(),
                "provider_unavailable" => tx("无法连接服务方").into(),
                "provider_timeout" => tx("服务方响应超时").into(),
                "provider_response_too_large" | "result_too_large" => tx("服务方返回的列表过大").into(),
                "invalid_provider_response" => tx("服务方返回的列表无法识别").into(),
                "no_chat_provider" => tx("还没有配置对话模型").into(),
                "no_insight_provider" => tx("还没有配置分析模型").into(),
                "no_minutes_provider" => tx("还没有配置纪要模型").into(),
                "invalid_insight_request" => tx("分析请求无效").into(),
                "provider_outcome_unknown" => tx("回复状态未知，可重试").into(),
                "provider_failure" => tx("对话模型返回错误").into(),
                "invalid_chat_request" => tx("对话内容不符合要求（过长或格式有误）").into(),
                "item_not_trashed" => tx("条目不在回收站中（可能已被恢复）").into(),
                // Desktop-side: a not-found read of an item listed in the trash.
                "item_in_trash" => tx("这个条目在回收站中，恢复后才能阅读").into(),
                "item_already_trashed" => tx("条目已在回收站中").into(),
                "invalid_trash_request" => tx("回收站请求无效").into(),
                other => txf("代理错误：{0}", &[&other]),
            },
        }
    }
}

/// Where requests go.
pub trait Backend: Send + Sync + 'static {
    fn send(&self, command: &Command) -> Result<Response, ClientError>;
    fn mode(&self) -> BackendMode;
    /// A slow command with its own deadline (`item_chat`). Each call is its own
    /// connection, so it never holds up other requests.
    fn send_slow(&self, command: &Command, _timeout: Duration) -> Result<Response, ClientError> {
        self.send(command)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendMode {
    /// A real agent at this owner-only socket.
    Agent { socket: PathBuf },
    /// In-memory synthetic data for development; never a real library.
    SyntheticFixture,
}

impl BackendMode {
    pub fn is_fixture(&self) -> bool {
        matches!(self, BackendMode::SyntheticFixture)
    }
}

/// Real agent over the owner-only Unix socket.
pub struct AgentBackend {
    socket: PathBuf,
    timeout: Duration,
    busy_backoff: Vec<Duration>,
}

/// Pauses before each retry of a read the agent answered `library_loading`:
/// its single background query slot is busy (another read, an edit, an index
/// rebuild). Same schedule as the phone (mobile/web gateway-source.mjs).
pub const BUSY_BACKOFF_MS: [u64; 6] = [300, 600, 1200, 2400, 4000, 4000];

impl AgentBackend {
    pub fn new(socket: PathBuf) -> Self {
        Self { socket, timeout: Duration::from_secs(5), busy_backoff: BUSY_BACKOFF_MS.iter().map(|&ms| Duration::from_millis(ms)).collect() }
    }

    /// `MEMEX_AGENT_SOCKET`, else the packaged default under Application Support.
    pub fn from_environment() -> Self {
        let socket = std::env::var_os("MEMEX_AGENT_SOCKET")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
                home.join("Library/Application Support").join(DATA_DIR_NAME).join("agent.sock")
            });
        Self::new(socket)
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_busy_backoff(mut self, pauses: Vec<Duration>) -> Self {
        self.busy_backoff = pauses;
        self
    }
}

/// Reads that may be repeated after `library_loading`, unchanged (same revision,
/// hash and version): pure, side-effect-free reads only. Writes and
/// model-dispatching requests (`align_request`, `translate_request`, `edit_*`,
/// metadata and trash edits) never are, and neither are previews that may write
/// a quote or job (`translate_preview`, `format_preview`): a timed-out or busy
/// reply does not prove the daemon is not still executing them. The user
/// retries those (「重试」).
pub(crate) fn busy_retryable(command: &Command) -> bool {
    use book::ReaderRequest as R;
    matches!(command, Command::LibraryQuery { .. } | Command::GroupTreeRead { .. } | Command::ReadingAnnotationsRead { .. })
        || matches!(command, Command::Reader { request: R::Toc { .. } | R::Chunk { .. } | R::Alignment { .. } | R::AlignStatus { .. }
            | R::TranslateStatus { .. } | R::TranslateChunk { .. } | R::RevisionChunk { .. }, .. })
        || matches!(command, Command::ItemCoverBatch { .. } | Command::ItemCoverChunk { .. })
}

impl AgentBackend {
    fn send_within(&self, command: &Command, timeout: Duration) -> Result<Response, ClientError> {
        let request = serde_json::to_vec(command).map_err(|_| ClientError::InvalidFrame)?;
        let request_limit = if matches!(command, Command::MarkdownEdit { .. }) { 96 * 1024 * 1024 + 4096 } else { 192 * 1024 };
        let bytes = transport::send(&self.socket, &request, request_limit, MAX_RESPONSE, timeout)?;
        let response: Response =
            serde_json::from_slice(&bytes).map_err(|_| ClientError::InvalidFrame)?;
        interpret(command, response)
    }
}

impl Backend for AgentBackend {
    fn send(&self, command: &Command) -> Result<Response, ClientError> {
        // Only explicit busy replies to reads are repeated, a bounded number of
        // times. Each RPC keeps its own deadline; transport failures and writes
        // are never retried. Exhausted, the caller shows 「Mac 正忙，稍后重试」.
        let read = busy_retryable(command);
        let mut pauses = self.busy_backoff.iter();
        loop {
            let result = self.send_within(command, self.timeout);
            if read && result.as_ref().is_err_and(ClientError::is_busy) {
                if let Some(pause) = pauses.next() {
                    std::thread::sleep(*pause);
                    continue;
                }
            }
            return result;
        }
    }

    fn send_slow(&self, command: &Command, timeout: Duration) -> Result<Response, ClientError> {
        self.send_within(command, timeout.max(self.timeout))
    }

    fn mode(&self) -> BackendMode {
        BackendMode::Agent { socket: self.socket.clone() }
    }
}

/// Shared `ok=false` handling: commands newer than the 916 agent are reported as
/// unsupported when an older daemon rejects their shape, never retried as another read.
pub(crate) fn interpret(command: &Command, response: Response) -> Result<Response, ClientError> {
    if response.ok {
        return Ok(response);
    }
    let code = response.reason_code.unwrap_or_else(|| "internal_error".into());
    // The 916 agent answers commands it does not know with `invalid_request` or
    // `internal_error` (observed against the archived 916 build). For commands newer
    // than 916 neither code can be told apart from "not implemented", so both mean
    // unsupported here; the UI words this as "may be an older agent", never as fact.
    if command.is_extension() && !command.is_capability_gated() && (code == "invalid_request" || code == "internal_error") {
        return Err(ClientError::Unsupported);
    }
    Err(ClientError::Daemon(code))
}

/// Typed facade shared by all UI modules.
#[derive(Clone)]
pub struct Client {
    backend: Arc<dyn Backend>,
    /// Probed once per client; a transport failure is not remembered.
    capabilities: Arc<std::sync::Mutex<Option<Capabilities>>>,
}

impl Client {
    pub fn new(backend: impl Backend) -> Self {
        Self { backend: Arc::new(backend), capabilities: Default::default() }
    }

    pub fn mode(&self) -> BackendMode {
        self.backend.mode()
    }

    /// `capabilities`, cached. An agent that does not know the command (older
    /// than bibliographic v2) has none; the caller then never sends v2 commands.
    /// The agent's version; `Err(Unsupported)` from an agent without the command.
    pub fn agent_version(&self) -> Result<AgentVersion, ClientError> {
        self.backend.send(&Command::AgentVersion)?.agent_version.ok_or(ClientError::InvalidFrame)
    }

    pub fn capabilities(&self) -> Result<Capabilities, ClientError> {
        if let Some(c) = self.capabilities.lock().unwrap().clone() {
            return Ok(c);
        }
        let caps = match self.backend.send(&Command::Capabilities) {
            Ok(r) => r.capabilities.as_ref().map(Capabilities::from_agent).unwrap_or_default(),
            Err(ClientError::Unsupported) => Capabilities::default(),
            Err(e) => return Err(e),
        };
        *self.capabilities.lock().unwrap() = Some(caps.clone());
        Ok(caps)
    }

    /// Forget the probed capabilities (after a refresh, the agent may have changed).
    pub fn forget_capabilities(&self) {
        *self.capabilities.lock().unwrap() = None;
    }

    fn require_bibliographic(&self) -> Result<Capabilities, ClientError> {
        let caps = self.capabilities()?;
        if caps.bibliographic { Ok(caps) } else { Err(ClientError::Unsupported) }
    }

    /// Bibliographic fields of `item_id`, refused unless they belong to
    /// `revision_id` (the revision the editable record was loaded at).
    pub fn bibliographic(&self, item_id: &str, revision_id: &str, max_sensitivity: Sensitivity) -> Result<Bibliographic, ClientError> {
        self.require_bibliographic()?;
        let read = self
            .backend
            .send(&Command::BibliographicMetadataRead { item_id: item_id.into(), max_sensitivity })?
            .bibliographic_metadata
            .ok_or(ClientError::InvalidFrame)?;
        if read.metadata_schema_version != biblio::METADATA_SCHEMA_VERSION || read.item.item_id != item_id {
            return Err(ClientError::InvalidFrame);
        }
        if read.item.revision_id != revision_id {
            return Err(ClientError::Daemon("stale_item_revision".into()));
        }
        Ok(read.into())
    }

    /// One atomic bibliographic edit (or lookup apply). The receipt must be for
    /// the edited item and follow the expected revision.
    pub fn bibliographic_edit(&self, edit: biblio::MetadataEdit, max_sensitivity: Sensitivity) -> Result<ItemUpdate, ClientError> {
        self.require_bibliographic()?;
        let item_id = edit.item_id.clone();
        let expected = edit.expected_revision_id.clone();
        let receipt = self
            .backend
            .send(&Command::BibliographicMetadataEdit { max_sensitivity, edit })?
            .item_update
            .ok_or(ClientError::InvalidFrame)?;
        if receipt.item_id != item_id || receipt.previous_revision_id.as_deref().is_some_and(|p| p != expected) {
            return Err(ClientError::InvalidFrame);
        }
        Ok(receipt)
    }

    /// Other authorized items with the same normalized DOI/ISBN.
    pub fn identifier_duplicates(&self, identifiers: Vec<biblio::IdentifierQuery>, exclude_item_id: Option<String>, max_sensitivity: Sensitivity) -> Result<Vec<biblio::IdentifierDuplicate>, ClientError> {
        self.require_bibliographic()?;
        self.backend
            .send(&Command::BibliographicDuplicateCheck { identifiers, exclude_item_id, max_sensitivity })?
            .identifier_duplicates
            .ok_or(ClientError::InvalidFrame)
    }

    /// Explicit single-item lookup of one DOI/ISBN; only when the agent lists a
    /// source for that kind.
    pub fn lookup_preview(&self, item_id: &str, expected_revision_id: &str, kind: &str, value: &str, max_sensitivity: Sensitivity) -> Result<biblio::LookupPreview, ClientError> {
        if self.require_bibliographic()?.lookup_source(kind).is_none() {
            return Err(ClientError::Unsupported);
        }
        let preview = self
            .backend
            .send(&Command::BibliographicLookupPreview {
                item_id: item_id.into(),
                expected_revision_id: expected_revision_id.into(),
                identifier: biblio::IdentifierQuery { kind: kind.into(), value: value.into() },
                max_sensitivity,
            })?
            .lookup_preview
            .ok_or(ClientError::InvalidFrame)?;
        if preview.item_id != item_id || preview.expected_revision_id != expected_revision_id || preview.identifier.kind != kind {
            return Err(ClientError::InvalidFrame);
        }
        Ok(preview)
    }

    /// Every authorized folder node (all `group_tree_read` pages of one view);
    /// counts ignore any text query. Unsupported unless `groups_v1` is advertised.
    pub fn groups(&self) -> Result<Vec<GroupNode>, ClientError> {
        let caps = self.capabilities()?;
        if !caps.groups {
            return Err(ClientError::Unsupported);
        }
        let mut limit = caps.group_page_limit.max(1);
        let mut nodes = vec![];
        let mut view: Option<String> = None;
        let mut offset = 0;
        loop {
            let response = match self.backend.send(&Command::GroupTreeRead { max_sensitivity: Sensitivity::Restricted, offset, limit, expected_view: view.clone() }) {
                Err(ClientError::Daemon(code)) if code == "result_too_large" && limit > 1 => { limit = (limit / 2).max(1); continue; }
                Err(ClientError::TooLarge) if limit > 1 => { limit = (limit / 2).max(1); continue; }
                result => result?,
            };
            let page = response.group_tree.ok_or(ClientError::InvalidFrame)?;
            if page.offset != offset || view.as_ref().is_some_and(|v| v != &page.view) {
                return Err(ClientError::InvalidFrame);
            }
            view = Some(page.view);
            nodes.extend(page.nodes.into_iter().map(GroupNode::from));
            match page.next_offset {
                Some(next) if next > offset => offset = next,
                Some(_) => return Err(ClientError::InvalidFrame),
                None => return Ok(nodes),
            }
        }
    }

    /// One single-op `group_edit`. Refused without sending unless `groups_v1.writes`.
    /// Retrying after a transport error must reuse `request_id`.
    pub fn group_edit(&self, request_id: &str, expected_version: Option<&str>, op: GroupEditOp) -> Result<(), ClientError> {
        self.group_edit_receipt(request_id, expected_version, op).map(|_| ())
    }

    /// `group_edit`, returning the written node's version from the receipt
    /// (`None` when the agent sent no receipt). `move_members` is refused
    /// without sending unless `groups_v1.write_ops` lists it.
    pub fn group_edit_receipt(&self, request_id: &str, expected_version: Option<&str>, op: GroupEditOp) -> Result<Option<GroupEditReceipt>, ClientError> {
        let caps = self.capabilities()?;
        if !caps.group_writes || (matches!(op, GroupEditOp::ReorderRoot { .. }) && !caps.group_reorder) || (matches!(op, GroupEditOp::MoveMembers { .. }) && !caps.group_move_members)
            || (matches!(op, GroupEditOp::TransferFolders { .. }) && !caps.group_transfer_folders) {
            return Err(ClientError::Unsupported);
        }
        let transfer_target = match &op { GroupEditOp::TransferFolders { target_parent_id, .. } => Some(target_parent_id.clone()), _ => None };
        let response = self.backend.send(&Command::GroupEdit { max_sensitivity: Sensitivity::Restricted, request_id: request_id.into(), expected_version: expected_version.map(str::to_string), ops: vec![op] })?;
        if let Some(target) = transfer_target {
            // The durable receipt is the write result; the paginated tree is a
            // separate read. A missing or misbound receipt is never success.
            let receipt = response.group_edit.filter(|r| r.group_id == target && !r.version.is_empty()).ok_or(ClientError::InvalidFrame)?;
            return Ok(Some(receipt));
        }
        response.group_tree.ok_or(ClientError::InvalidFrame)?;
        Ok(response.group_edit)
    }

    /// Asks the agent to translate `item_id`. Never sent unless the agent
    /// advertises `translation_provider_v1` with `available: true`.
    pub fn translation_request(&self, item_id: &str) -> Result<TranslationStatus, ClientError> {
        if self.capabilities()?.translation != Some(true) {
            return Err(ClientError::Unsupported);
        }
        let response = self.backend.send(&Command::TranslationRequest { item_id: item_id.into(), max_sensitivity: Sensitivity::Restricted })?;
        let mut status = response.translation.ok_or(ClientError::InvalidFrame)?;
        if status.ai_minutes.is_none() {
            status.ai_minutes = response.ai_minutes;
        }
        Ok(status)
    }

    /// Asks for the AI 纪要 of `item_id`. Never sent unless the agent advertises
    /// `minutes_request_v1`; the reply carries the minutes' current state.
    pub fn minutes_request(&self, item_id: &str) -> Result<AiMinutes, ClientError> {
        if !self.capabilities()?.minutes_request {
            return Err(ClientError::Unsupported);
        }
        self.backend
            .send(&Command::MinutesRequest { item_id: item_id.into(), max_sensitivity: Sensitivity::Restricted })?
            .ai_minutes
            .ok_or(ClientError::InvalidFrame)
    }

    /// Asks for AI insights of `item_id`. Never sent unless the agent advertises
    /// `ai_insights_v1` with every requested kind.
    pub fn insight_request(&self, item_id: &str, kinds: &[InsightKind]) -> Result<AiInsights, ClientError> {
        let caps = self.capabilities()?;
        let Some(available) = caps.insights else { return Err(ClientError::Unsupported) };
        if kinds.is_empty() || !kinds.iter().all(|k| available.has(*k)) {
            return Err(ClientError::Unsupported);
        }
        self.backend
            .send(&Command::InsightRequest { item_id: item_id.into(), max_sensitivity: Sensitivity::Restricted, kinds: kinds.to_vec() })?
            .ai_insights
            .ok_or(ClientError::InvalidFrame)
    }

    /// One `item_chat` send, off the UI thread, with its own long deadline. Never
    /// sent unless the agent advertises `item_chat_v1`, or when the messages
    /// break the agent's limits. The reply must answer this request.
    pub fn item_chat(&self, request: &ItemChatRequest) -> Result<ItemChatReply, ClientError> {
        if !self.capabilities()?.item_chat {
            return Err(ClientError::Unsupported);
        }
        let id_ok = (16..=128).contains(&request.request_id.len()) && request.request_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        if !id_ok || !chat::valid_messages(&request.messages) {
            return Err(ClientError::Daemon("invalid_chat_request".into()));
        }
        let command = Command::ItemChat { item_id: request.item_id.clone(), max_sensitivity: Sensitivity::Restricted, request_id: request.request_id.clone(), messages: request.messages.clone() };
        let reply = self.backend.send_slow(&command, chat::ITEM_CHAT_TIMEOUT)?.item_chat.ok_or(ClientError::InvalidFrame)?;
        if reply.request_id != request.request_id {
            return Err(ClientError::InvalidFrame);
        }
        Ok(reply)
    }

    /// The item's whole stored conversation, oldest first, page by page
    /// (`item_chat_history_v1` only; blocking, call off the UI thread). Every page
    /// must be about this item and move forward. When turns are added while the
    /// pages are read (the version changes), the read starts over, at most 3 times.
    pub fn conversation_read(&self, item_id: &str) -> Result<Vec<chat::StoredTurn>, ClientError> {
        if !self.capabilities()?.item_chat_history {
            return Err(ClientError::Unsupported);
        }
        let mut attempt = 0;
        'read: loop {
            attempt += 1;
            let (mut turns, mut cursor, mut version) = (Vec::<chat::StoredTurn>::new(), None::<String>, None::<String>);
            for _ in 0..chat::MAX_CONVERSATION_PAGES {
                let command = Command::ConversationRead { item_id: item_id.into(), max_sensitivity: Sensitivity::Restricted, cursor: cursor.clone() };
                let page = self.backend.send(&command)?.conversation.ok_or(ClientError::InvalidFrame)?;
                let after = turns.last().map(|t| t.seq);
                let ordered = page.turns.windows(2).all(|w| w[0].seq < w[1].seq) && page.turns.first().is_none_or(|t| after.is_none_or(|a| t.seq > a));
                if page.item_id != item_id || !ordered {
                    return Err(ClientError::InvalidFrame);
                }
                match &version {
                    Some(v) if *v != page.version && attempt < 3 => continue 'read,
                    _ => version = Some(page.version.clone()),
                }
                turns.extend(page.turns);
                match page.next_cursor {
                    // The next page must continue after the last turn received.
                    Some(next) if next.parse::<i64>().ok() == turns.last().map(|t| t.seq) => cursor = Some(next),
                    Some(_) => return Err(ClientError::InvalidFrame),
                    None => return Ok(turns),
                }
            }
            return Ok(turns);
        }
    }

    /// One page of the recoverable trash. Never sent unless the agent advertises
    /// `dedup_trash_v1`.
    pub fn group_trash_all(&self, group_id: &str) -> Result<Vec<trash::TrashEntry>, ClientError> {
        if !self.capabilities()?.group_trash { return Err(ClientError::Unsupported); }
        let mut items = Vec::new();
        let mut cursor = None;
        loop {
            let response = self.backend.send(&Command::GroupTrashList { group_id: group_id.into(), cursor: cursor.clone(), max_sensitivity: Sensitivity::Restricted })?;
            items.extend(response.trash_items.ok_or(ClientError::InvalidFrame)?);
            match response.next_cursor {
                Some(next) if cursor.as_ref().is_none_or(|old| &next > old) => cursor = Some(next),
                Some(_) => return Err(ClientError::InvalidFrame),
                None => return Ok(items),
            }
        }
    }

    pub fn group_trash_edit(&self, group_id: &str, item_id: &str, revision: &str, request_id: &str, restore_plan: Option<&str>) -> Result<(), ClientError> {
        if !self.capabilities()?.group_trash { return Err(ClientError::Unsupported); }
        self.backend.send(&Command::GroupTrashEdit { group_id: group_id.into(), item_id: item_id.into(), expected_revision_id: revision.into(), request_id: request_id.into(), restore_plan: restore_plan.map(str::to_owned), max_sensitivity: Sensitivity::Restricted })?;
        Ok(())
    }

    pub fn trash_page(&self, cursor: Option<String>) -> Result<(Vec<trash::TrashEntry>, Option<String>), ClientError> {
        if !self.capabilities()?.trash {
            return Err(ClientError::Unsupported);
        }
        let response = self.backend.send(&Command::TrashList { cursor })?;
        Ok((response.trash_items.ok_or(ClientError::InvalidFrame)?, response.next_cursor))
    }

    /// The whole trash, page by page (at most `MAX_TRASH_PAGES`).
    pub fn trash_all(&self) -> Result<Vec<trash::TrashEntry>, ClientError> {
        let (mut items, mut cursor) = self.trash_page(None)?;
        for _ in 1..trash::MAX_TRASH_PAGES {
            let Some(next) = cursor.take() else { break };
            let (more, next) = self.trash_page(Some(next))?;
            items.extend(more);
            cursor = next;
        }
        Ok(items)
    }

    /// Puts one trashed item back into the library (`dedup_trash_v1` only).
    pub fn trash_restore(&self, item_id: &str) -> Result<trash::TrashRestore, ClientError> {
        if !self.capabilities()?.trash {
            return Err(ClientError::Unsupported);
        }
        let receipt = self.backend.send(&Command::ItemTrashRestore { item_id: item_id.into() })?.trash_restore.ok_or(ClientError::InvalidFrame)?;
        if receipt.item_id != item_id {
            return Err(ClientError::InvalidFrame);
        }
        Ok(receipt)
    }

    /// Read-only translation state with its reason (`scoped_translation_status`).
    pub fn translation_status(&self, item_id: &str) -> Result<TranslationStatus, ClientError> {
        self.backend
            .send(&Command::ScopedTranslationStatus { item_id: item_id.into(), max_sensitivity: Sensitivity::Restricted })?
            .translation
            .ok_or(ClientError::InvalidFrame)
    }


    pub fn library_page(&self, cursor: Option<String>) -> Result<LibraryPage, ClientError> {
        let response = self.backend.send(&Command::LibraryList { cursor })?;
        Ok(LibraryPage {
            items: response.library_items.unwrap_or_default(),
            next_cursor: response.next_cursor,
        })
    }

    pub fn item_read(&self, item_id: &str) -> Result<CanonicalItem, ClientError> {
        match self.backend.send(&Command::ItemRead { item_id: item_id.into() }) {
            Ok(response) => response.item.ok_or(ClientError::InvalidFrame),
            Err(ClientError::Daemon(code)) if code == "result_too_large" => self.segmented_item_read(item_id),
            Err(error) => Err(error),
        }
    }

    /// Analysis projection. A failed or unavailable translation gets its reason
    /// from `scoped_translation_status` for the same revision, when the agent has it.
    pub fn library_item_read(&self, item_id: &str) -> Result<TranslationItem, ClientError> {
        let mut item = self
            .backend
            .send(&Command::LibraryItemRead { item_id: item_id.into() })?
            .library_item
            .ok_or(ClientError::InvalidFrame)?;
        // A failed translation's reason and diagnostic, and a finished one's blocks
        // kept in the original, come from `scoped_translation_status` (newer agents
        // add the last two; older ones leave them absent).
        let failed = matches!(item.translation_state, AnalysisState::Failed | AnalysisState::Unavailable);
        if failed || item.translation_state == AnalysisState::Succeeded {
            if let Ok(status) = self.translation_status(item_id) {
                if status.revision_id.as_deref().is_none_or(|r| r == item.revision_id) {
                    if failed && item.translation_reason.is_none() {
                        item.translation_reason = status.reason;
                    }
                    item.translation_failure_detail = status.failure_detail.filter(|_| failed);
                    item.untranslated_blocks = status.untranslated_blocks.filter(|_| !failed);
                }
            }
        }
        Ok(item)
    }

    /// Reads canonical and analysis projections and refuses mixed revisions.
    pub fn item_detail(&self, item_id: &str) -> Result<ItemDetail, ClientError> {
        let canonical = self.item_read(item_id)?;
        let analysis = self.library_item_read(item_id)?;
        if canonical.item_id != item_id
            || analysis.item_id != item_id
            || canonical.revision_id != analysis.revision_id
        {
            return Err(ClientError::Daemon("stale_item_revision".into()));
        }
        Ok(ItemDetail { canonical, analysis })
    }

    pub fn update_metadata(
        &self,
        item_id: &str,
        expected_revision_id: &str,
        title: &str,
        manual_tags: &[String],
    ) -> Result<ItemUpdate, ClientError> {
        self.backend
            .send(&Command::UpdateItemMetadata {
                item_id: item_id.into(),
                expected_revision_id: expected_revision_id.into(),
                title: title.into(),
                manual_tags: manual_tags.to_vec(),
            })?
            .item_update
            .ok_or(ClientError::InvalidFrame)
    }

    pub fn jobs(&self) -> Result<Vec<Job>, ClientError> {
        Ok(self.backend.send(&Command::List { cursor: None })?.jobs.unwrap_or_default())
    }

    pub fn job_action(&self, action: JobAction, job_id: &str) -> Result<(), ClientError> {
        let job_id = job_id.to_string();
        let command = match action {
            JobAction::Retry => Command::Retry { job_id },
            JobAction::Cancel => Command::Cancel { job_id },
            JobAction::Approve => Command::Approve { job_id },
        };
        self.backend.send(&command).map(|_| ())
    }

    /// One window of a `library_query` view. Pass `view = None` only with offset 0.
    pub fn library_query(
        &self,
        query: &QuerySpec,
        max_sensitivity: Sensitivity,
        offset: usize,
        limit: usize,
        view: Option<String>,
    ) -> Result<QueryWindow, ClientError> {
        // `unfiled` is never sent to an agent that does not advertise it.
        if query.unfiled && !self.capabilities()?.group_query_unfiled {
            return Err(ClientError::Unsupported);
        }
        let window = self
            .backend
            .send(&Command::LibraryQuery {
                query: query.clone(),
                max_sensitivity,
                offset,
                limit: limit.clamp(1, MAX_QUERY_LIMIT),
                expected_view: view.clone(),
            })?
            .library_query
            .ok_or(ClientError::InvalidFrame)?;
        if window.offset != offset || view.is_some_and(|v| v != window.view) {
            return Err(ClientError::InvalidFrame);
        }
        Ok(window)
    }

    pub fn context_read(&self, request: ContextRequest) -> Result<ContextSlice, ClientError> {
        let item_id = request.item_id.clone();
        let revision = request.expected_revision_id.clone();
        let slice = self
            .backend
            .send(&Command::ContextRead {
                item_id: request.item_id,
                expected_revision_id: request.expected_revision_id,
                start_byte: request.start_byte,
                end_byte: request.end_byte,
                max_sensitivity: request.max_sensitivity,
            })?
            .context
            .ok_or(ClientError::InvalidFrame)?;
        // Never show text for a different anchor than the one requested.
        if slice.input.item_id != item_id
            || slice.input.revision_id != revision
            || slice.start_byte != request.start_byte
            || slice.end_byte != request.end_byte
        {
            return Err(ClientError::InvalidFrame);
        }
        Ok(slice)
    }
}

impl Client {
    pub fn model_settings(&self) -> Result<ModelSettings, ClientError> {
        self.backend.send(&Command::ModelSettingsRead)?.model_settings.ok_or(ClientError::InvalidFrame)
    }

    /// One compare-and-swap of the whole configuration.
    pub fn replace_model_settings(&self, expected_revision: u64, configuration: ModelConfiguration) -> Result<ModelSettings, ClientError> {
        self.backend
            .send(&Command::ModelSettingsReplace { expected_revision, configuration })?
            .model_settings
            .ok_or(ClientError::InvalidFrame)
    }

    pub fn provider_credential(&self, expected_revision: u64, provider_id: &str) -> Result<ProviderCredential, ClientError> {
        let c = self
            .backend
            .send(&Command::ProviderCredentialRead { expected_revision, provider_id: provider_id.into() })?
            .provider_credential
            .ok_or(ClientError::InvalidFrame)?;
        check_credential(c, provider_id)
    }

    pub fn replace_provider_credential(&self, expected_revision: u64, provider_id: &str, expected_generation: u64, api_key: Secret) -> Result<ProviderCredential, ClientError> {
        let c = self
            .backend
            .send(&Command::ProviderCredentialReplace { expected_revision, provider_id: provider_id.into(), expected_generation, api_key })?
            .provider_credential
            .ok_or(ClientError::InvalidFrame)?;
        check_credential(c, provider_id)
    }

    pub fn clear_provider_credential(&self, expected_revision: u64, provider_id: &str, expected_generation: u64) -> Result<ProviderCredential, ClientError> {
        let c = self
            .backend
            .send(&Command::ProviderCredentialClear { expected_revision, provider_id: provider_id.into(), expected_generation })?
            .provider_credential
            .ok_or(ClientError::InvalidFrame)?;
        check_credential(c, provider_id)
    }

    /// Explicit `GET models` by the agent. Advertised IDs only, never availability.
    pub fn discover_models(&self, expected_revision: u64, provider_id: &str, authorization: DiscoverAuthorization) -> Result<ProviderModels, ClientError> {
        let m = self
            .backend
            .send(&Command::ProviderModelsDiscover { expected_revision, provider_id: provider_id.into(), authorization })?
            .provider_models
            .ok_or(ClientError::InvalidFrame)?;
        if m.provider_id != provider_id || m.settings_revision != expected_revision {
            return Err(ClientError::InvalidFrame);
        }
        Ok(m)
    }
}

/// Answers for another provider are refused; a masked key never exposes more than
/// three characters.
fn check_credential(c: ProviderCredential, provider_id: &str) -> Result<ProviderCredential, ClientError> {
    let masked_ok = c.masked_key.as_deref().is_none_or(|m| m.trim_start_matches('*').chars().count() <= 3);
    if c.provider_id != provider_id || !masked_ok || (c.state != CredentialState::Saved && c.masked_key.is_some()) {
        return Err(ClientError::InvalidFrame);
    }
    Ok(c)
}

/// Monotonic generation counter. A view takes a ticket when it starts a request and
/// drops the response unless the ticket is still current; selecting another item,
/// refreshing or cancelling invalidates outstanding tickets.
#[derive(Debug, Default, Clone)]
pub struct Generation(Arc<AtomicU64>);

impl Generation {
    pub fn begin(&self) -> u64 {
        self.0.fetch_add(1, Ordering::SeqCst) + 1
    }
    pub fn invalidate(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
    pub fn is_current(&self, ticket: u64) -> bool {
        self.0.load(Ordering::SeqCst) == ticket
    }
}

#[cfg(test)]
mod tests;
