use crate::{
    bibliographic::LookupNetwork,
    config::{ensure_owner_directory, AgentConfig},
    coordinator::Coordinator,
    fetch::{ReqwestTransport, SystemResolver},
    provider_generation::{ProviderWorker, SavedProviderSource},
    queue::{JobView, Queue},
};
use anyhow::{bail, Result};
use memex_analysis::{
    GeneratedTextPurpose, SummaryJobState, SummaryQueue, TranslationJobState,
};
use memex_core::Sensitivity;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{FileTypeExt, MetadataExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
    time::Duration,
};
mod diagnostics;
mod query_worker;
mod frames;
mod reader;
pub use reader::ReaderRequest;
use query_worker::{dispatch_query, BackgroundQueries, QueryAccess};

pub const MAX_FRAME: usize = 64 * 1024;
/// Request frames: an `item_chat` carries up to 64 KiB of message content plus JSON escaping.
pub const MAX_REQUEST_FRAME: usize = 192 * 1024;
/// Only owner Markdown edits may carry a full 16 MiB revision (worst-case JSON escaping).
pub const MAX_MARKDOWN_EDIT_FRAME: usize = 96 * 1024 * 1024 + 4096;
pub const MAX_RESPONSE_FRAME: usize = 256 * 1024;
const MAX_MARKDOWN_BYTES: usize = 256 * 1024;
const MAX_LIBRARY_TRANSLATION_TEXT_BYTES: usize = 240 * 1024;
const MAX_LIBRARY_SUMMARY_TEXT_BYTES: usize = 8 * 1024;
const LIBRARY_PAGE_SIZE: usize = 50;
const TRANSLATION_RETRY_BASE_DELAY: Duration = Duration::from_secs(1);

#[cfg(test)]
thread_local! {
    static AFTER_HOOK_ABSENCE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}
/// A full reconcile pass runs at startup and then this often; between passes new captures and
/// IPC requests wake the workers directly, and a pass only enqueues revisions it has not seen.
const ANALYSIS_RECONCILE_INTERVAL: Duration = Duration::from_secs(600);
/// Longest an idle worker sleeps before it polls its queue again (enqueue paths in this process
/// wake it earlier through `notify_workers`).
const WORKER_IDLE_WAIT: Duration = Duration::from_secs(2);

static WORK_SIGNAL: (std::sync::Mutex<u64>, std::sync::Condvar) =
    (std::sync::Mutex::new(0), std::sync::Condvar::new());

/// Wakes idle workers after something may have been enqueued (an IPC request or a capture).
fn notify_workers() {
    let (count, signal) = &WORK_SIGNAL;
    if let Ok(mut count) = count.lock() {
        *count = count.wrapping_add(1);
        signal.notify_all();
    }
}

/// Blocks until `notify_workers` runs or `timeout` passes, without a busy loop.
fn wait_for_work(seen: &mut u64, timeout: Duration) {
    let (count, signal) = &WORK_SIGNAL;
    let Ok(guard) = count.lock() else {
        std::thread::sleep(timeout);
        return;
    };
    let guard = if *guard == *seen {
        match signal.wait_timeout(guard, timeout) {
            Ok((guard, _)) => guard,
            Err(_) => return,
        }
    } else {
        guard
    };
    *seen = *guard;
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Producer {
    LocalCli,
    LocalSurface,
    RemoteIphone,
}

impl Producer {
    fn as_str(self) -> &'static str {
        match self {
            Self::LocalCli => "local_cli",
            Self::LocalSurface => "local_surface",
            Self::RemoteIphone => "remote_iphone",
        }
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Enqueue {
        producer: Producer,
        producer_job_id: String,
        url: String,
    },
    List {
        cursor: Option<String>,
    },
    Status {
        job_id: String,
    },
    RemoteStatus {
        job_id: String,
    },
    ScopedCaptureEnqueue {
        principal: String,
        request_key: String,
        url: String,
        max_sensitivity: Sensitivity,
    },
    ScopedCaptureStatus {
        principal: String,
        job_id: String,
        max_sensitivity: Sensitivity,
    },
    ScopedTranslationStatus {
        item_id: String,
        max_sensitivity: Sensitivity,
    },
    ScopedTranslationRead {
        item_id: String,
        expected_revision_id: String,
        start_byte: usize,
        max_sensitivity: Sensitivity,
    },
    ItemRead {
        item_id: String,
    },
    RemoteItemRead {
        job_id: String,
        item_id: String,
    },
    LibraryList {
        cursor: Option<String>,
    },
    LibraryItemRead {
        item_id: String,
    },
    ItemCoverBatch {
        item_ids: Vec<String>,
        max_sensitivity: Sensitivity,
    },
    ItemCoverChunk {
        item_id: String,
        version: String,
        thumbnail: bool,
        offset: usize,
        max_sensitivity: Sensitivity,
    },
    ImageRead {
        item_id: String,
        expected_revision_id: String,
        target: String,
        offset: usize,
        max_sensitivity: Sensitivity,
    },
    ContextRead(memex_analysis::ContextReadRequest),
    LibraryQuery(memex_library::LibraryQueryRequest),
    ScopedItemRead {
        item_id: String,
        max_sensitivity: Sensitivity,
    },
    ScopedMetadataUpdate {
        item_id: String,
        max_sensitivity: Sensitivity,
        update: memex_store::ItemMetadataUpdate,
    },
    ModelSettingsRead,
    ModelSettingsReplace {
        expected_revision: u64,
        configuration: memex_models::Configuration,
    },
    ProviderCredentialRead {
        expected_revision: u64,
        provider_id: String,
    },
    ProviderCredentialReplace {
        expected_revision: u64,
        provider_id: String,
        expected_generation: u64,
        api_key: memex_provider::credentials::ApiKey,
    },
    ProviderCredentialClear {
        expected_revision: u64,
        provider_id: String,
        expected_generation: u64,
    },
    ProviderModelsDiscover {
        expected_revision: u64,
        provider_id: String,
        authorization: crate::models::DiscoveryAuthorization,
    },
    UpdateItemMetadata {
        item_id: String,
        expected_revision_id: String,
        title: String,
        manual_tags: Vec<String>,
    },
    /// Explicit owner click applying a (suggested) title; keeps manual tags and every
    /// derived result (same body). Idempotent: when the current title already equals
    /// `title`, nothing is written and the current revision is returned.
    ItemTitleApply {
        item_id: String,
        expected_revision_id: String,
        title: String,
        request_id: String,
    },
    /// One request with a fixed synthetic prompt through the saved credential.
    ModelGenerationTest {
        expected_revision: u64,
        purpose: memex_models::Purpose,
        expected_generation: u64,
    },
    /// Protocol discovery only; enqueues nothing and makes no request.
    Capabilities,
    /// Read-only build identity of this daemon (package version and executable digest).
    AgentVersion,
    BibliographicMetadataRead {
        item_id: String,
        max_sensitivity: Sensitivity,
    },
    BibliographicMetadataEdit {
        max_sensitivity: Sensitivity,
        edit: memex_store::ItemMetadataEditV2,
    },
    MarkdownEdit {
        max_sensitivity: Sensitivity,
        edit: memex_store::MarkdownEdit,
    },
    Reader {
        max_sensitivity: Sensitivity,
        #[serde(default)]
        principal: Option<String>,
        request: ReaderRequest,
    },
    ReadingAnnotationsRead {
        item_id: String,
        revision_id: String,
        max_sensitivity: Sensitivity,
    },
    ReadingAnnotationsSave {
        expected_version: String,
        annotations: memex_store::ReadingAnnotations,
        max_sensitivity: Sensitivity,
    },
    BibliographicDuplicateCheck {
        identifiers: Vec<memex_core::ItemIdentifier>,
        exclude_item_id: Option<String>,
        max_sensitivity: Sensitivity,
    },
    /// Explicit owner request: sends only this one item's DOI (Crossref) or
    /// ISBN (Open Library). No batch or background variant exists.
    BibliographicLookupPreview {
        item_id: String,
        expected_revision_id: String,
        identifier: memex_core::ItemIdentifier,
        max_sensitivity: Sensitivity,
    },
    /// Further pages of a frozen preview (no network, no write).
    BibliographicLookupPreviewRead {
        preview_id: String,
        max_sensitivity: Sensitivity,
        candidate_offset: usize,
        candidate_limit: usize,
    },
    UiActiveGroupSet {
        request_id: String,
        group_id: Option<String>,
        max_sensitivity: Sensitivity,
    },
    /// groups_v1 single-op write; `ops` must hold exactly one op. Returns the updated tree.
    GroupEdit {
        max_sensitivity: Sensitivity,
        request_id: String,
        /// Version token of the target node (the parent for create_folder); absent for create_root.
        #[serde(default)]
        expected_version: Option<String>,
        ops: Vec<crate::groups::GroupEditWireOp>,
    },
    /// Explicit owner request to translate (non-Chinese) and enrich metadata for one item's
    /// current revision. AI minutes are not authorized here (see `MinutesRequest`).
    TranslationRequest {
        item_id: String,
        max_sensitivity: Sensitivity,
    },
    /// Explicit owner request for AI minutes of one item's current revision (any language).
    MinutesRequest {
        item_id: String,
        max_sensitivity: Sensitivity,
    },
    /// Read-only AI minutes of one item's current revision under a sensitivity ceiling (the
    /// mobile gateway). Never authorizes, enqueues or calls a model (see `MinutesRequest`).
    ScopedMinutesStatus {
        item_id: String,
        max_sensitivity: Sensitivity,
    },
    /// Read-only AI insights of one item's current revision under a sensitivity ceiling (the
    /// mobile gateway). Never authorizes, enqueues or calls a model (see `InsightRequest`).
    ScopedInsightsStatus {
        item_id: String,
        max_sensitivity: Sensitivity,
    },
    /// Read-only stored `item_chat` conversation of one item under a sensitivity ceiling,
    /// oldest turn first; `cursor` is the previous page's `next_cursor`.
    ConversationRead {
        item_id: String,
        max_sensitivity: Sensitivity,
        #[serde(default)]
        cursor: Option<String>,
    },
    /// Owner only (never routed by the mobile gateway): stores turns restored from outside the
    /// daemon before the item's existing conversation (see `item_chat`).
    ConversationImport {
        item_id: String,
        origin: String,
        turns: Vec<crate::item_chat::ImportTurn>,
    },
    /// Owner only (never routed by the mobile gateway): title-only translation of up to
    /// `batch_size` existing items (e.g. bulk imports) that have no Chinese title yet.
    /// `dry_run` counts candidates and estimates tokens without queueing or sending anything.
    TitleTranslationBackfill {
        batch_size: usize,
        dry_run: bool,
    },
    /// Explicit owner request for AI insights (`highlights`, `quotes`, `fact_check`).
    InsightRequest {
        item_id: String,
        max_sensitivity: Sensitivity,
        kinds: Vec<String>,
    },
    /// One conversation turn about one item (served on its own thread).
    ItemChat {
        item_id: String,
        max_sensitivity: Sensitivity,
        request_id: String,
        messages: Vec<crate::item_chat::ChatMessage>,
    },
    /// Duplicate planning over visible items (served on its own thread). Model calls only with
    /// `use_model` (classification route), at most 200 per plan.
    DedupPlan {
        max_sensitivity: Sensitivity,
        use_model: bool,
    },
    /// Applies a persisted plan: re-validates revisions, merges tags/groups, trashes the rest.
    DedupApply {
        plan_id: String,
        expected_group_count: usize,
    },
    /// Recoverable trash: list (50 per page, ascending item id) and restore. No hard delete.
    TrashList {
        cursor: Option<String>,
    },
    ItemTrashRestore {
        item_id: String,
    },
    GroupDeletePreview {
        group_id: String,
        max_sensitivity: Sensitivity,
    },
    GroupDelete {
        group_id: String,
        confirmation_token: String,
        request_id: String,
        max_sensitivity: Sensitivity,
    },
    GroupDeleteList {
        cursor: Option<String>,
        max_sensitivity: Sensitivity,
    },
    GroupDeleteRestore {
        deletion_id: String,
        request_id: String,
        max_sensitivity: Sensitivity,
    },
    GroupTrashList {
        group_id: String,
        cursor: Option<String>,
        max_sensitivity: Sensitivity,
    },
    GroupTrashEdit {
        group_id: String,
        item_id: String,
        request_id: String,
        expected_revision_id: String,
        restore_plan: Option<String>,
        max_sensitivity: Sensitivity,
    },
    /// groups_v1 navigation tree.
    GroupTreeRead {
        max_sensitivity: Sensitivity,
        offset: usize,
        limit: usize,
        expected_view: Option<String>,
    },
    /// Window of one large candidate value (string bytes or array elements).
    BibliographicLookupValueRead {
        preview_id: String,
        candidate_id: String,
        side: memex_store::LookupValueSide,
        offset: usize,
        max_sensitivity: Sensitivity,
    },
    /// Read-only capture progress (capability `capture_jobs_recent_v1`); `limit` is clamped to 1..=100.
    CaptureJobsRecent {
        max_sensitivity: Sensitivity,
        limit: u32,
    },
    Retry {
        job_id: String,
    },
    Cancel {
        job_id: String,
    },
    Approve {
        job_id: String,
    },
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub ok: bool,
    pub job: Option<JobView>,
    pub jobs: Option<Vec<JobView>>,
    pub next_cursor: Option<String>,
    pub reason_code: Option<String>,
    #[serde(default)]
    pub item: Option<ItemResult>,
    #[serde(default)]
    pub library_items: Option<Vec<LibraryListItem>>,
    #[serde(default)]
    pub library_item: Option<LibraryItemResult>,
    #[serde(default)]
    pub item_update: Option<memex_store::ItemMetadataUpdateReceipt>,
    // Omit on old commands: older Rust consumers deny unknown response fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<memex_analysis::ContextSlice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_query: Option<memex_library::QueryPage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_entry: Option<memex_library::LibraryEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_settings: Option<memex_models::Snapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_credential: Option<memex_provider::credentials::CredentialStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_models: Option<memex_provider::DiscoveredModels>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translation: Option<memex_analysis::TranslationProjection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bibliographic_metadata: Option<memex_store::ItemMetadataV2Read>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identifier_duplicates: Option<Vec<memex_store::IdentifierDuplicate>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lookup_preview: Option<memex_store::LookupPreviewPage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lookup_value: Option<memex_store::LookupValuePage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_tree: Option<memex_store::GroupTreePage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_test: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<crate::bibliographic::AgentCapabilities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_edit: Option<memex_store::GroupEditReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_active_group: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_delete_preview: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_delete: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_delete_restore: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_deletions: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reading_annotations: Option<memex_store::ReadingAnnotations>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reader: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_minutes: Option<crate::translation_request::AiMinutes>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_chat: Option<crate::item_chat::ItemChatReply>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_insights: Option<crate::insights::AiInsights>,
    /// `title_translation_backfill` report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_backfill: Option<crate::translation_request::TitleBackfill>,
    /// `conversation_read` page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<crate::item_chat::Conversation>,
    /// `conversation_import` receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_import: Option<crate::item_chat::ConversationImportReceipt>,
    /// `agent_version` reply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_version: Option<AgentVersion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedup_plan: Option<crate::dedup::DedupPlan>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedup_apply: Option<crate::dedup::DedupApplyReport>,
    /// `trash_list` page (`next_cursor` carries the continuation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trash_items: Option<Vec<memex_store::TrashEntry>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trash_restore: Option<memex_store::TrashRestoreReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_cover: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_jobs: Option<crate::queue::recent::CaptureJobsRecent>,
}
/// `agent_version`: what is running, for the owner's "version" screens. No paths or secrets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentVersion {
    /// `memex-agent` package version.
    pub version: String,
    /// First 12 hex digits of the running executable's SHA-256 (null while computing or unreadable).
    pub binary_sha256: Option<String>,
    /// Lightweight query capability; no model credential lookup is required.
    #[serde(default)]
    pub query_origin: bool,
    #[serde(default)]
    pub group_reorder: bool,
    #[serde(default)]
    pub group_trash: bool,
}

fn agent_version() -> AgentVersion {
    static BINARY: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    static START: std::sync::Once = std::sync::Once::new();
    // Debug/large binaries can take longer than the whole IPC deadline to hash.
    // Version discovery must never stall the owner command loop or other reads.
    START.call_once(|| {
        std::thread::spawn(|| {
            let binary = (|| {
                use sha2::{Digest, Sha256};
                let bytes = fs::read(std::env::current_exe().ok()?).ok()?;
                Some(hex::encode(&Sha256::digest(&bytes)[..6]))
            })();
            let _ = BINARY.set(binary);
        });
    });
    AgentVersion {
        version: env!("CARGO_PKG_VERSION").into(),
        binary_sha256: BINARY.get().cloned().flatten(),
        query_origin: true,
        group_reorder: true,
        group_trash: true,
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemResult {
    pub item_id: String,
    pub revision_id: String,
    pub title: String,
    pub markdown: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_tags: Option<Vec<String>>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LibraryItemResult {
    pub item_id: String,
    pub revision_id: String,
    pub summary_state: LibrarySummaryState,
    pub summary: Option<String>,
    pub translation_state: LibraryTranslationState,
    pub title: Option<String>,
    pub markdown: Option<String>,
    /// AI minutes of the current revision (summary prompt version 2 only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_minutes: Option<crate::translation_request::AiMinutes>,
    /// AI insights of the current revision; item arrays are omitted when they would not fit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_insights: Option<crate::insights::AiInsights>,
    /// Additive: the translated title as soon as the title step succeeded, independent of the
    /// document (`title` stays success-only for compatibility).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translated_title: Option<String>,
    /// With `translated_title`: `with_context` (the full translation's title step) or
    /// `title_only` (the automatic capture or backfill title translation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_source: Option<String>,
    /// Additive: document batches finished so far while queued, running or failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translation_progress: Option<memex_analysis::TranslationProgressView>,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LibrarySummaryState {
    NotRequested,
    Queued,
    Running,
    Succeeded,
    Failed,
    Unavailable,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LibraryTranslationState {
    NotRequested,
    Queued,
    Running,
    Succeeded,
    Failed,
    Unavailable,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LibraryListItem {
    pub item_id: String,
    pub revision_id: String,
    pub title: String,
    pub item_type: String,
    pub captured_at: Option<String>,
    pub published_at: Option<String>,
    pub duration_seconds: Option<u32>,
    pub site_name: Option<String>,
}
impl Response {
    /// Successful response with every payload empty; feature modules fill theirs.
    pub(crate) fn empty_ok() -> Self {
        Self {
            ok: true,
            reason_code: None,
            ..Self::error("")
        }
    }
    fn error(code: &str) -> Self {
        Self {
            ok: false,
            job: None,
            jobs: None,
            next_cursor: None,
            reason_code: Some(sanitize(code)),
            item: None,
            library_items: None,
            library_item: None,
            item_update: None,
            context: None,
            library_query: None,
            library_entry: None,
            model_settings: None,
            provider_credential: None,
            provider_models: None,
            translation: None,
            bibliographic_metadata: None,
            identifier_duplicates: None,
            lookup_preview: None,
            lookup_value: None,
            group_tree: None,
            generation_test: None,
            capabilities: None,
            group_edit: None,
            ui_active_group: None,
            group_delete_preview: None,
            group_delete: None,
            group_delete_restore: None,
            group_deletions: None,
            reading_annotations: None,
            reader: None,
            ai_minutes: None,
            item_chat: None,
            ai_insights: None,
            title_backfill: None,
            conversation: None,
            conversation_import: None,
            agent_version: None,
            dedup_plan: None,
            dedup_apply: None,
            trash_items: None,
            trash_restore: None,
            image: None,
            item_cover: None,
            capture_jobs: None,
        }
    }
}
fn sanitize(s: &str) -> String {
    const OK: &[&str] = &[
        "invalid_cover_request",
        "invalid_cover_offset",
        "stale_cover_version",
        "cover_unavailable",
        "image_resource_not_found",
        "image_read_failed",
        "image_too_large",
        "invalid_image_range",
        "invalid_request",
        "no_translation_provider",
        "frame_too_large",
        "job_not_found",
        "request_identity_conflict",
        "permission_denied",
        "translation_unavailable",
        "job_not_retryable",
        "job_not_cancellable",
        "job_not_approvable",
        "invalid_cursor",
        "internal_error",
        "item_not_found",
        "result_too_large",
        "remote_resource_not_found",
        "library_resource_not_found",
        "library_scan_limit_exceeded",
        "invalid_cursor",
        "invalid_metadata_update",
        "invalid_markdown_edit",
        "invalid_reading_request",
        "invalid_reading_annotations",
        "stale_reading_annotations",
        "stale_reader_state",
        "invalid_reader_annotation",
        "stale_reader_annotations",
        "reader_annotation_conflict",
        "reader_annotation_limit_exceeded",
        "reader_annotation_not_found",
        "reader_annotations_unavailable",
        "invalid_reader_pass",
        "invalid_reader_pass_dates",
        "invalid_reader_pass_timing",
        "stale_reader_passes",
        "stale_reader_pass_timing",
        "reader_pass_conflict",
        "reader_pass_not_found",
        "reader_pass_limit_exceeded",
        "reader_passes_unavailable",
        "reader_pass_closed",
        "reader_pass_not_timeable",
        "reader_pass_session_expired",
        "reader_pass_clock_discontinuity",
        "reader_pass_segment_conflict",
        "invalid_reader_request",
        "revision_text_unavailable",
        "invalid_content_format_request",
        "content_format_unavailable",
        "content_format_preview_not_found",
        "content_format_quote_stale",
        "content_format_quote_consumed",
        "content_format_batch_not_found",
        "content_format_changes_stale",
        "content_format_restore_conflict",
        "content_format_invalid_result",
        "invalid_reader_edit",
        "reader_edit_not_found",
        "reader_edit_too_large",
        "reader_edit_upload_conflict",
        "invalid_reader_alignment_request",
        "stale_chapter_alignment",
        "reader_job_in_progress",
        "reader_job_not_found",
        "no_low_dial_provider",
        "reader_repair_budget_exceeded",
        "reader_repair_failed",
        "invalid_reader_translation_request",
        "reader_translation_not_needed",
        "reader_translation_language_unsupported",
        "reader_translation_budget_exceeded",
        "reader_translation_quote_stale",
        "reader_translation_unavailable",
        "stale_reader_translation",
        "no_translation_provider",
        "invalid_reader_anchor",
        "invalid_reader_state",
        "invalid_reader_offset",
        "invalid_reader_limit",
        "reader_side_not_found",
        "reader_chapter_not_found",
        "stale_book_manifest",
        "stale_item_revision",
        "metadata_unchanged",
        "item_revision_limit_exceeded",
        "invalid_context_request",
        "context_range_too_large",
        "context_resource_not_found",
        "invalid_context_range",
        "context_read_failed",
        "library_loading",
        "library_query_failed",
        "library_read_failed",
        "invalid_query_window",
        "stale_query_view",
        "invalid_model_settings",
        "unsupported_model_settings",
        "stale_model_settings",
        "invalid_provider_settings",
        "invalid_provider_credential",
        "credential_store_unavailable",
        "stale_provider_credential",
        "provider_credential_missing",
        "provider_credential_scope_mismatch",
        "provider_unauthorized",
        "provider_redirect_refused",
        "provider_unavailable",
        "invalid_generation_request",
        "generation_outcome_unknown",
        "generation_rejected_no_retry",
        "invalid_generation_response_no_retry",
        "generation_incomplete_no_retry",
        "generation_model_mismatch_no_retry",
        "provider_timeout",
        "provider_response_too_large",
        "invalid_provider_response",
        "unsupported_metadata_schema_version",
        "request_id_conflict",
        "item_update_in_progress",
        "preview_unavailable",
        "preview_expired",
        "invalid_lookup_preview",
        "invalid_candidate_selection",
        "invalid_identifier_query",
        "invalid_lookup_request",
        "lookup_identifier_invalid",
        "lookup_identifier_unsupported",
        "lookup_identifier_not_on_item",
        "lookup_identifier_mismatch",
        "lookup_not_found",
        "lookup_rate_limited",
        "lookup_unavailable",
        "lookup_policy_rejected",
        "lookup_redirect_rejected",
        "lookup_response_too_large",
        "invalid_lookup_response",
        "lookup_preview_limit_exceeded",
        "invalid_lookup_window",
        "invalid_group_window",
        "stale_group_view",
        "invalid_group_query",
        "invalid_group_edit",
        "stale_group_version",
        "invalid_group_delete_request",
        "group_delete_preview_not_found",
        "group_delete_preview_expired",
        "group_delete_preview_limit",
        "group_delete_unavailable",
        "stale_group_delete_preview",
        "stale_group_delete_view",
        "group_delete_restore_conflict",
        "group_read_only",
        "groups_unchanged",
        "group_limit_exceeded",
        "group_request_limit_exceeded",
        "no_chat_provider",
        "provider_outcome_unknown",
        "provider_failure",
        "invalid_chat_request",
        "no_insight_provider",
        "no_minutes_provider",
        "invalid_insight_request",
        "invalid_conversation_request",
        "invalid_conversation_import",
        "conversation_not_empty",
        "no_dedup_provider",
        "invalid_dedup_request",
        "dedup_plan_not_found",
        "dedup_group_count_mismatch",
        "item_not_trashed",
        "item_already_trashed",
        "item_trashed",
        "invalid_trash_request",
    ];
    // Field-scoped edit errors carry a field name and fixed reason code.
    const PREFIXES: &[&str] = &[
        "invalid_metadata_field:",
        "readonly_metadata_field:",
        "unknown_metadata_field:",
        "candidate_dependency_missing:",
    ];
    if PREFIXES.iter().any(|prefix| {
        s.strip_prefix(prefix).is_some_and(|rest| {
            !rest.is_empty()
                && rest.len() <= 128
                && rest
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b':')
        })
    }) {
        return s.to_owned();
    }
    OK.iter()
        .find(|x| **x == s)
        .unwrap_or(&"internal_error")
        .to_string()
}
pub fn send_command(socket: &Path, cmd: &Command) -> Result<Response> {
    let mut s = UnixStream::connect(socket)?;
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    s.set_write_timeout(Some(Duration::from_secs(5)))?;
    write_frame(&mut s, &zeroize::Zeroizing::new(serde_json::to_vec(cmd)?))?;
    Ok(serde_json::from_slice(&read_response_frame(&mut s)?)?)
}
pub fn run_daemon(config_path: &Path) -> Result<()> {
    run_daemon_with_sources(config_path, Vec::new())
}

/// The installed daemon with compile-time source handlers (see `crate::source`).
pub fn run_daemon_with_sources(
    config_path: &Path,
    sources: Vec<std::sync::Arc<dyn crate::source::SourceHandler>>,
) -> Result<()> {
    let transport = ReqwestTransport::with_source_images(&sources);
    run_daemon_inner(config_path, SystemResolver::default(), transport, DEFAULT_READER_STEPS,
        crate::queue::reader_translation::credential_generation,
        crate::queue::content_format::credential_generation, sources)
}

const DEFAULT_READER_STEPS: [ReaderStep; 3] = [
    |q, library| crate::queue::reader_repair::step(q, library, None),
    crate::queue::reader_translation::jobs::step,
    crate::queue::content_format::jobs::step,
];

/// The same UDS/coordinator pipeline with an explicit capture transport. The
/// installed daemon always selects the production transport above; examples
/// can exercise admission/Store without any network capability.
pub fn run_daemon_with_capture_transport<
    R: crate::fetch::Resolver + 'static,
    T: crate::fetch::HttpTransport + 'static,
>(
    config_path: &Path,
    resolver: R,
    transport: T,
) -> Result<()> {
    run_daemon_with_workers(config_path, resolver, transport, DEFAULT_READER_STEPS,
        crate::queue::reader_translation::credential_generation,
        crate::queue::content_format::credential_generation)
}

/// One reader executor step (claims at most one job of its own function).
pub type ReaderStep = fn(&mut Queue, &Path) -> Result<bool>;

/// Reader functions (repair, chapter translation, content format) each run on their own
/// thread under their own executor lock, so a long job of one function never queues
/// the others. Per-item uniqueness stays an admission rule (`reader_item_busy`).
fn spawn_reader_workers(steps: [ReaderStep; 3], library: PathBuf, database: PathBuf) {
    for step in steps {
        let (library, database) = (library.clone(), database.clone());
        std::thread::spawn(move || {
            let mut seen = 0;
            loop {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    Queue::open(&database).and_then(|mut q| step(&mut q, &library))
                }));
                if !matches!(result, Ok(Ok(true))) {
                    wait_for_work(&mut seen, WORKER_IDLE_WAIT);
                }
            }
        });
    }
}

/// Explicit dependency injection for synthetic process tests, never a config or
/// environment switch in the installed daemon.
pub fn run_daemon_with_workers<
    R: crate::fetch::Resolver + 'static,
    T: crate::fetch::HttpTransport + 'static,
>(config_path: &Path, resolver: R, transport: T,
    reader_steps: [ReaderStep; 3],
    translation_generation: fn(&Path, &memex_models::Snapshot) -> Result<u64>,
    format_generation: fn(&Path, &memex_models::Snapshot) -> Result<u64>,
) -> Result<()> {
    run_daemon_inner(config_path, resolver, transport, reader_steps, translation_generation,
        format_generation, Vec::new())
}

fn run_daemon_inner<
    R: crate::fetch::Resolver + 'static,
    T: crate::fetch::HttpTransport + 'static,
>(config_path: &Path, resolver: R, transport: T,
    reader_steps: [ReaderStep; 3],
    translation_generation: fn(&Path, &memex_models::Snapshot) -> Result<u64>,
    format_generation: fn(&Path, &memex_models::Snapshot) -> Result<u64>,
    sources: Vec<std::sync::Arc<dyn crate::source::SourceHandler>>,
) -> Result<()> {
    let cfg = AgentConfig::read(config_path)?;
    cfg.validate()?;
    ensure_owner_directory(&cfg.library)?;
    memex_store::recover_pending_ingests(&cfg.library)?;
    // AI minutes became manual-only: drop the automatic authorization of minutes jobs that
    // were queued under the old rule but never started (once per library).
    SummaryQueue::open(&cfg.library)?.deauthorize_unstarted_minutes_once()?;
    let mut queue = Queue::open(&cfg.database)?;
    if cfg.legacy_translation.is_some() {
        eprintln!("memex: ignoring retired `translation` block in config; generation uses model settings");
    }
    let mut queries = BackgroundQueries::new(cfg.library.clone())?;
    queries.translation_generation = translation_generation;
    queries.format_generation = format_generation;
    let lookup_network = LookupNetwork::production(cfg.metadata_lookup_contact.clone());
    queue.recover_leases()?;
    let source_index = std::sync::Arc::new(std::sync::Mutex::new(crate::coordinator::SourceIndex::default()));
    if cfg.intake.is_some() {
        // Cold verification precedes HTTP readiness and holds no queue write lock.
        // Both admission and worker then share incremental Store identities.
        memex_store::with_consistent_snapshot(&cfg.library, |root| {
            source_index.lock().map_err(|_| anyhow::anyhow!("source_index_unavailable"))?.refresh(root)
        })?;
    }
    if cfg.socket.exists() {
        let m = fs::symlink_metadata(&cfg.socket)?;
        if !m.file_type().is_socket() || m.uid() != unsafe { libc::geteuid() } {
            bail!("unsafe_socket_path")
        };
        match UnixStream::connect(&cfg.socket) {
            Ok(_) => bail!("daemon_already_running"),
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                fs::remove_file(&cfg.socket)?
            }
            Err(_) => bail!("socket_state_unknown"),
        }
    }
    let listener = bind_owner_socket(&cfg.socket)?;
    crate::queue::reader_repair::recover(&mut queue, &cfg.library)?;
    spawn_reader_workers(reader_steps, cfg.library.clone(), cfg.database.clone());
    let intake_status = if let Some(intake) = &cfg.intake {
        ensure_owner_directory(&intake.snapshots)?;
        let secret = memex_intake::read_pairing_secret(&intake.secret_file)?;
        let mut backend = crate::queue::ChromeIntake::new(cfg.database.clone(), cfg.library.clone())?;
        backend.source_index = source_index.clone();
        let server = memex_intake::IntakeServer::bind(
            &cfg.library,
            &intake.snapshots,
            secret,
            intake.origin.clone(),
            intake.port,
        )?.with_url_jobs(backend);
        let (result, status) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let _ = result.send(server.serve());
        });
        Some(status)
    } else {
        None
    };
    let library = cfg.library.clone();
    let translation_library = cfg.library.clone();
    let translation_database = cfg.database.clone();
    std::thread::spawn(move || run_translation_worker(translation_library, translation_database));
    let chat_source: std::sync::Arc<dyn crate::provider_generation::ProviderSource> =
        std::sync::Arc::new(SavedProviderSource {
            database: cfg.database.clone(),
        });
    listener.set_nonblocking(true)?;
    let _guard = SocketGuard(cfg.socket.clone());
    let incoming = frames::Incoming::new();
    let (worker_result, worker_status) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let result = (|| -> Result<()> {
            let mut coordinator = sources.into_iter().fold(
                Coordinator::new(cfg, resolver, transport)?,
                Coordinator::with_source_handler,
            );
            coordinator.source_index = source_index;
            coordinator.queue.recover_leases()?;
            loop {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    coordinator.process_one()
                }));
                match capture_step(result) {
                    CaptureStep::Progress => notify_workers(),
                    CaptureStep::LeaseLost => {}
                    CaptureStep::Idle | CaptureStep::Busy => std::thread::sleep(Duration::from_millis(100)),
                    CaptureStep::Restart(reason_code) => {
                        // Only this worker restarts; IPC, Intake and other workers keep
                        // running. An acquired lease stays durable and recovers on expiry.
                        eprintln!("memex: capture_worker_restarted reason_code={reason_code}");
                        std::thread::sleep(CAPTURE_RESTART_DELAY);
                        let _ = coordinator.queue.recover_leases();
                    }
                }
            }
        })();
        let _ = worker_result.send(result);
    });
    loop {
        if let Some(status) = &intake_status {
            match status.try_recv() {
                Ok(Ok(())) => bail!("intake_stopped"),
                Ok(Err(_)) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    bail!("intake_failed")
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        match worker_status.try_recv() {
            Ok(Ok(())) => bail!("worker_stopped"),
            Ok(Err(_)) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                bail!("worker_failed")
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
        match listener.accept() {
            Ok((stream, _)) => { let _ = incoming.admit(stream); }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e.into()),
        }
        if let Some(mut prepared) = incoming.next() {
            let _ = serve_prepared(&mut prepared.stream, &mut queue, &library, &mut queries,
                &lookup_network, &chat_source, prepared.command);
            notify_workers();
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

const CAPTURE_RESTART_DELAY: Duration = Duration::from_secs(1);

/// What the capture worker does after one `process_one` attempt.
#[derive(Debug, PartialEq, Eq)]
enum CaptureStep {
    Progress,
    Idle,
    LeaseLost,
    /// A competing SQLite writer: retry quietly, as before.
    Busy,
    /// Any other error or panic: record a sanitized reason code and restart this worker's
    /// loop. It never terminates the daemon (in-flight jobs elsewhere are unaffected).
    Restart(String),
}

fn capture_step(result: std::thread::Result<Result<bool>>) -> CaptureStep {
    match result {
        Ok(Ok(true)) => CaptureStep::Progress,
        Ok(Ok(false)) => CaptureStep::Idle,
        Ok(Err(error)) if error.to_string() == "lease_lost" => CaptureStep::LeaseLost,
        Ok(Err(error)) if matches!(error.downcast_ref::<rusqlite::Error>(),
            Some(rusqlite::Error::SqliteFailure(code, _))
                if matches!(code.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)) => {
            CaptureStep::Busy
        }
        Ok(Err(error)) => CaptureStep::Restart(capture_reason_code(&error.to_string())),
        Err(_) => CaptureStep::Restart("internal_error".into()),
    }
}

/// Only a bare fixed code (e.g. `fetch_worker_failed`) is logged; any message that could
/// carry a path, URL or other prose becomes `internal_error`.
fn capture_reason_code(message: &str) -> String {
    if (1..=64).contains(&message.len())
        && message.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        message.to_owned()
    } else {
        "internal_error".into()
    }
}

/// Provider jobs (translation, minutes, metadata, insights) in flight at once, across kinds and
/// items. Each worker leases one job at a time through the queues' IMMEDIATE transactions, with
/// its own SQLite connections; running jobs keep their lease renewed, so no worker recovers a
/// job another one is still sending.
const PROVIDER_WORKERS: usize = 6;

fn run_translation_worker(library: PathBuf, database: PathBuf) -> ! {
    for _ in 0..PROVIDER_WORKERS {
        let (library, database) = (library.clone(), database.clone());
        std::thread::spawn(move || run_provider_worker(library, database));
    }
    let mut reconciler = AnalysisReconciler::new(ANALYSIS_RECONCILE_INTERVAL);
    let mut seen = 0;
    loop {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            translation_worker_step(&library, &database, None, &mut reconciler)
        }));
        match result {
            Ok(Ok(true)) => notify_workers(),
            Ok(Ok(false)) => wait_for_work(&mut seen, WORKER_IDLE_WAIT),
            Ok(Err(_)) | Err(_) => std::thread::sleep(Duration::from_secs(1)),
        }
    }
}

/// The only generation path; it only ever leases explicitly authorized jobs.
fn run_provider_worker(library: PathBuf, database: PathBuf) -> ! {
    let mut provider = ProviderWorker::new(Box::new(SavedProviderSource { database }));
    let mut seen = 0;
    loop {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            provider.step(&library, TRANSLATION_RETRY_BASE_DELAY)
        }));
        match result {
            Ok(Ok(true)) => {}
            Ok(Ok(false)) => wait_for_work(&mut seen, WORKER_IDLE_WAIT),
            Ok(Err(_)) | Err(_) => std::thread::sleep(Duration::from_secs(1)),
        }
    }
}

fn translation_worker_step(
    library: &Path,
    database: &Path,
    provider: Option<&mut ProviderWorker>,
    reconciler: &mut AnalysisReconciler,
) -> Result<bool> {
    let mut capture_queue = Queue::open(database)?;
    let mut hook_progress = false;
    let mut hook_deferred = false;
    let mut hook_defer_failed = false;
    if let Some(intent) = capture_queue.pending_translation_intent()? {
        match memex_analysis::enqueue_item_translation(
            library,
            &intent.item_id,
            &intent.revision_id,
        )
        .and_then(|_| {
            let summary = memex_analysis::enqueue_item_summary(
                library,
                &intent.item_id,
                &intent.revision_id,
            )?;
            // A capture this daemon just stored is provider-eligible for its TITLE only (one
            // title-only translation) and metadata; the body is translated only on an explicit
            // `translation_request`, which also re-translates the title with context. Background
            // reconciliation and bulk imports are never eligible. AI minutes are manual only
            // (`minutes_request`): the summary job is created but not authorized. Chinese
            // titles get no title call, but they do get metadata enrichment.
            memex_analysis::TitleTranslationQueue::open(library)?
                .request_capture(&intent.item_id, &intent.revision_id)?;
            if matches!(
                summary,
                memex_analysis::SummaryEnqueueOutcome::Enqueued { .. }
            ) {
                // Only beside eligible text (same classifier gate as the summary).
                memex_analysis::MetadataQueue::open(library)?
                    .request(&intent.item_id, &intent.revision_id)?;
            }
            Ok(())
        })
        .and_then(|_| capture_queue.complete_translation_intent(&intent))
        {
            Ok(()) => hook_progress = true,
            Err(_) => {
                hook_deferred = true;
                hook_defer_failed = capture_queue.defer_translation_intent(&intent).is_err();
            }
        }
    }
    // Store is the durable source of truth for every current revision, including direct
    // loopback-intake captures and owner metadata edits that intentionally return success after
    // their Store commit. This bounded pass closes a transient post-Store queue failure without
    // introducing a second item database or turning a successful write into an ambiguous retry.
    let reconciliation = reconciler.process_next_page(library);
    // The only generation path; it only ever leases explicitly authorized jobs.
    let provider_progress = match provider {
        Some(worker) => worker.step(library, TRANSLATION_RETRY_BASE_DELAY),
        None => Ok(false),
    };
    if hook_progress
        || reconciliation.as_ref().is_ok_and(|progress| *progress)
        || provider_progress.as_ref().is_ok_and(|progress| *progress)
    {
        return Ok(true);
    }
    reconciliation?;
    provider_progress?;
    if hook_defer_failed {
        bail!("translation_hook_defer_failed")
    }
    if hook_deferred {
        bail!("translation_hook_deferred")
    }
    Ok(false)
}

struct AnalysisReconciler {
    cursor: Option<String>,
    next_scan: std::time::Instant,
    interval: Duration,
    /// Revision already reconciled per item in this process; later passes skip it, so a
    /// periodic pass costs one metadata read per item and no queue work when nothing changed.
    reconciled: std::collections::HashMap<String, String>,
    quality: memex_analysis::capture_quality::CaptureQualityReconciler,
}

impl AnalysisReconciler {
    fn new(interval: Duration) -> Self {
        Self {
            cursor: None,
            next_scan: std::time::Instant::now(),
            interval,
            reconciled: Default::default(),
            quality: Default::default(),
        }
    }

    fn process_next_page(&mut self, library: &Path) -> Result<bool> {
        let quality_progress = self.quality.process_next(library)?;
        if std::time::Instant::now() < self.next_scan {
            return Ok(quality_progress);
        }
        let (items, next_cursor) = memex_store::page_all_item_metadata(
            library,
            self.cursor.as_deref(),
            LIBRARY_PAGE_SIZE,
        )?;
        let mut enqueued = false;
        for item in &items {
            if self.reconciled.get(&item.item_id) == Some(&item.revision_id) {
                continue;
            }
            memex_analysis::enqueue_item_translation(library, &item.item_id, &item.revision_id)?;
            memex_analysis::enqueue_item_summary(library, &item.item_id, &item.revision_id)?;
            self.reconciled
                .insert(item.item_id.clone(), item.revision_id.clone());
            enqueued = true;
        }
        self.cursor = next_cursor;
        if self.cursor.is_none() {
            self.next_scan = std::time::Instant::now() + self.interval;
        }
        Ok(quality_progress || enqueued || self.cursor.is_some())
    }
}

#[cfg(test)]
fn process_translation_queue_step<M: memex_analysis::TranslationModel>(
    library: &Path,
    model: &mut M,
    retry_base_delay: Duration,
) -> Result<bool> {
    let mut queue = memex_analysis::TranslationQueue::open(library)?;
    if queue.process_next(model)? {
        return Ok(true);
    }
    queue.retry_one_eligible(retry_base_delay)
}

fn bind_owner_socket(path: &Path) -> Result<UnixListener> {
    if let Some(p) = path.parent() {
        ensure_owner_directory(p)?;
        let metadata = fs::symlink_metadata(p)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            bail!("unsafe_socket_directory")
        }
    }
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_socket()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o177 != 0
    {
        bail!("unsafe_socket_path")
    }
    Ok(listener)
}
struct SocketGuard(std::path::PathBuf);
impl Drop for SocketGuard {
    fn drop(&mut self) {
        if let Ok(m) = fs::symlink_metadata(&self.0) {
            if m.file_type().is_socket() && m.uid() == unsafe { libc::geteuid() } {
                let _ = fs::remove_file(&self.0);
            }
        }
    }
}
#[cfg(test)]
fn serve(
    s: &mut UnixStream,
    q: &mut Queue,
    library: &Path,
    queries: &mut memex_library::LibraryQuery,
    lookup: &LookupNetwork,
) -> Result<()> {
    let chat: std::sync::Arc<dyn crate::provider_generation::ProviderSource> =
        std::sync::Arc::new(SavedProviderSource {
            database: q.database_path().to_owned(),
        });
    serve_with_chat(s, q, library, queries, lookup, &chat)
}

/// Serves one connection. Query/index work and model calls run off the listener.
/// Other commands run on the (serial) listener thread except
/// `item_chat`, whose model call can take up to 120 s: it is answered from its own thread with
/// its own Store reads and its own settings/Keychain resolution, so it shares no SQLite
/// connection with the listener and never blocks `library_query` and friends.
#[cfg(test)]
fn serve_with_chat<'a>(
    s: &mut UnixStream,
    q: &mut Queue,
    library: &Path,
    queries: impl Into<QueryAccess<'a>>,
    lookup: &LookupNetwork,
    chat: &std::sync::Arc<dyn crate::provider_generation::ProviderSource>,
) -> Result<()> {
    let command = frames::read_command(s);
    serve_prepared(s, q, library, queries, lookup, chat, command)
}

fn serve_prepared<'a>(
    s: &mut UnixStream, q: &mut Queue, library: &Path,
    queries: impl Into<QueryAccess<'a>>, lookup: &LookupNetwork,
    chat: &std::sync::Arc<dyn crate::provider_generation::ProviderSource>, command: Result<Command>,
) -> Result<()> {
    let mut queries = queries.into();
    let response = match command {
        Ok(Command::ItemChat {
            item_id,
            max_sensitivity,
            request_id,
            messages,
        }) => {
            let mut stream = s.try_clone()?;
            let library = library.to_owned();
            let database = q.database_path().to_owned();
            let chat = chat.clone();
            std::thread::spawn(move || {
                let response = item_chat_response(
                    &library,
                    &database,
                    chat.as_ref(),
                    &item_id,
                    max_sensitivity,
                    &request_id,
                    &messages,
                );
                if let Ok(body) = response_body(&response) {
                    let _ = write_response_frame(&mut stream, &body);
                }
            });
            return Ok(());
        }
        // Whole-library scans, model judgments and many CAS writes: off the listener thread.
        Ok(command @ (Command::DedupPlan { .. } | Command::DedupApply { .. })) => {
            let mut stream = s.try_clone()?;
            let library = library.to_owned();
            let database = q.database_path().to_owned();
            let source = chat.clone();
            std::thread::spawn(move || {
                let response = dedup_response(&library, &database, source.as_ref(), command);
                if let Ok(body) = response_body(&response) {
                    let _ = write_response_frame(&mut stream, &body);
                }
            });
            return Ok(());
        }
        Ok(c) => {
            if matches!(&c, Command::LibraryQuery(_) | Command::GroupTreeRead { .. } | Command::GroupEdit { .. }
                | Command::GroupDeletePreview { .. } | Command::GroupDelete { .. }
                | Command::GroupDeleteList { .. } | Command::GroupDeleteRestore { .. }
                | Command::ItemCoverBatch { .. } | Command::ItemCoverChunk { .. }
                | Command::MarkdownEdit { .. } | Command::ReadingAnnotationsRead { .. } | Command::ReadingAnnotationsSave { .. } | Command::Reader { .. }) {
                if let QueryAccess::Background(worker) = &mut queries {
                    return worker.serve(s.try_clone()?, c, q.database_path());
                }
            }
            let inline = match &mut queries {
                QueryAccess::Inline(query) => Some(&mut **query),
                QueryAccess::Background(_) => None,
            };
            let operation = diagnostics::Operation::for_command(&c);
            dispatch(q, library, inline, lookup, c)
                .unwrap_or_else(|e| diagnostics::error_response_for_operation(library, operation, &e))
        },
        Err(e) => Response::error(&e.to_string()),
    };
    write_response_frame(s, &response_body(&response)?)
}

fn dedup_response(
    library: &Path,
    database: &Path,
    source: &dyn crate::provider_generation::ProviderSource,
    command: Command,
) -> Response {
    let mut response = Response::empty_ok();
    let result = match command {
        Command::DedupPlan {
            max_sensitivity,
            use_model,
        } => crate::dedup::plan(library, database, source, max_sensitivity, use_model)
            .map(|plan| response.dedup_plan = Some(plan)),
        Command::DedupApply {
            plan_id,
            expected_group_count,
        } => crate::dedup::apply(library, database, &plan_id, expected_group_count)
            .map(|report| response.dedup_apply = Some(report)),
        _ => Err(anyhow::anyhow!("invalid_request")),
    };
    match result {
        Ok(()) => response,
        Err(error) => Response::error(&error.to_string()),
    }
}

fn item_chat_response(
    library: &Path,
    database: &Path,
    source: &dyn crate::provider_generation::ProviderSource,
    item_id: &str,
    max_sensitivity: Sensitivity,
    request_id: &str,
    messages: &[crate::item_chat::ChatMessage],
) -> Response {
    match crate::item_chat::handle(
        library,
        database,
        source,
        item_id,
        max_sensitivity,
        request_id,
        messages,
    ) {
        Ok(reply) => {
            let mut response = Response::empty_ok();
            response.item_chat = Some(reply);
            response
        }
        Err(code) => Response::error(code),
    }
}
#[cfg(target_os = "linux")]
fn peer_uid(s: &UnixStream) -> Result<u32> {
    let mut c = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut n = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            s.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut c as *mut _ as *mut _,
            &mut n,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(c.uid)
}
#[cfg(target_os = "macos")]
fn peer_uid(s: &UnixStream) -> Result<u32> {
    let (mut uid, mut gid) = (0, 0);
    let rc = unsafe { libc::getpeereid(s.as_raw_fd(), &mut uid, &mut gid) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(uid)
}
fn dispatch_cover(library: &Path, command: Command) -> Result<Response> {
    use base64::Engine;
    let value = match command {
        Command::ItemCoverBatch { item_ids, max_sensitivity } => {
            serde_json::to_value(memex_store::covers::cover_batch(library, &item_ids, max_sensitivity)?)?
        }
        Command::ItemCoverChunk { item_id, version, thumbnail, offset, max_sensitivity } => {
            let chunk = memex_store::covers::cover_chunk(library, &item_id, &version, thumbnail, offset, max_sensitivity)?;
            serde_json::json!({
                "media_type": chunk.media_type, "sha256": chunk.sha256,
                "total_bytes": chunk.total_bytes, "offset": chunk.offset, "next_offset": chunk.next_offset,
                "data_base64": base64::engine::general_purpose::STANDARD.encode(&chunk.bytes),
            })
        }
        _ => bail!("invalid_request"),
    };
    let mut response = Response::empty_ok();
    response.item_cover = Some(value);
    Ok(response)
}

fn dispatch(
    q: &mut Queue,
    library: &Path,
    queries: Option<&mut memex_library::LibraryQuery>,
    lookup: &LookupNetwork,
    c: Command,
) -> Result<Response> {
    // Feature modules own their commands; the core match below never sees them.
    let c = match reader::dispatch_job(q, library, c, crate::queue::reader_translation::credential_generation,
        crate::queue::content_format::credential_generation)? {
        Ok(response) => return Ok(response),
        Err(c) => c,
    };
    let c = match crate::bibliographic::dispatch(q, library, lookup, c)? {
        Ok(response) => return Ok(response),
        Err(c) => c,
    };
    let c = match queries {
        Some(queries) => match dispatch_query(library, queries, c)? {
            Ok(response) => return Ok(response),
            Err(c) => c,
        },
        None => c,
    };
    let c = match crate::translation_request::dispatch(q, library, c)? {
        Ok(response) => return Ok(response),
        Err(c) => c,
    };
    let c = match crate::insights::dispatch(q, library, c)? {
        Ok(response) => return Ok(response),
        Err(c) => c,
    };
    let c = match crate::item_chat::dispatch(q.database_path(), library, c)? {
        Ok(response) => return Ok(response),
        Err(c) => c,
    };
    let c = match crate::model_test::dispatch(q, c)? {
        Ok(response) => return Ok(response),
        Err(c) => c,
    };
    let mut capabilities = None;
    let mut context = None;
    let library_query = None;
    let mut library_entry = None;
    let mut model_settings = None;
    let mut provider_credential = None;
    let mut provider_models = None;
    let mut translation = None;
    let (job, jobs, next_cursor, item, library_items, library_item, item_update) = match c {
        Command::UiActiveGroupSet { request_id, group_id, max_sensitivity } => {
            let mut response = Response::empty_ok();
            response.ui_active_group = Some(q.set_active_group(library, &request_id, group_id.as_deref(), max_sensitivity)?);
            return Ok(response);
        }
        Command::ScopedCaptureEnqueue {
            principal,
            request_key,
            url,
            max_sensitivity,
        } => {
            if max_sensitivity < Sensitivity::Internal {
                bail!("permission_denied");
            }
            let job = q.scoped_enqueue(&principal, &request_key, &url)?;
            check_capture_result(library, &job, max_sensitivity)?;
            (Some(job), None, None, None, None, None, None)
        }
        Command::ScopedCaptureStatus {
            principal,
            job_id,
            max_sensitivity,
        } => {
            if max_sensitivity < Sensitivity::Internal {
                bail!("permission_denied");
            }
            let job = q.scoped_get(&principal, &job_id)?;
            check_capture_result(library, &job, max_sensitivity)?;
            (Some(job), None, None, None, None, None, None)
        }
        Command::ScopedTranslationStatus {
            item_id,
            max_sensitivity,
        } => {
            let mut projection = memex_analysis::read_translation_projection(
                library,
                &item_id,
                max_sensitivity,
                None,
            )?;
            if projection.state == "not_requested" {
                if q.translation_intent_pending(&item_id, &projection.revision_id)? {
                    projection.state = "awaiting_enqueue".into();
                    projection.reason = Some("capture_hook_pending".into());
                } else {
                    // The hook may have enqueued and acknowledged between the reads.
                    // Observe Analysis again before claiming absence, never enqueue here.
                    #[cfg(test)]
                    AFTER_HOOK_ABSENCE.with(|hook| {
                        if let Some(write) = hook.borrow_mut().take() {
                            write();
                        }
                    });
                    projection = memex_analysis::read_translation_projection(
                        library,
                        &item_id,
                        max_sensitivity,
                        None,
                    )?;
                    // Store commit and finish_stored are separate: a new intent
                    // may have appeared during that read, possibly for a new revision.
                    if projection.state == "not_requested"
                        && q.translation_intent_pending(&item_id, &projection.revision_id)?
                    {
                        projection.state = "awaiting_enqueue".into();
                        projection.reason = Some("capture_hook_pending".into());
                    }
                }
            }
            // Reconciled jobs nobody requested never run: report them as not requested
            // so the reader offers an explicit request.
            if projection.state == "queued"
                && !memex_analysis::translation_authorized(
                    library,
                    &item_id,
                    &projection.revision_id,
                )?
            {
                projection.state = "not_requested".into();
                projection.reason = None;
            }
            translation = Some(projection);
            (None, None, None, None, None, None, None)
        }
        Command::ScopedTranslationRead {
            item_id,
            expected_revision_id,
            start_byte,
            max_sensitivity,
        } => {
            translation = Some(memex_analysis::read_translation_projection(
                library,
                &item_id,
                max_sensitivity,
                Some((&expected_revision_id, start_byte)),
            )?);
            (None, None, None, None, None, None, None)
        }
        Command::Enqueue {
            producer,
            producer_job_id,
            url,
        } => (
            Some(q.enqueue(producer.as_str(), &producer_job_id, &url)?),
            None,
            None,
            None,
            None,
            None,
            None,
        ),
        Command::List { cursor } => {
            let (jobs, next_cursor) = q.list(cursor.as_deref())?;
            (None, Some(jobs), next_cursor, None, None, None, None)
        }
        Command::Status { job_id } => (Some(q.get(&job_id)?), None, None, None, None, None, None),
        Command::RemoteStatus { job_id } => (
            Some(q.remote_get(&job_id)?),
            None,
            None,
            None,
            None,
            None,
            None,
        ),
        Command::ItemRead { item_id } => {
            // A trashed item reads like a missing one (each command keeps its own code).
            if memex_store::is_item_trashed(library, &item_id)? {
                bail!("item_not_found")
            }
            let (_, revision) = memex_store::read_item(library, &item_id)
                .map_err(|_| anyhow::anyhow!("item_not_found"))?;
            if revision.content_markdown.len() > MAX_MARKDOWN_BYTES {
                bail!("result_too_large")
            }
            (
                None,
                None,
                None,
                Some(ItemResult {
                    item_id: revision.item_id,
                    revision_id: revision.revision_id,
                    title: revision.title,
                    markdown: revision.content_markdown,
                    manual_tags: Some(
                        revision
                            .metadata
                            .manual_tags
                            .map(|tags| tags.value)
                            .unwrap_or_default(),
                    ),
                }),
                None,
                None,
                None,
            )
        }
        Command::RemoteItemRead { job_id, item_id } => {
            q.authorize_remote_item(&job_id, &item_id)?;
            if memex_store::is_item_trashed(library, &item_id)? {
                bail!("remote_resource_not_found")
            }
            let (_, revision) = memex_store::read_item(library, &item_id)
                .map_err(|_| anyhow::anyhow!("remote_resource_not_found"))?;
            if revision.content_markdown.len() > MAX_MARKDOWN_BYTES {
                bail!("result_too_large")
            }
            (
                None,
                None,
                None,
                Some(ItemResult {
                    item_id: revision.item_id,
                    revision_id: revision.revision_id,
                    title: revision.title,
                    markdown: revision.content_markdown,
                    manual_tags: None,
                }),
                None,
                None,
                None,
            )
        }
        Command::LibraryList { cursor } => {
            let (items, next_cursor) = library_page(library, cursor.as_deref(), LIBRARY_PAGE_SIZE)?;
            (None, None, next_cursor, None, Some(items), None, None)
        }
        Command::ContextRead(request) => {
            context = Some(memex_analysis::context_read(library, &request)?);
            (None, None, None, None, None, None, None)
        }
        command @ (Command::ItemCoverBatch { .. } | Command::ItemCoverChunk { .. }) => {
            return dispatch_cover(library, command);
        }
        Command::ImageRead { item_id, expected_revision_id, target, offset, max_sensitivity } => {
            use base64::Engine;
            let chunk = memex_store::read_image(library, &item_id, &expected_revision_id, &target, offset, max_sensitivity)?;
            let mut response = Response::empty_ok();
            response.image = Some(serde_json::json!({
                "item_id": item_id, "revision_id": expected_revision_id, "target": target,
                "media_type": chunk.media_type, "sha256": chunk.sha256,
                "total_bytes": chunk.total_bytes, "offset": chunk.offset, "next_offset": chunk.next_offset,
                "data_base64": base64::engine::general_purpose::STANDARD.encode(&chunk.bytes),
            }));
            return Ok(response);
        }
        Command::LibraryQuery(_) => bail!("library_loading"),
        Command::ScopedItemRead {
            item_id,
            max_sensitivity,
        } => {
            library_entry = Some(memex_library::read_entry(
                library,
                &item_id,
                max_sensitivity,
            )?);
            (None, None, None, None, None, None, None)
        }
        Command::ScopedMetadataUpdate {
            item_id,
            max_sensitivity,
            update,
        } => {
            // No translation/summary jobs from the mobile edit seam. Authorization
            // is imposed by the gateway; the Store enforces its ceiling inside CAS.
            let previous_title = memex_store::read_item(library, &item_id)
                .ok()
                .map(|(_, revision)| revision.title);
            let receipt = memex_store::revise_item_metadata_at_sensitivity(
                library,
                &item_id,
                update,
                max_sensitivity,
            )?;
            retitle_after_edit(library, &receipt.item_id, previous_title.as_deref());
            (None, None, None, None, None, None, Some(receipt))
        }
        Command::ModelSettingsRead => {
            model_settings = Some(q.model_settings()?);
            (None, None, None, None, None, None, None)
        }
        Command::ModelSettingsReplace {
            expected_revision,
            configuration,
        } => {
            model_settings = Some(q.replace_model_settings(expected_revision, configuration)?);
            (None, None, None, None, None, None, None)
        }
        Command::ProviderCredentialRead {
            expected_revision,
            provider_id,
        } => {
            provider_credential = Some(q.provider_credential(expected_revision, &provider_id)?);
            (None, None, None, None, None, None, None)
        }
        Command::ProviderCredentialReplace {
            expected_revision,
            provider_id,
            expected_generation,
            api_key,
        } => {
            provider_credential = Some(q.replace_provider_credential(
                expected_revision,
                &provider_id,
                expected_generation,
                Some(api_key),
            )?);
            (None, None, None, None, None, None, None)
        }
        Command::ProviderCredentialClear {
            expected_revision,
            provider_id,
            expected_generation,
        } => {
            provider_credential = Some(q.replace_provider_credential(
                expected_revision,
                &provider_id,
                expected_generation,
                None,
            )?);
            (None, None, None, None, None, None, None)
        }
        Command::ProviderModelsDiscover {
            expected_revision,
            provider_id,
            authorization,
        } => {
            provider_models =
                Some(q.discover_provider_models(expected_revision, &provider_id, authorization)?);
            (None, None, None, None, None, None, None)
        }
        Command::LibraryItemRead { item_id } => {
            let metadata = memex_store::read_item_metadata_at_sensitivity(
                library,
                &item_id,
                Sensitivity::Restricted,
            )?
            .ok_or_else(|| anyhow::anyhow!("library_resource_not_found"))?;
            let (_, revision) = memex_store::read_item(library, &item_id)?;
            if revision.revision_id != metadata.revision_id {
                bail!("invalid item relationship")
            }
            let (translation_state, title, markdown) =
                library_translation_projection(library, &item_id, &revision.revision_id)?;
            let (summary_state, summary) =
                library_summary_projection(library, &item_id, &revision.revision_id)?;
            let ai_minutes =
                crate::translation_request::ai_minutes(library, &item_id, &revision.revision_id)?;
            if summary.as_ref().map_or(0, String::len)
                + ai_minutes.text.as_ref().map_or(0, String::len)
                + title.as_ref().map_or(0, String::len)
                + markdown.as_ref().map_or(0, String::len)
                > MAX_LIBRARY_TRANSLATION_TEXT_BYTES
            {
                bail!("result_too_large")
            }
            let current = memex_store::read_item_metadata_at_sensitivity(
                library,
                &item_id,
                Sensitivity::Restricted,
            )?
            .ok_or_else(|| anyhow::anyhow!("library_resource_not_found"))?;
            if current.revision_id != revision.revision_id {
                bail!("invalid item relationship")
            }
            let insights = crate::insights::projection(library, &revision)?;
            let (translated_title, translation_progress) = match translation_state {
                LibraryTranslationState::Succeeded => (title.clone(), None),
                LibraryTranslationState::NotRequested | LibraryTranslationState::Unavailable => {
                    (None, None)
                }
                _ => memex_analysis::translation_partial(
                    library,
                    &revision.item_id,
                    &revision.revision_id,
                )?,
            };
            let (translated_title, title_source) = match translated_title {
                Some(text) => (Some(text), Some(memex_analysis::TITLE_SOURCE_WITH_CONTEXT.into())),
                None => memex_analysis::translated_title(
                    library,
                    &revision.item_id,
                    &revision.revision_id,
                    &revision.title,
                )
                .ok()
                .flatten()
                .map_or((None, None), |view| {
                    (Some(view.translated_title), Some(view.title_source))
                }),
            };
            let mut result = LibraryItemResult {
                item_id: revision.item_id,
                revision_id: revision.revision_id,
                summary_state,
                summary,
                translation_state,
                title,
                markdown,
                ai_minutes: Some(ai_minutes),
                ai_insights: Some(insights),
                translated_title,
                title_source,
                translation_progress,
            };
            // Insight items never push a translated read over the response frame; the client
            // reads them with `insight_request` (a succeeded kind is returned without a call).
            if serde_json::to_vec(&result)?.len() > MAX_RESPONSE_FRAME - 16 * 1024 {
                result.ai_insights = result.ai_insights.map(|value| value.without_items());
            }
            (None, None, None, None, None, Some(result), None)
        }
        Command::UpdateItemMetadata {
            item_id,
            expected_revision_id,
            title,
            manual_tags,
        } => {
            let previous_title = memex_store::read_item(library, &item_id)
                .ok()
                .map(|(_, revision)| revision.title);
            let receipt = memex_store::revise_item_metadata(
                library,
                &item_id,
                memex_store::ItemMetadataUpdate {
                    expected_revision_id,
                    title,
                    manual_tags,
                },
            )?;
            retitle_after_edit(library, &receipt.item_id, previous_title.as_deref());
            enqueue_after_owner_edit(library, &receipt);
            (None, None, None, None, None, None, Some(receipt))
        }
        Command::ItemTitleApply {
            item_id,
            expected_revision_id,
            title,
            request_id,
        } => {
            let title = title.trim().to_owned();
            anyhow::ensure!(
                !title.is_empty()
                    && title.chars().count() <= 512
                    && !title.chars().any(char::is_control)
                    && (1..=128).contains(&request_id.len())
                    && request_id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')),
                "invalid_metadata_update"
            );
            let (_, current) = memex_store::read_item(library, &item_id)
                .map_err(|_| anyhow::anyhow!("library_resource_not_found"))?;
            let receipt = if current.title == title {
                // Replay (or already applied): no new revision.
                memex_store::ItemMetadataUpdateReceipt {
                    schema_version: memex_core::SCHEMA_VERSION,
                    item_id: current.item_id.clone(),
                    previous_revision_id: current.revision_id.clone(),
                    revision_id: current.revision_id.clone(),
                    revision_number: current.revision_number,
                }
            } else {
                let manual_tags = current
                    .metadata
                    .manual_tags
                    .as_ref()
                    .map(|tags| tags.value.clone())
                    .unwrap_or_default();
                let receipt = memex_store::revise_item_metadata(
                    library,
                    &item_id,
                    memex_store::ItemMetadataUpdate {
                        expected_revision_id,
                        title,
                        manual_tags,
                    },
                )?;
                retitle_after_edit(library, &receipt.item_id, Some(&current.title));
                enqueue_after_owner_edit(library, &receipt);
                receipt
            };
            (None, None, None, None, None, None, Some(receipt))
        }
        // Served from their own thread by `serve_with_chat`; this synchronous path is unused there.
        command @ (Command::DedupPlan { .. } | Command::DedupApply { .. }) => {
            let source = SavedProviderSource {
                database: q.database_path().to_owned(),
            };
            let database = q.database_path().to_owned();
            return Ok(dedup_response(library, &database, &source, command));
        }
        command @ (Command::GroupDeletePreview { .. } | Command::GroupDelete { .. }
            | Command::GroupDeleteList { .. } | Command::GroupDeleteRestore { .. }) => {
            return crate::groups::deletion::dispatch(library, command);
        }
        Command::GroupTrashList { group_id, cursor, max_sensitivity } => {
            let (items, next_cursor) = memex_store::list_group_trash(library, &group_id, cursor.as_deref(), max_sensitivity)?;
            let mut response = Response::empty_ok();
            response.trash_items = Some(items);
            response.next_cursor = next_cursor;
            return Ok(response);
        }
        Command::GroupTrashEdit { group_id, item_id, request_id, expected_revision_id, restore_plan, max_sensitivity } => {
            memex_store::edit_group_trash(library, &group_id, &item_id, &request_id, &expected_revision_id, restore_plan.as_deref(), max_sensitivity)?;
            return Ok(Response::empty_ok());
        }
        Command::CaptureJobsRecent { max_sensitivity, limit } => {
            if max_sensitivity < Sensitivity::Internal {
                bail!("permission_denied");
            }
            // Metadata-only visibility (ceiling and trash); never reads item content.
            let recent = q.capture_jobs_recent(limit, |id| {
                matches!(memex_store::read_item_metadata_at_sensitivity(library, id, max_sensitivity), Ok(Some(_)))
            })?;
            let mut response = Response::empty_ok();
            response.capture_jobs = Some(recent);
            return Ok(response);
        }
        Command::TrashList { cursor } => {
            let (items, next_cursor) = memex_store::list_trash(library, cursor.as_deref())?;
            let mut response = Response::empty_ok();
            response.trash_items = Some(items);
            response.next_cursor = next_cursor;
            return Ok(response);
        }
        Command::ItemTrashRestore { item_id } => {
            let receipt = memex_store::restore_item(library, &item_id)?;
            let mut response = Response::empty_ok();
            response.trash_restore = Some(receipt);
            return Ok(response);
        }
        // Served from its own thread by `serve_with_chat`; this synchronous path is unused there.
        Command::ItemChat {
            item_id,
            max_sensitivity,
            request_id,
            messages,
        } => {
            let source = SavedProviderSource {
                database: q.database_path().to_owned(),
            };
            return Ok(item_chat_response(
                library,
                q.database_path(),
                &source,
                &item_id,
                max_sensitivity,
                &request_id,
                &messages,
            ));
        }
        Command::AgentVersion => {
            let mut response = Response::empty_ok();
            response.agent_version = Some(agent_version());
            return Ok(response);
        }
        Command::Capabilities => {
            let mut value = crate::bibliographic::capabilities();
            value.translation_provider_v1 = crate::provider_generation::capability(q);
            value.ai_insights_v1 = Some(crate::insights::capability(
                &value.translation_provider_v1.purpose_routes,
            ));
            value
                .features
                .push(crate::item_chat::ITEM_CHAT_HISTORY_FEATURE.into());
            value.features.push("reader_v1".into());
            value.features.push("reader_annotations_v1".into());
            value.features.push("reader_paragraph_alignment_v1".into());
            value.features.push("reader_chapter_repair_v1".into());
            value.features.push("reader_chapter_edit_v1".into());
            value.features.push("reader_chapter_translation_v1".into());
            value.features.push("reader_chapter_translation_metadata_v1".into());
            value.features.push("reader_chapter_translation_alignment_v1".into());
            value.features.push("reader_revision_text_v1".into());
            value.features.push("reader_content_format_v1".into());
            value.features.push("reader_content_format_batch_v1".into());
            value.features.push("reader_passes_v1".into());
            value.features.push("reader_annotations_ai_translation_v1".into());
            value.features.push("item_cover_v1".into());
            value.features.push("ui_active_group_v1".into());
            value.features.push("ai_minutes_suggested_title_v1".into());
            value.features.push("item_title_apply_v1".into());
            value.features.push("one_step_model_jobs_v1".into());
            value
                .features
                .push(crate::queue::recent::CAPTURE_JOBS_RECENT_FEATURE.into());
            capabilities = Some(value);
            (None, None, None, None, None, None, None)
        }
        // Routed to their feature modules above.
        Command::BibliographicMetadataRead { .. }
        | Command::BibliographicMetadataEdit { .. }
        | Command::MarkdownEdit { .. }
        | Command::Reader { .. }
        | Command::ReadingAnnotationsRead { .. }
        | Command::ReadingAnnotationsSave { .. }
        | Command::BibliographicDuplicateCheck { .. }
        | Command::BibliographicLookupPreview { .. }
        | Command::BibliographicLookupPreviewRead { .. }
        | Command::BibliographicLookupValueRead { .. }
        | Command::GroupTreeRead { .. }
        | Command::GroupEdit { .. }
        | Command::TranslationRequest { .. }
        | Command::MinutesRequest { .. }
        | Command::ScopedMinutesStatus { .. }
        | Command::ScopedInsightsStatus { .. }
        | Command::ConversationRead { .. }
        | Command::ConversationImport { .. }
        | Command::InsightRequest { .. }
        | Command::TitleTranslationBackfill { .. }
        | Command::ModelGenerationTest { .. } => bail!("internal_error"),
        Command::Retry { job_id } => (Some(q.retry(&job_id)?), None, None, None, None, None, None),
        Command::Cancel { job_id } => {
            (Some(q.cancel(&job_id)?), None, None, None, None, None, None)
        }
        Command::Approve { job_id } => (
            Some(q.approve(&job_id)?),
            None,
            None,
            None,
            None,
            None,
            None,
        ),
    };
    Ok(Response {
        ok: true,
        job,
        jobs,
        next_cursor,
        reason_code: None,
        item,
        library_items,
        library_item,
        item_update,
        context,
        library_query,
        library_entry,
        model_settings,
        provider_credential,
        provider_models,
        translation,
        bibliographic_metadata: None,
        identifier_duplicates: None,
        lookup_preview: None,
        lookup_value: None,
        group_tree: None,
        generation_test: None,
        image: None,
        item_cover: None,
        capabilities,
        group_edit: None,
        ui_active_group: None,
        group_delete_preview: None,
        group_delete: None,
        group_delete_restore: None,
        group_deletions: None,
        reading_annotations: None,
        reader: None,
        ai_minutes: None,
        item_chat: None,
        ai_insights: None,
        title_backfill: None,
        conversation: None,
        conversation_import: None,
        agent_version: None,
        dedup_plan: None,
        dedup_apply: None,
        trash_items: None,
        trash_restore: None,
        capture_jobs: None,
    })
}

fn check_capture_result(library: &Path, job: &JobView, ceiling: Sensitivity) -> Result<()> {
    if let Some(id) = &job.item_id {
        memex_library::read_entry(library, id, ceiling)
            .map_err(|_| anyhow::anyhow!("remote_resource_not_found"))?;
    }
    Ok(())
}

/// The Store commit is already durable. A transient analysis-queue failure must not turn a
/// successful owner edit into an ambiguous failure that the client might retry; the bounded
/// Store reconciler repairs any missing current-revision jobs. Same-body revisions reuse the
/// inherited results, so nothing new is sent.
fn enqueue_after_owner_edit(library: &Path, receipt: &memex_store::ItemMetadataUpdateReceipt) {
    let _ = memex_analysis::enqueue_item_translation(library, &receipt.item_id, &receipt.revision_id);
    let _ = memex_analysis::enqueue_item_summary(library, &receipt.item_id, &receipt.revision_id);
}

/// Best effort after a durable owner title edit: derived results stay with the item's body
/// (see `memex_store::body_identity`); only the edited title is translated again, and only
/// when the item's translation was already authorized. Never fails the edit.
pub(crate) fn retitle_after_edit(library: &Path, item_id: &str, previous_title: Option<&str>) {
    if let Some(previous_title) = previous_title {
        let _ = memex_analysis::TitleTranslationQueue::open(library)
            .and_then(|mut titles| titles.request_retitle(item_id, previous_title));
    }
}

fn library_summary_projection(
    library: &Path,
    item_id: &str,
    revision_id: &str,
) -> Result<(LibrarySummaryState, Option<String>)> {
    let Some(status) = memex_analysis::summary_status(library, item_id, revision_id)? else {
        return Ok((LibrarySummaryState::NotRequested, None));
    };
    let state = match status.state {
        // Reconciled jobs nobody requested never run; report them as not requested.
        SummaryJobState::Queued
            if !memex_analysis::summary_authorized(library, item_id, revision_id)? =>
        {
            LibrarySummaryState::NotRequested
        }
        SummaryJobState::Queued => LibrarySummaryState::Queued,
        SummaryJobState::Running => LibrarySummaryState::Running,
        SummaryJobState::Failed => LibrarySummaryState::Failed,
        SummaryJobState::Succeeded => {
            let Some(summary) = memex_analysis::validated_summary(library, item_id, revision_id)?
            else {
                return Ok((LibrarySummaryState::Unavailable, None));
            };
            let Some(record) = memex_analysis::read_record_at_sensitivity(
                library,
                &summary.analysis_id,
                Sensitivity::Restricted,
            )?
            else {
                return Ok((LibrarySummaryState::Unavailable, None));
            };
            if record
                .generated_text
                .as_ref()
                .map(|generated| generated.purpose)
                != Some(GeneratedTextPurpose::Summary)
            {
                return Ok((LibrarySummaryState::Unavailable, None));
            }
            if record.body_markdown.len() > MAX_LIBRARY_SUMMARY_TEXT_BYTES {
                bail!("result_too_large")
            }
            return Ok((LibrarySummaryState::Succeeded, Some(record.body_markdown)));
        }
    };
    Ok((state, None))
}

fn library_translation_projection(
    library: &Path,
    item_id: &str,
    revision_id: &str,
) -> Result<(LibraryTranslationState, Option<String>, Option<String>)> {
    let Some(status) = memex_analysis::translation_status(library, item_id, revision_id)? else {
        return Ok((LibraryTranslationState::NotRequested, None, None));
    };
    let state = match status.state {
        TranslationJobState::Queued
            if !memex_analysis::translation_authorized(library, item_id, revision_id)? =>
        {
            LibraryTranslationState::NotRequested
        }
        TranslationJobState::Queued => LibraryTranslationState::Queued,
        TranslationJobState::Running => LibraryTranslationState::Running,
        TranslationJobState::Failed => LibraryTranslationState::Failed,
        TranslationJobState::Succeeded => {
            let Some((pair, source)) =
                memex_analysis::effective_translation(library, item_id, revision_id)?
            else {
                return Ok((LibraryTranslationState::Unavailable, None, None));
            };
            let Some(title_record) = memex_analysis::read_record_at_sensitivity(
                library,
                &pair.title_analysis_id,
                Sensitivity::Restricted,
            )?
            else {
                return Ok((LibraryTranslationState::Unavailable, None, None));
            };
            let Some(document_record) = memex_analysis::read_record_at_sensitivity(
                library,
                &pair.document_analysis_id,
                Sensitivity::Restricted,
            )?
            else {
                return Ok((LibraryTranslationState::Unavailable, None, None));
            };
            if title_record
                .generated_text
                .as_ref()
                .map(|generated| generated.purpose)
                != Some(GeneratedTextPurpose::TitleTranslation)
            {
                return Ok((LibraryTranslationState::Unavailable, None, None));
            }
            // effective_translation verifies either the original document record or the
            // exact table-repair assembly against its durable binding. Assemblies deliberately
            // have no whole-document generated_text descriptor; arbitrary records never reach
            // this branch merely by having generated_text=None.
            if title_record.body_markdown.len() > 8 * 1024
                || document_record.body_markdown.len() > MAX_LIBRARY_TRANSLATION_TEXT_BYTES
                || title_record
                    .body_markdown
                    .len()
                    .saturating_add(document_record.body_markdown.len())
                    > MAX_LIBRARY_TRANSLATION_TEXT_BYTES
            {
                bail!("result_too_large")
            }
            // The body translation is inherited across metadata edits; the translated title
            // only while the title is unchanged (else the edited title's own translation).
            let (_, current) = memex_store::read_item(library, item_id)?;
            let title = if source.title == current.title {
                Some(title_record.body_markdown)
            } else {
                memex_analysis::retitled_translation(library, item_id, &current.title)?
            };
            return Ok((
                LibraryTranslationState::Succeeded,
                title,
                Some(document_record.body_markdown),
            ));
        }
    };
    Ok((state, None, None))
}

fn library_page(
    library: &Path,
    cursor: Option<&str>,
    page_size: usize,
) -> Result<(Vec<LibraryListItem>, Option<String>)> {
    let (metadata, next_cursor) = memex_store::page_all_item_metadata(library, cursor, page_size)?;
    let mut items = Vec::with_capacity(metadata.len());
    for metadata in metadata {
        if metadata.title.len() > 4096
            || metadata.item_type.len() > 128
            || metadata
                .captured_at
                .as_ref()
                .is_some_and(|value| value.len() > 64)
        {
            bail!("result_too_large")
        }
        if metadata.title.is_empty()
            || metadata.item_type.is_empty()
            || metadata.title.chars().any(char::is_control)
            || metadata.item_type.chars().any(char::is_control)
        {
            bail!("invalid item metadata relationship")
        }
        if metadata
            .captured_at
            .as_deref()
            .is_some_and(|value| chrono::DateTime::parse_from_rfc3339(value).is_err())
        {
            bail!("invalid item metadata relationship")
        }
        items.push(LibraryListItem {
            published_at: metadata.metadata.published_day(),
            duration_seconds: metadata.metadata.duration_seconds.as_ref().map(|v| v.value).filter(|v| *v > 0),
            item_id: metadata.item_id,
            revision_id: metadata.revision_id,
            title: metadata.title,
            item_type: metadata.item_type,
            captured_at: metadata.captured_at,
            site_name: metadata
                .metadata
                .site_name
                .map(|value| value.value)
                .filter(|value| value.len() <= 256 && !value.chars().any(char::is_control)),
        });
    }
    Ok((items, next_cursor))
}
fn read_frame(r: &mut impl Read) -> Result<Vec<u8>> {
    read_bounded_frame(r, MAX_REQUEST_FRAME)
}
fn read_response_frame(r: &mut impl Read) -> Result<Vec<u8>> {
    read_bounded_frame(r, MAX_RESPONSE_FRAME)
}
fn read_bounded_frame(r: &mut impl Read, limit: usize) -> Result<Vec<u8>> {
    let mut n = [0; 4];
    r.read_exact(&mut n)?;
    let n = u32::from_be_bytes(n) as usize;
    if n > limit {
        bail!("frame_too_large")
    }
    let mut b = vec![0; n];
    r.read_exact(&mut b)?;
    Ok(b)
}
fn write_frame(w: &mut impl Write, b: &[u8]) -> Result<()> {
    write_bounded_frame(w, b, MAX_REQUEST_FRAME)
}
fn write_response_frame(w: &mut impl Write, b: &[u8]) -> Result<()> {
    write_bounded_frame(w, b, MAX_RESPONSE_FRAME)
}
fn response_body(response: &Response) -> Result<Vec<u8>> {
    let body = serde_json::to_vec(response)?;
    if body.len() <= MAX_RESPONSE_FRAME {
        Ok(body)
    } else {
        Ok(serde_json::to_vec(&Response::error("result_too_large"))?)
    }
}
fn write_bounded_frame(w: &mut impl Write, b: &[u8], limit: usize) -> Result<()> {
    if b.len() > limit {
        bail!("frame_too_large")
    }
    w.write_all(&(b.len() as u32).to_be_bytes())?;
    w.write_all(b)?;
    w.flush()?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_worker_errors_restart_the_worker_instead_of_stopping_the_daemon() {
        let busy = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            None,
        );
        assert_eq!(capture_step(Ok(Err(busy.into()))), CaptureStep::Busy);
        assert_eq!(capture_step(Ok(Err(anyhow::anyhow!("lease_lost")))), CaptureStep::LeaseLost);
        assert_eq!(capture_step(Ok(Ok(true))), CaptureStep::Progress);
        assert_eq!(capture_step(Ok(Ok(false))), CaptureStep::Idle);
        assert_eq!(
            capture_step(Ok(Err(anyhow::anyhow!("fetch_worker_failed")))),
            CaptureStep::Restart("fetch_worker_failed".into())
        );
        // Prose (paths, URLs, error chains) never reaches the log.
        assert_eq!(
            capture_step(Ok(Err(anyhow::anyhow!("open /Users/x/secret.db: denied")))),
            CaptureStep::Restart("internal_error".into())
        );
        let panicked = std::panic::catch_unwind(|| -> Result<bool> { panic!("synthetic") });
        assert_eq!(capture_step(panicked), CaptureStep::Restart("internal_error".into()));
    }

    #[test]
    fn a_failing_capture_worker_restarts_without_stopping_the_daemon() {
        struct Poisoned;
        impl crate::fetch::Resolver for Poisoned {
            fn resolve(&self, _: &str, _: Duration) -> Result<Vec<std::net::IpAddr>> {
                bail!("dns_timeout") // the fatal-worker disposition
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let cfg = AgentConfig {
            library: root.join("library"),
            spool: root.join("spool"),
            database: root.join("agent.db"),
            socket: root.join("d.sock"),
            legacy_translation: None,
            intake: None,
            metadata_lookup_contact: None,
            chrome_executable: None,
        };
        let job = Queue::open(&cfg.database)
            .unwrap()
            .enqueue("test", "poisoned-daemon", "https://worker.test/one")
            .unwrap();
        let config_path = root.join("config.json");
        fs::write(&config_path, serde_json::to_vec(&cfg).unwrap()).unwrap();
        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600)).unwrap();
        let daemon = std::thread::spawn(move || {
            run_daemon_with_workers(&config_path, Poisoned, Offline, [|_, _| Ok(false); 3],
                |_, _| Ok(7), |_, _| Ok(7))
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        // The capture attempt fails with fetch_worker_failed and keeps its durable lease.
        while Queue::open(&cfg.database).unwrap().get(&job.job_id).unwrap().state != crate::queue::State::Leased {
            assert!(!daemon.is_finished(), "daemon stopped");
            assert!(std::time::Instant::now() < deadline, "capture never attempted");
            std::thread::sleep(Duration::from_millis(50));
        }
        std::thread::sleep(CAPTURE_RESTART_DELAY + Duration::from_millis(500));
        assert!(!daemon.is_finished(), "a capture worker error stopped the daemon");
        let response = send_command(&cfg.socket, &Command::List { cursor: None }).unwrap();
        assert!(response.ok);
    }
    use memex_analysis::TranslationQueue;
    use memex_analysis::{
        GeneratedTextPurpose, SummaryModel, TranslationModel, TranslationModelDescriptor,
        TranslationModelError, TranslationModelInput,
    };
    use memex_core::{
        BlobDescriptor, CaptureEnvelope, ItemDraft, Sensitivity, SourceProfile, VerifiedBlob,
        VerifiedCapture,
    };

    // Existing single-command tests exercise the same dispatch implementation.
    // Pagination is checked over a persistent service and the real socket below.
    fn dispatch(q: &mut Queue, library: &Path, c: Command) -> Result<Response> {
        dispatch_with(q, library, &offline_network(), c)
    }

    fn dispatch_with(
        q: &mut Queue,
        library: &Path,
        network: &LookupNetwork,
        c: Command,
    ) -> Result<Response> {
        super::dispatch(
            q,
            library,
            Some(&mut memex_library::LibraryQuery::new(library)?),
            network,
            c,
        )
    }

    /// Tests never reach a real network: the default network always fails.
    struct Offline;
    impl crate::fetch::Resolver for Offline {
        fn resolve(&self, _: &str, _: Duration) -> Result<Vec<std::net::IpAddr>> {
            bail!("offline_test_network")
        }
    }
    impl crate::fetch::HttpTransport for Offline {
        fn get(
            &self,
            _: &url::Url,
            _: &[std::net::SocketAddr],
            _: Duration,
        ) -> Result<crate::fetch::RawResponse> {
            bail!("offline_test_network")
        }
    }
    fn offline_network() -> LookupNetwork {
        LookupNetwork::new(Offline, Offline)
    }

    #[derive(Clone, Default)]
    struct Canned {
        responses: std::sync::Arc<std::sync::Mutex<Vec<crate::fetch::RawResponse>>>,
        calls: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }
    impl crate::fetch::Resolver for Canned {
        fn resolve(&self, _: &str, _: Duration) -> Result<Vec<std::net::IpAddr>> {
            Ok(vec!["93.184.216.34".parse().unwrap()])
        }
    }
    impl crate::fetch::HttpTransport for Canned {
        fn get(
            &self,
            url: &url::Url,
            pinned: &[std::net::SocketAddr],
            _: Duration,
        ) -> Result<crate::fetch::RawResponse> {
            assert_eq!(pinned[0].port(), 443);
            self.calls.lock().unwrap().push(url.to_string());
            let mut responses = self.responses.lock().unwrap();
            if responses.is_empty() {
                bail!("no_canned_response")
            }
            Ok(responses.remove(0))
        }
    }
    fn canned(responses: Vec<crate::fetch::RawResponse>) -> (LookupNetwork, Canned) {
        let canned = Canned::default();
        *canned.responses.lock().unwrap() = responses;
        (LookupNetwork::new(canned.clone(), canned.clone()), canned)
    }
    fn json_response(status: u16, body: &serde_json::Value) -> crate::fetch::RawResponse {
        crate::fetch::RawResponse {
            status,
            headers: vec![(
                "content-type".into(),
                "application/json; charset=utf-8".into(),
            )],
            body: serde_json::to_vec(body).unwrap(),
        }
    }
    fn header_response(status: u16, headers: &[(&str, &str)]) -> crate::fetch::RawResponse {
        crate::fetch::RawResponse {
            status,
            headers: headers
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            body: Vec::new(),
        }
    }

    #[test]
    fn cover_reads_use_bounded_background_lane_and_preserve_bytes_and_permissions() {
        use base64::Engine;
        use sha2::{Digest, Sha256};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let item = synthetic_stored_item_at_sensitivity(&root, "cover", Sensitivity::Private);
        let (_, revision) = memex_store::read_item(&root, &item).unwrap();
        let rss = b"<rss/>";
        let original = include_bytes!("../examples/fixtures/reading-image.png");
        let cover = memex_store::covers::put_cover(&root, memex_store::covers::CoverEdit {
            item_id: item.clone(), expected_revision_id: revision.revision_id,
            expected_cover_version: None, request_id: "cover-ipc".into(),
            source: memex_store::covers::CoverSource::Podcast {
                episode_url: "https://example.invalid/cover".into(), youtube_video_id: "abcdefghijk".into(),
                image_url: "https://i.ytimg.com/vi/abcdefghijk/hqdefault.jpg".into(),
                rss_sha256: format!("{:x}", Sha256::digest(rss)),
            },
        }, original, rss, Sensitivity::Private).unwrap();
        let mut worker = BackgroundQueries::new(root.clone()).unwrap();
        let mut call = |value: serde_json::Value| {
            let command = serde_json::from_value(value).unwrap();
            let (server, mut client) = UnixStream::pair().unwrap();
            client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            worker.serve(server, command, &root.join("must-not-open.db")).unwrap();
            let response: Response = serde_json::from_slice(&read_response_frame(&mut client).unwrap()).unwrap();
            response
        };
        let batch = |ceiling: &str| serde_json::json!({"command":"item_cover_batch",
            "item_ids":[item], "max_sensitivity":ceiling});
        assert_eq!(call(batch("public")).item_cover.unwrap(), serde_json::json!([null]));
        assert_eq!(call(batch("private")).item_cover.unwrap()[0]["version"], cover.version);
        for thumbnail in [false, true] {
            let mut bytes = Vec::new();
            let mut offset = 0;
            loop {
                let response = call(serde_json::json!({"command":"item_cover_chunk", "item_id":item,
                    "version":cover.version, "thumbnail":thumbnail, "offset":offset, "max_sensitivity":"private"}));
                assert!(response.ok, "{:?}", response.reason_code);
                let chunk = response.item_cover.unwrap();
                assert_eq!(chunk["offset"], offset);
                let part = base64::engine::general_purpose::STANDARD.decode(chunk["data_base64"].as_str().unwrap()).unwrap();
                assert!(part.len() <= 96 * 1024);
                bytes.extend(part);
                match chunk["next_offset"].as_u64() {
                    Some(next) => offset = next,
                    None => break,
                }
            }
            let expected = if thumbnail { &cover.thumbnail } else { &cover.raster };
            assert_eq!(format!("{:x}", Sha256::digest(&bytes)), expected.sha256);
            assert_eq!(bytes.len(), expected.byte_size);
            if !thumbnail { assert_eq!(bytes, original); }
        }
        let chunk = |ceiling: &str, version: &str| serde_json::json!({"command":"item_cover_chunk",
            "item_id":item,"version":version,"thumbnail":false,"offset":0,"max_sensitivity":ceiling});
        assert_eq!(call(chunk("public", &cover.version)).reason_code.as_deref(), Some("library_resource_not_found"));
        assert_eq!(call(chunk("private", &"0".repeat(64))).reason_code.as_deref(), Some("stale_cover_version"));
        assert!(!root.join("must-not-open.db").exists());
        assert!(serde_json::to_value(Response::empty_ok()).unwrap().get("item_cover").is_none());
    }

    fn synthetic_stored_item(root: &Path, suffix: &str) -> String {
        synthetic_stored_item_at_sensitivity(root, suffix, Sensitivity::Public)
    }

    fn synthetic_stored_item_at_sensitivity(
        root: &Path,
        suffix: &str,
        sensitivity: Sensitivity,
    ) -> String {
        stored_item_with(root, suffix, sensitivity, None)
    }

    /// `content`: `(source_url, title, markdown)` instead of the suffix-derived defaults.
    fn stored_item_with(
        root: &Path,
        suffix: &str,
        sensitivity: Sensitivity,
        content: Option<(&str, &str, &str)>,
    ) -> String {
        let source_id = format!("remote-test-source-{suffix}");
        let source_url = content.map_or_else(
            || format!("https://example.invalid/{suffix}"),
            |(url, _, _)| url.to_owned(),
        );
        let title = content.map_or_else(
            || format!("Synthetic remote result {suffix}"),
            |(_, title, _)| title.to_owned(),
        );
        let capture = VerifiedCapture {
            source: SourceProfile {
                schema_version: 1,
                source_id: source_id.clone(),
                name: "Synthetic remote test source".into(),
                source_kind: "web".into(),
                canonical_uris: vec![],
                evidence_roles: vec![],
                default_sensitivity: sensitivity,
                preferred_extractor: None,
            },
            capture: CaptureEnvelope {
                schema_version: 1,
                adapter_id: "remote-test".into(),
                adapter_version: "1".into(),
                external_capture_id: format!("remote-test-capture-{suffix}"),
                source_id,
                captured_at: "2026-01-01T00:00:00Z".into(),
                capture_method: "synthetic".into(),
                sensitivity,
                route_summary: None,
                title: Some(title.clone()),
                source_url: source_url.clone(),
                canonical_url: None,
                artifacts: vec![],
            },
            blobs: vec![VerifiedBlob {
                descriptor: BlobDescriptor {
                    schema_version: 1,
                    locator: "synthetic".into(),
                    media_type: "text/plain".into(),
                    byte_size: 1,
                    sha256: "synthetic".into(),
                    source_url: None,
                },
                blob_id: "sha256:synthetic".into(),
                source_backend_ref: Some("capture-bundle:synthetic".into()),
                bytes: b"x".to_vec(),
            }],
        };
        let draft = ItemDraft {
            item_type: "webpage".into(),
            title,
            source_url,
            canonical_url: None,
            content_markdown: if let Some((_, _, markdown)) = content {
                markdown.to_owned()
            } else if suffix.starts_with("long-english-") {
                // 36 paragraphs of ~2 KiB (~70 KiB, podcast-sized): a glossary request and six
                // 12 KiB translation segments of six paragraphs each.
                (0..36)
                    .map(|index| {
                        format!(
                            "Paragraph {index:02}: {}",
                            "The harmless synthetic article explains facts. ".repeat(42)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n")
            } else if suffix.starts_with("chinese-") {
                "# 合成中文文章\n\n这是一篇用于测试的合成中文文章，内容完全是虚构的，不包含任何私密信息，也不需要被翻译成中文。我们只用它来检查语言判断是否正确。".into()
            } else if matches!(suffix, "translated-library" | "desktop-metadata")
                || suffix.starts_with("english-")
            {
                format!(
                    "# Synthetic remote result {suffix}\n\nThis harmless article has the facts and context that are needed for a deterministic English translation test. The source is synthetic and contains no private data."
                )
            } else {
                format!("# Synthetic remote result {suffix}")
            },
            sensitivity,
            translator_id: "synthetic".into(),
            translator_version: "1".into(),
            metadata: if suffix.starts_with("english-intake-keywords") {
                memex_core::ItemMetadata {
                    keywords: Some(memex_core::Sourced {
                        value: vec!["harmless article".into()],
                        provenance: memex_core::MetadataProvenance::new(
                            memex_core::MetadataOrigin::IntakeProvided,
                            "synthetic",
                            "1",
                        ),
                    }),
                    ..Default::default()
                }
            } else {
                Default::default()
            },
            attachments: Vec::new(),
        };
        memex_store::ingest(root, &capture, draft)
            .unwrap()
            .item_ids[0]
            .clone()
    }

    struct SyntheticTranslationModel;

    impl TranslationModel for SyntheticTranslationModel {
        fn descriptor(&self) -> TranslationModelDescriptor {
            TranslationModelDescriptor {
                model_id: "synthetic-ipc-test".into(),
                model_sha256: format!("sha256:{}", "7".repeat(64)),
                adapter_id: "memex.synthetic-ipc-test".into(),
                adapter_version: "1".into(),
                request_timeout: Duration::from_secs(1),
            }
        }

        fn translate(
            &mut self,
            input: TranslationModelInput<'_>,
        ) -> std::result::Result<String, TranslationModelError> {
            Ok(match input.purpose() {
                GeneratedTextPurpose::TitleTranslation => "合成远程文章翻译结果".into(),
                GeneratedTextPurpose::DocumentTranslation => {
                    "# 合成远程结果\n\n这是一篇无害的合成文章，包含确定性英文翻译测试所需的事实与上下文。来源完全为合成数据，不包含任何私密内容。".into()
                }
                _ => return Err(TranslationModelError::ProviderFailure),
            })
        }
    }

    struct SyntheticSummaryModel;

    impl SummaryModel for SyntheticSummaryModel {
        fn descriptor(&self) -> memex_analysis::SummaryModelDescriptor {
            memex_analysis::SummaryModelDescriptor {
                model_id: "synthetic-summary-ipc-test".into(),
                model_sha256: format!("sha256:{}", "6".repeat(64)),
                adapter_id: "memex.synthetic-summary-ipc-test".into(),
                adapter_version: "1".into(),
                request_timeout: Duration::from_secs(1),
            }
        }

        fn summarize(
            &mut self,
            input: memex_analysis::SummaryModelInput<'_>,
        ) -> std::result::Result<String, memex_analysis::SummaryModelError> {
            assert!(input.source_text().contains("harmless article"));
            Ok(
                "这篇无害的合成文章概括了用于确定性验证的事实与上下文，且不包含任何私密内容。"
                    .into(),
            )
        }
    }

    struct BlockingTranslationModel {
        entered: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
        blocked: bool,
    }

    impl TranslationModel for BlockingTranslationModel {
        fn descriptor(&self) -> TranslationModelDescriptor {
            TranslationModelDescriptor {
                model_id: "synthetic-blocking-test".into(),
                model_sha256: format!("sha256:{}", "8".repeat(64)),
                adapter_id: "memex.synthetic-blocking-test".into(),
                adapter_version: "1".into(),
                request_timeout: Duration::from_secs(1),
            }
        }

        fn translate(
            &mut self,
            input: TranslationModelInput<'_>,
        ) -> std::result::Result<String, TranslationModelError> {
            if !self.blocked {
                self.blocked = true;
                self.entered.send(()).unwrap();
                self.release.recv().unwrap();
            }
            Ok(match input.purpose() {
                GeneratedTextPurpose::TitleTranslation => "隔离的合成文章翻译".into(),
                GeneratedTextPurpose::DocumentTranslation => {
                    "# 隔离的合成翻译\n\n这段无害的合成中文证明阻塞的翻译工作不会占用捕获队列。"
                        .into()
                }
                _ => return Err(TranslationModelError::ProviderFailure),
            })
        }
    }

    struct FlakyTranslationModel {
        calls: usize,
    }

    impl TranslationModel for FlakyTranslationModel {
        fn descriptor(&self) -> TranslationModelDescriptor {
            TranslationModelDescriptor {
                model_id: "synthetic-flaky-test".into(),
                model_sha256: format!("sha256:{}", "9".repeat(64)),
                adapter_id: "memex.synthetic-flaky-test".into(),
                adapter_version: "1".into(),
                request_timeout: Duration::from_secs(1),
            }
        }

        fn translate(
            &mut self,
            input: TranslationModelInput<'_>,
        ) -> std::result::Result<String, TranslationModelError> {
            self.calls += 1;
            if self.calls == 1 {
                return Err(TranslationModelError::Timeout);
            }
            Ok(match input.purpose() {
                GeneratedTextPurpose::TitleTranslation => "自动重试后的合成文章翻译".into(),
                GeneratedTextPurpose::DocumentTranslation => {
                    "# 自动重试后的合成翻译\n\n这段无害的合成中文证明临时模型失败会自动重试并最终成功。".into()
                }
                _ => return Err(TranslationModelError::ProviderFailure),
            })
        }
    }

    #[test]
    fn framing_bounds() {
        let mut b = Vec::new();
        write_frame(&mut b, b"{}").unwrap();
        assert_eq!(read_frame(&mut b.as_slice()).unwrap(), b"{}");
        assert!(matches!(
            serde_json::from_slice::<Command>(br#"{"command":"list"}"#).unwrap(),
            Command::List { cursor: None }
        ));
        assert!(matches!(
            serde_json::from_slice::<Command>(
                br#"{"command":"item_read","item_id":"item_synthetic"}"#
            )
            .unwrap(),
            Command::ItemRead { item_id } if item_id == "item_synthetic"
        ));
        assert!(serde_json::from_slice::<Command>(
            br#"{"command":"item_read","item_id":"item_synthetic","url":"private"}"#
        )
        .is_err());
        assert!(matches!(
            serde_json::from_slice::<Command>(br#"{"command":"remote_item_read","job_id":"job_synthetic","item_id":"item_synthetic"}"#).unwrap(),
            Command::RemoteItemRead { job_id, item_id } if job_id == "job_synthetic" && item_id == "item_synthetic"
        ));
        assert!(matches!(
            serde_json::from_slice::<Command>(br#"{"command":"library_list","cursor":null}"#)
                .unwrap(),
            Command::LibraryList { cursor: None }
        ));
        assert!(matches!(
            serde_json::from_slice::<Command>(
                br#"{"command":"library_item_read","item_id":"item_synthetic"}"#
            )
            .unwrap(),
            Command::LibraryItemRead { item_id } if item_id == "item_synthetic"
        ));
        assert!(matches!(
            serde_json::from_slice::<Command>(
                br#"{"command":"update_item_metadata","item_id":"item_synthetic","expected_revision_id":"revision_synthetic","title":"Synthetic title","manual_tags":["one"]}"#
            )
            .unwrap(),
            Command::UpdateItemMetadata { item_id, expected_revision_id, title, manual_tags }
                if item_id == "item_synthetic"
                    && expected_revision_id == "revision_synthetic"
                    && title == "Synthetic title"
                    && manual_tags == ["one"]
        ));
        assert!(serde_json::from_slice::<Command>(
            br#"{"command":"remote_status","job_id":"job_synthetic","extra":true}"#
        )
        .is_err());
        let over = ((MAX_REQUEST_FRAME + 1) as u32).to_be_bytes().to_vec();
        assert_eq!(
            read_frame(&mut over.as_slice()).unwrap_err().to_string(),
            "frame_too_large"
        );
        let response_over = ((MAX_RESPONSE_FRAME + 1) as u32).to_be_bytes().to_vec();
        assert_eq!(
            read_response_frame(&mut response_over.as_slice())
                .unwrap_err()
                .to_string(),
            "frame_too_large"
        );
        assert_eq!(
            Response::error("https://secret.test/a?q=x")
                .reason_code
                .unwrap(),
            "internal_error"
        );
        assert_eq!(
            Response::error("invalid_cursor").reason_code.as_deref(),
            Some("invalid_cursor")
        );
        assert_eq!(
            Response::error("library_scan_limit_exceeded")
                .reason_code
                .as_deref(),
            Some("library_scan_limit_exceeded")
        );
        let old: Response = serde_json::from_str(
            r#"{"ok":true,"job":null,"jobs":null,"next_cursor":null,"reason_code":null}"#,
        )
        .unwrap();
        assert!(old.item.is_none());
        assert!(old.library_items.is_none());
        assert!(old.library_item.is_none());
        assert!(old.item_update.is_none());
        assert!(old.context.is_none());
        assert!(serde_json::to_value(&old).unwrap().get("context").is_none());

        let escaped = Response {
            ok: true,
            job: None,
            jobs: None,
            next_cursor: None,
            reason_code: None,
            item: Some(ItemResult {
                item_id: "item_synthetic".into(),
                revision_id: "rev_synthetic".into(),
                title: "Synthetic".into(),
                markdown: "\\".repeat(MAX_MARKDOWN_BYTES),
                manual_tags: None,
            }),
            library_items: None,
            library_item: None,
            item_update: None,
            context: None,
            library_query: None,
            library_entry: None,
            model_settings: None,
            provider_credential: None,
            provider_models: None,
            translation: None,
            bibliographic_metadata: None,
            identifier_duplicates: None,
            lookup_preview: None,
            lookup_value: None,
            group_tree: None,
            generation_test: None,
            capabilities: None,
            group_edit: None,
            ..crate::ipc::Response::empty_ok()
        };
        let bounded: Response = serde_json::from_slice(&response_body(&escaped).unwrap()).unwrap();
        assert!(!bounded.ok);
        assert_eq!(bounded.reason_code.as_deref(), Some("result_too_large"));
    }

    #[test]
    fn model_settings_over_owner_socket_persist_without_changing_old_responses() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let socket = root_path.join("ipc/agent.sock");
        let listener = bind_owner_socket(&socket).unwrap();
        let configuration = serde_json::json!({
            "providers": [{"id":"synthetic", "label":"Synthetic proxy", "api_base":"http://127.0.0.1:9/v1/"}],
            "models": [{"id":"one", "label":"One", "provider_id":"synthetic", "model_id":"synthetic-unavailable"}],
            "selections": [{"purpose":"chat", "model":"one"}]
        });
        let save = serde_json::json!({"command":"model_settings_replace", "expected_revision":0, "configuration":configuration});
        let mut secret = save.clone();
        secret["expected_revision"] = 1.into();
        secret["configuration"]["providers"][0]["api_key"] = "synthetic-do-not-echo".into();
        let commands = [
            serde_json::json!({"command":"model_settings_read"}),
            save.clone(),
            save,
            secret,
            serde_json::json!({"command":"model_settings_read"}),
            serde_json::json!({"command":"list", "cursor":null}),
        ];
        let server_root = root_path.clone();
        let server = std::thread::spawn(move || {
            for _ in 0..6 {
                // Reopen between requests to prove the result is persisted, not UI state.
                let mut queue = Queue::open(&server_root.join("state/agent.db")).unwrap();
                let mut queries = memex_library::LibraryQuery::new(&server_root).unwrap();
                let (mut stream, _) = listener.accept().unwrap();
                serve(
                    &mut stream,
                    &mut queue,
                    &server_root,
                    &mut queries,
                    &offline_network(),
                )
                .unwrap();
            }
        });
        let responses: Vec<serde_json::Value> = commands
            .iter()
            .map(|command| {
                let mut stream = UnixStream::connect(&socket).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                write_frame(&mut stream, &serde_json::to_vec(command).unwrap()).unwrap();
                serde_json::from_slice(&read_response_frame(&mut stream).unwrap()).unwrap()
            })
            .collect();
        server.join().unwrap();
        assert_eq!(responses[0]["model_settings"]["revision"], 0);
        assert_eq!(responses[1]["ok"], true);
        assert_eq!(responses[1]["model_settings"]["revision"], 1);
        assert_eq!(responses[2]["reason_code"], "stale_model_settings");
        assert_eq!(responses[3]["ok"], false);
        assert!(!responses[3].to_string().contains("synthetic-do-not-echo"));
        assert_eq!(
            responses[4]["model_settings"]["configuration"],
            configuration
        );
        assert_eq!(responses[4]["model_settings"]["revision"], 1);
        assert_eq!(responses[5]["ok"], true);
        for index in [2, 3, 5] {
            assert!(responses[index].get("model_settings").is_none());
        }
    }

    #[test]
    fn context_read_over_owner_socket_requires_a_ceiling_and_preserves_old_wire_shape() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let item =
            synthetic_stored_item_at_sensitivity(&root_path, "context", Sensitivity::Private);
        let (_, revision) = memex_store::read_item(&root_path, &item).unwrap();
        let request = serde_json::json!({
            "command": "context_read", "item_id": item,
            "expected_revision_id": revision.revision_id,
            "start_byte": 2, "end_byte": 11, "max_sensitivity": "private"
        });
        let mut denied = request.clone();
        denied["max_sensitivity"] = "public".into();
        let mut missing_ceiling = request.clone();
        missing_ceiling
            .as_object_mut()
            .unwrap()
            .remove("max_sensitivity");
        let mut stale = request.clone();
        stale["expected_revision_id"] = "rev_stale".into();
        let mut unexpected = request.clone();
        unexpected["provider"] = "must-not-call".into();
        let commands = [
            request,
            denied,
            missing_ceiling,
            stale,
            unexpected,
            serde_json::json!({"command": "library_list", "cursor": null}),
        ];
        let socket = root_path.join("ipc/agent.sock");
        let listener = bind_owner_socket(&socket).unwrap();
        let server_root = root_path.clone();
        let server = std::thread::spawn(move || {
            let mut queue = Queue::open(&server_root.join("state/agent.db")).unwrap();
            let mut queries = memex_library::LibraryQuery::new(&server_root).unwrap();
            for _ in 0..6 {
                let (mut stream, _) = listener.accept().unwrap();
                serve(
                    &mut stream,
                    &mut queue,
                    &server_root,
                    &mut queries,
                    &offline_network(),
                )
                .unwrap();
            }
        });
        let responses: Vec<serde_json::Value> = commands
            .iter()
            .map(|command| {
                let mut stream = UnixStream::connect(&socket).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                write_frame(&mut stream, &serde_json::to_vec(command).unwrap()).unwrap();
                serde_json::from_slice(&read_response_frame(&mut stream).unwrap()).unwrap()
            })
            .collect();
        server.join().unwrap();
        assert_eq!(responses[0]["ok"], true);
        assert_eq!(responses[0]["context"]["content_markdown"], "Synthetic");
        assert_eq!(responses[0]["context"]["input"]["item_id"], item);
        assert_eq!(
            responses[0]["context"]["input"]["revision_id"],
            revision.revision_id
        );
        for (response, code) in responses[1..5].iter().zip([
            "context_resource_not_found",
            "internal_error",
            "stale_item_revision",
            "internal_error",
        ]) {
            assert_eq!(response["ok"], false);
            assert_eq!(response["reason_code"], code);
            assert!(response.get("context").is_none());
        }
        assert_eq!(responses[5]["ok"], true);
        assert!(responses[5].get("context").is_none());
    }

    #[test]
    fn reader_over_owner_socket_is_bounded_and_persists_shared_state() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let item = synthetic_stored_item_at_sensitivity(&root, "reader", Sensitivity::Private);
        let (_, revision) = memex_store::read_item(&root, &item).unwrap();
        let socket = root.join("ipc/reader.sock");
        let listener = bind_owner_socket(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let mut queue = Queue::open(&root.join("state/agent.db")).unwrap();
            let mut queries = memex_library::LibraryQuery::new(&root).unwrap();
            for _ in 0..7 {
                let (mut stream, _) = listener.accept().unwrap();
                serve(&mut stream, &mut queue, &root, &mut queries, &offline_network()).unwrap();
            }
        });
        let exchange = |request: serde_json::Value, ceiling: &str| -> serde_json::Value {
            let command: Command = serde_json::from_value(serde_json::json!({"command":"reader", "max_sensitivity":ceiling, "request":request})).unwrap();
            serde_json::to_value(send_command(&socket, &command).unwrap()).unwrap()
        };
        let toc = exchange(serde_json::json!({"op":"toc", "item_id":item, "revision_id":revision.revision_id}), "private");
        assert_eq!(toc["ok"], true);
        assert!(toc["reader"].get("content_markdown").is_none());
        let chunk = exchange(serde_json::json!({"op":"chunk", "item_id":item, "revision_id":revision.revision_id, "side":"source", "chapter":"section-0", "offset":0, "limit":5}), "private");
        assert_eq!(chunk["reader"]["markdown"], "# Syn");
        assert_eq!(chunk["reader"]["next_offset"], 5);
        let initial = exchange(serde_json::json!({"op":"state", "item_id":item}), "private");
        let state = serde_json::json!({"visit_id":"mac-open-1", "bookmarks":[], "position":{
            "revision_id":revision.revision_id, "side":"source", "sha256":toc["reader"]["editions"][0]["sha256"], "chapter":"section-0", "offset":5
        }});
        assert_eq!(exchange(serde_json::json!({"op":"save", "item_id":item, "expected_version":initial["reader"]["version"], "state":state}), "private")["ok"], true);
        assert_eq!(exchange(serde_json::json!({"op":"resume"}), "private")["reader"], serde_json::json!([item, state]));
        let denied = exchange(serde_json::json!({"op":"toc", "item_id":item, "revision_id":revision.revision_id}), "public");
        assert_eq!(denied["reason_code"], "library_resource_not_found");
        assert!(denied.get("reader").is_none());
        assert!(exchange(serde_json::json!({"op":"resume"}), "public")["reader"].is_null());
        server.join().unwrap();
    }

    #[test]
    fn scoped_results_recheck_current_private_ceiling_after_success() {
        use sha2::{Digest, Sha256};
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let id = synthetic_stored_item_at_sensitivity(
            &root,
            "translated-library",
            Sensitivity::Internal,
        );
        let (mut item, mut revision) = memex_store::read_item(&root, &id).unwrap();
        let mut queue = Queue::open(&root.join("agent.db")).unwrap();
        let principal = "a".repeat(64);
        let job = queue
            .scoped_enqueue(
                &principal,
                &format!("remote_mobile_{}", "A".repeat(32)),
                "https://example.invalid/article",
            )
            .unwrap();
        let leased = queue.lease(60).unwrap().unwrap();
        queue
            .finish_stored(&leased, &id, &revision.revision_id)
            .unwrap();
        memex_analysis::enqueue_item_translation(&root, &id, &revision.revision_id).unwrap();
        assert!(TranslationQueue::open(&root)
            .unwrap()
            .process_next(&mut SyntheticTranslationModel)
            .unwrap());
        let commands = |ceiling| {
            [
                Command::ScopedCaptureStatus {
                    principal: principal.clone(),
                    job_id: job.job_id.clone(),
                    max_sensitivity: ceiling,
                },
                Command::ScopedItemRead {
                    item_id: id.clone(),
                    max_sensitivity: ceiling,
                },
                Command::ScopedTranslationStatus {
                    item_id: id.clone(),
                    max_sensitivity: ceiling,
                },
                Command::ScopedTranslationRead {
                    item_id: id.clone(),
                    expected_revision_id: revision.revision_id.clone(),
                    start_byte: 0,
                    max_sensitivity: ceiling,
                },
            ]
        };
        for command in commands(Sensitivity::Internal) {
            assert!(dispatch(&mut queue, &root, command).is_ok());
        }
        // Construct a valid higher-ceiling legacy Store snapshot in this disposable
        // fixture. No product ACL mutation API is implied; remove only its completed
        // ingest repair journal, and refresh both typed-object integrity hashes.
        fs::remove_dir_all(root.join("catalog/ingest-journal")).unwrap();
        item.sensitivity = Sensitivity::Private;
        revision.sensitivity = Sensitivity::Private;
        let item_bytes = serde_json::to_vec(&item).unwrap();
        let revision_bytes = serde_json::to_vec(&revision).unwrap();
        fs::write(root.join("items").join(&id).join("item.json"), &item_bytes).unwrap();
        fs::write(
            root.join("items")
                .join(&id)
                .join("revisions")
                .join(format!("{}.json", revision.revision_id)),
            &revision_bytes,
        )
        .unwrap();
        let integrity = serde_json::json!({"schema_version":1,"item_id":id,"current_revision":revision.revision_id,"sensitivity":"private","item_sha256":format!("sha256:{:x}",Sha256::digest(&item_bytes)),"revision_sha256":format!("sha256:{:x}",Sha256::digest(&revision_bytes))});
        fs::write(
            root.join("integrity/items").join(format!("{id}.json")),
            serde_json::to_vec(&integrity).unwrap(),
        )
        .unwrap();
        // Positive control excludes a denial caused merely by corrupt fixture data.
        let readable = memex_library::read_entry(&root, &id, Sensitivity::Private).unwrap();
        assert!(readable.markdown.contains("harmless article"));
        for command in commands(Sensitivity::Internal) {
            let error = dispatch(&mut queue, &root, command).unwrap_err();
            let response = Response::error(&error.to_string());
            assert!(!response.ok);
            assert_ne!(response.reason_code.as_deref(), Some("internal_error"));
            assert!(
                response.translation.is_none()
                    && response.library_entry.is_none()
                    && response.job.is_none()
            );
        }
    }

    #[test]
    fn scoped_translation_status_waits_for_durable_current_revision_hook() {
        let root = tempfile::tempdir().unwrap();
        let library = root.path().canonicalize().unwrap();
        let item = synthetic_stored_item_at_sensitivity(
            &library,
            "translated-library",
            Sensitivity::Internal,
        );
        let (_, revision) = memex_store::read_item(&library, &item).unwrap();
        let database = library.join("agent.db");
        let mut queue = Queue::open(&database).unwrap();
        let status = |queue: &mut Queue, ceiling| {
            dispatch(
                queue,
                &library,
                Command::ScopedTranslationStatus {
                    item_id: item.clone(),
                    max_sensitivity: ceiling,
                },
            )
        };
        assert_eq!(
            status(&mut queue, Sensitivity::Internal)
                .unwrap()
                .translation
                .unwrap()
                .state,
            "not_requested"
        );
        queue
            .enqueue("test", "pending-hook", "https://example.invalid/hook")
            .unwrap();
        let leased = queue.lease(60).unwrap().unwrap();
        let hook_database = database.clone();
        let hook_item = item.clone();
        let hook_revision = revision.revision_id.clone();
        AFTER_HOOK_ABSENCE.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                Queue::open(&hook_database)
                    .unwrap()
                    .finish_stored(&leased, &hook_item, &hook_revision)
                    .unwrap();
            }))
        });
        // Force the exact formerly missing interleaving on another SQLite connection.
        assert_eq!(
            status(&mut queue, Sensitivity::Internal)
                .unwrap()
                .translation
                .unwrap()
                .state,
            "awaiting_enqueue"
        );
        let intent = queue.pending_translation_intent().unwrap().unwrap();
        queue.defer_translation_intent(&intent).unwrap();
        drop(queue);
        let mut queue = Queue::open(&database).unwrap();
        let pending = status(&mut queue, Sensitivity::Internal)
            .unwrap()
            .translation
            .unwrap();
        assert_eq!(pending.state, "awaiting_enqueue");
        assert_eq!(pending.reason.as_deref(), Some("capture_hook_pending"));
        assert_eq!(pending.revision_id, revision.revision_id);
        assert!(status(&mut queue, Sensitivity::Public).is_err());
        assert!(
            memex_analysis::translation_status(&library, &item, &revision.revision_id)
                .unwrap()
                .is_none()
        );
        memex_analysis::enqueue_item_translation(&library, &item, &revision.revision_id).unwrap();
        // The hook enqueues and authorizes before acknowledging.
        TranslationQueue::open(&library)
            .unwrap()
            .authorize(&item, &revision.revision_id)
            .unwrap();
        // Analysis exists before the durable hook acknowledgment, so expose its real state.
        assert_eq!(
            status(&mut queue, Sensitivity::Internal)
                .unwrap()
                .translation
                .unwrap()
                .state,
            "queued"
        );
        queue.complete_translation_intent(&intent).unwrap();
        assert_eq!(
            status(&mut queue, Sensitivity::Internal)
                .unwrap()
                .translation
                .unwrap()
                .state,
            "queued"
        );
        memex_store::revise_item_metadata(
            &library,
            &item,
            memex_store::ItemMetadataUpdate {
                expected_revision_id: revision.revision_id.clone(),
                title: "Changed revision".into(),
                manual_tags: vec![],
            },
        )
        .unwrap();
        // A pending old-revision intent cannot keep the new revision awaiting enqueue.
        queue
            .enqueue("test", "old-hook", "https://example.invalid/old-hook")
            .unwrap();
        let old = queue.lease(60).unwrap().unwrap();
        queue
            .finish_stored(&old, &item, &revision.revision_id)
            .unwrap();
        let current = status(&mut queue, Sensitivity::Internal)
            .unwrap()
            .translation
            .unwrap();
        assert_ne!(current.revision_id, revision.revision_id);
        assert_eq!(current.state, "not_requested");
    }

    #[test]
    fn library_query_over_owner_socket_refreshes_and_rejects_stale_windows() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let a = synthetic_stored_item(&root_path, "A");
        let b = synthetic_stored_item(&root_path, "B");
        synthetic_stored_item_at_sensitivity(&root_path, "private", Sensitivity::Private);
        let socket = root_path.join("ipc/agent.sock");
        let listener = bind_owner_socket(&socket).unwrap();
        let server_root = root_path.clone();
        let server = std::thread::spawn(move || {
            let mut queue = Queue::open(&server_root.join("state/agent.db")).unwrap();
            let mut queries = memex_library::LibraryQuery::new(&server_root).unwrap();
            for _ in 0..5 {
                let (mut stream, _) = listener.accept().unwrap();
                serve(
                    &mut stream,
                    &mut queue,
                    &server_root,
                    &mut queries,
                    &offline_network(),
                )
                .unwrap();
            }
        });
        let exchange = |command: &serde_json::Value| -> serde_json::Value {
            let mut stream = UnixStream::connect(&socket).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            write_frame(&mut stream, &serde_json::to_vec(command).unwrap()).unwrap();
            serde_json::from_slice(&read_response_frame(&mut stream).unwrap()).unwrap()
        };
        let mut request = serde_json::json!({
            "command":"library_query", "query": {
                "text":"", "source":null, "item_type":null, "sort":"title", "descending":false
            }, "max_sensitivity":"public", "offset":0, "limit":1, "expected_view":null
        });
        let first = exchange(&request);
        assert_eq!(first["ok"], true);
        assert_eq!(first["library_query"]["total"], 2);
        assert_eq!(first["library_query"]["sources"][0]["count"], 2);
        assert_eq!(first["library_query"]["items"][0]["item_id"], a);
        request["expected_view"] = first["library_query"]["view"].clone();
        request["offset"] = 1.into();
        assert_eq!(
            exchange(&request)["library_query"]["items"][0]["item_id"],
            b
        );
        let (_, revision) = memex_store::read_item(&root_path, &a).unwrap();
        memex_store::revise_item_metadata(
            &root_path,
            &a,
            memex_store::ItemMetadataUpdate {
                expected_revision_id: revision.revision_id,
                title: "ZZZ".into(),
                manual_tags: vec![],
            },
        )
        .unwrap();
        let stale = exchange(&request);
        assert_eq!(stale["ok"], false);
        assert_eq!(stale["reason_code"], "stale_query_view");
        assert!(stale.get("library_query").is_none());
        request.as_object_mut().unwrap().remove("max_sensitivity");
        let denied = exchange(&request);
        assert_eq!(denied["ok"], false);
        assert!(denied.get("library_query").is_none());
        let old = exchange(&serde_json::json!({"command":"library_list", "cursor":null}));
        assert_eq!(old["ok"], true);
        assert!(old.get("library_query").is_none());
        server.join().unwrap();
    }

    #[test]
    fn socket_is_owner_only_and_responses_never_echo_full_urls() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let socket = root_path.join("ipc/agent.sock");
        let listener = bind_owner_socket(&socket).unwrap();
        let metadata = fs::symlink_metadata(&socket).unwrap();
        assert!(metadata.file_type().is_socket());
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        drop(listener);
        fs::remove_file(&socket).unwrap();

        let mut queue = Queue::open(&root_path.join("state/agent.db")).unwrap();
        let sensitive = "https://article.test/nested/path?access_token=synthetic-secret";
        let response = dispatch(
            &mut queue,
            root.path(),
            Command::Enqueue {
                producer: Producer::LocalSurface,
                producer_job_id: "synthetic_1".into(),
                url: sensitive.into(),
            },
        )
        .unwrap();
        let output = serde_json::to_string(&response).unwrap();
        assert!(!output.contains(sensitive));
        assert!(!output.contains("access_token"));
        assert!(!output.contains("synthetic-secret"));

        let error = dispatch(
            &mut queue,
            root.path(),
            Command::ItemRead {
                item_id: "https://private.test/body-secret".into(),
            },
        )
        .unwrap_err();
        let response = Response::error(&error.to_string());
        let output = serde_json::to_string(&response).unwrap();
        assert!(output.contains("item_not_found"));
        assert!(!output.contains("private.test"));
        assert!(!output.contains("body-secret"));
    }

    #[test]
    fn remote_commands_hide_unrelated_local_resources() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let mut queue = Queue::open(&root_path.join("agent.db")).unwrap();
        let local = queue
            .enqueue("local_surface", "local", "https://local.test/one")
            .unwrap();
        for command in [
            Command::RemoteStatus {
                job_id: local.job_id.clone(),
            },
            Command::RemoteItemRead {
                job_id: local.job_id,
                item_id: "item_guessed".into(),
            },
            Command::RemoteItemRead {
                job_id: "job_guessed".into(),
                item_id: "item_guessed".into(),
            },
        ] {
            let error = dispatch(&mut queue, &root_path, command).unwrap_err();
            assert_eq!(
                Response::error(&error.to_string()).reason_code.as_deref(),
                Some("remote_resource_not_found")
            );
        }
    }

    #[test]
    fn library_list_publication_summary_reads_current_rss_revision() {
        use sha2::{Digest, Sha256};
        use memex_store::podcast_metadata::{fill_podcast_metadata, PodcastMetadataFill, PodcastRssSource};
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let item_id = synthetic_stored_item(&root_path, "publication-summary");
        let before = library_page(&root_path, None, 10).unwrap().0.remove(0);
        let json = serde_json::to_value(&before).unwrap();
        assert!(json.as_object().unwrap().contains_key("published_at"));
        assert!(json.as_object().unwrap().contains_key("duration_seconds"));
        assert!(json["published_at"].is_null() && json["duration_seconds"].is_null());
        let url = "https://example.invalid/publication-summary";
        let raw = "Fri, 06 Nov 2026 23:00:00 -0800";
        let rss = format!("<rss><item><guid>summary</guid><link>{url}</link><pubDate>{raw}</pubDate><duration>1:02:03</duration></item></rss>");
        fill_podcast_metadata(&root_path, PodcastMetadataFill {
            item_id, expected_revision_id: before.revision_id.clone(), request_id: "summary-read".into(),
            source: PodcastRssSource {
                feed_url: "https://example.invalid/feed".into(), episode_url: url.into(), guid: "summary".into(),
                rss_sha256: format!("sha256:{:x}", Sha256::digest(rss.as_bytes())),
                source_start_byte: rss.find("<item>").unwrap(), source_end_byte: rss.find("</item>").unwrap() + 7,
                published_date: Some(memex_core::PartialDate { value: Some("2026-11-06".into()), precision: memex_core::DatePrecision::Day, raw: Some(raw.into()) }),
                published_raw: Some(raw.into()), published_instant: Some("2026-11-07T07:00:00Z".into()),
                duration_seconds: Some(3723), duration_raw: Some("1:02:03".into()),
            },
        }, rss.as_bytes(), Sensitivity::Public).unwrap();
        let after = library_page(&root_path, None, 10).unwrap().0.remove(0);
        assert_ne!(after.revision_id, before.revision_id);
        assert_eq!(after.published_at.as_deref(), Some("2026-11-06"));
        assert_eq!(after.duration_seconds, Some(3723));
        assert_eq!(after.captured_at, before.captured_at);
    }

    #[test]
    fn remote_library_lists_all_owner_sensitivities_and_reads_without_weakening_job_auth() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let public_item = synthetic_stored_item(&root_path, "library-public");
        let restricted_item = synthetic_stored_item_at_sensitivity(
            &root_path,
            "library-restricted",
            Sensitivity::Restricted,
        );
        let mut queue = Queue::open(&root_path.join("state/agent.db")).unwrap();

        let (first, next_cursor) = library_page(&root_path, None, 1).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(next_cursor.as_deref(), Some(first[0].item_id.as_str()));
        let (second, final_cursor) = library_page(&root_path, next_cursor.as_deref(), 1).unwrap();
        assert_eq!(second.len(), 1);
        assert!(final_cursor.is_none());
        assert!(first[0].item_id > second[0].item_id);
        let listed: std::collections::BTreeSet<_> = first
            .iter()
            .chain(second.iter())
            .map(|item| item.item_id.as_str())
            .collect();
        assert_eq!(
            listed,
            std::collections::BTreeSet::from([public_item.as_str(), restricted_item.as_str(),])
        );
        assert!(first
            .iter()
            .chain(second.iter())
            .all(|item| item.item_type == "webpage" && item.captured_at.is_some()));
        assert_eq!(
            library_page(&root_path, Some("item_missing"), 1)
                .unwrap_err()
                .to_string(),
            "invalid_cursor"
        );
        assert_eq!(
            Response::error("invalid_cursor").reason_code.as_deref(),
            Some("invalid_cursor")
        );

        let page = dispatch(
            &mut queue,
            &root_path,
            Command::LibraryList { cursor: None },
        )
        .unwrap();
        assert_eq!(page.library_items.unwrap().len(), 2);
        assert!(page.next_cursor.is_none());

        let read = dispatch(
            &mut queue,
            &root_path,
            Command::LibraryItemRead {
                item_id: restricted_item.clone(),
            },
        )
        .unwrap()
        .library_item
        .unwrap();
        assert_eq!(read.item_id, restricted_item);
        assert!(matches!(
            read.translation_state,
            LibraryTranslationState::NotRequested
        ));
        assert!(read.title.is_none());
        assert!(read.markdown.is_none());

        let missing = dispatch(
            &mut queue,
            &root_path,
            Command::LibraryItemRead {
                item_id: "item_missing".into(),
            },
        )
        .unwrap_err();
        assert_eq!(missing.to_string(), "library_resource_not_found");

        let local = queue
            .enqueue(
                "local_surface",
                "library-local",
                "https://local.test/library",
            )
            .unwrap();
        let unauthorized = dispatch(
            &mut queue,
            &root_path,
            Command::RemoteItemRead {
                job_id: local.job_id,
                item_id: public_item,
            },
        )
        .unwrap_err();
        assert_eq!(unauthorized.to_string(), "remote_resource_not_found");
    }

    #[test]
    fn owner_metadata_update_round_trips_through_the_owner_socket_contract() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let item_id = synthetic_stored_item(&root_path, "desktop-metadata");
        let mut queue = Queue::open(&root_path.join("state/agent.db")).unwrap();
        let before = dispatch(
            &mut queue,
            &root_path,
            Command::ItemRead {
                item_id: item_id.clone(),
            },
        )
        .unwrap()
        .item
        .unwrap();
        assert_eq!(before.manual_tags, Some(Vec::new()));

        let receipt = dispatch(
            &mut queue,
            &root_path,
            Command::UpdateItemMetadata {
                item_id: item_id.clone(),
                expected_revision_id: before.revision_id.clone(),
                title: "Desktop edited title".into(),
                manual_tags: vec!["two".into(), "one".into()],
            },
        )
        .unwrap()
        .item_update
        .unwrap();
        assert_eq!(receipt.item_id, item_id);
        assert_eq!(receipt.previous_revision_id, before.revision_id);
        assert_eq!(receipt.revision_number, 2);

        let after = dispatch(
            &mut queue,
            &root_path,
            Command::ItemRead {
                item_id: item_id.clone(),
            },
        )
        .unwrap()
        .item
        .unwrap();
        assert_eq!(after.revision_id, receipt.revision_id);
        assert_eq!(after.title, "Desktop edited title");
        assert_eq!(after.manual_tags, Some(vec!["one".into(), "two".into()]));
        assert_eq!(
            memex_analysis::translation_status(&root_path, &item_id, &after.revision_id)
                .unwrap()
                .unwrap()
                .state,
            TranslationJobState::Queued
        );
        assert_eq!(
            memex_analysis::summary_status(&root_path, &item_id, &after.revision_id)
                .unwrap()
                .unwrap()
                .state,
            SummaryJobState::Queued
        );

        let stale = dispatch(
            &mut queue,
            &root_path,
            Command::UpdateItemMetadata {
                item_id,
                expected_revision_id: before.revision_id,
                title: "Stale edit".into(),
                manual_tags: Vec::new(),
            },
        )
        .unwrap_err();
        assert_eq!(stale.to_string(), "stale_item_revision");
        assert_eq!(
            Response::error(&stale.to_string()).reason_code.as_deref(),
            Some("stale_item_revision")
        );
    }

    #[cfg(unix)]
    #[test]
    fn owner_metadata_update_is_reconciled_after_a_transient_analysis_failure() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let item_id = synthetic_stored_item(&root_path, "desktop-metadata");
        let (_, before) = memex_store::read_item(&root_path, &item_id).unwrap();
        symlink(root_path.join("state"), root_path.join("analysis")).unwrap();
        let mut queue = Queue::open(&root_path.join("state/agent.db")).unwrap();

        let response = dispatch(
            &mut queue,
            &root_path,
            Command::UpdateItemMetadata {
                item_id: item_id.clone(),
                expected_revision_id: before.revision_id,
                title: "Durably edited title".into(),
                manual_tags: vec!["durable".into()],
            },
        )
        .unwrap();
        let receipt = response.item_update.unwrap();
        let (_, after) = memex_store::read_item(&root_path, &item_id).unwrap();
        assert_eq!(after.revision_id, receipt.revision_id);
        assert_eq!(after.title, "Durably edited title");
        assert!(
            memex_analysis::translation_status(&root_path, &item_id, &after.revision_id).is_err()
        );
        fs::remove_file(root_path.join("analysis")).unwrap();
        let mut reconciler = AnalysisReconciler::new(Duration::ZERO);
        assert!(reconciler.process_next_page(&root_path).unwrap());
        assert_eq!(
            memex_analysis::translation_status(&root_path, &item_id, &after.revision_id)
                .unwrap()
                .unwrap()
                .state,
            TranslationJobState::Queued
        );
        assert_eq!(
            memex_analysis::summary_status(&root_path, &item_id, &after.revision_id)
                .unwrap()
                .unwrap()
                .state,
            SummaryJobState::Queued
        );
    }

    #[test]
    fn store_only_current_revision_is_reconciled_into_both_analysis_queues() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let item_id = synthetic_stored_item(&root_path, "translated-library");
        let (_, revision) = memex_store::read_item(&root_path, &item_id).unwrap();
        assert!(
            memex_analysis::translation_status(&root_path, &item_id, &revision.revision_id)
                .unwrap()
                .is_none()
        );
        assert!(
            memex_analysis::summary_status(&root_path, &item_id, &revision.revision_id)
                .unwrap()
                .is_none()
        );

        let mut reconciler = AnalysisReconciler::new(Duration::ZERO);
        assert!(reconciler.process_next_page(&root_path).unwrap());
        assert_eq!(
            memex_analysis::translation_status(&root_path, &item_id, &revision.revision_id)
                .unwrap()
                .unwrap()
                .state,
            TranslationJobState::Queued
        );
        assert_eq!(
            memex_analysis::summary_status(&root_path, &item_id, &revision.revision_id)
                .unwrap()
                .unwrap()
                .state,
            SummaryJobState::Queued
        );
    }

    #[test]
    fn owner_library_read_exposes_only_validated_current_generated_analysis() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let item_id = synthetic_stored_item(&root_path, "translated-library");
        let (_, revision) = memex_store::read_item(&root_path, &item_id).unwrap();
        let outcome =
            memex_analysis::enqueue_item_translation(&root_path, &item_id, &revision.revision_id)
                .unwrap();
        assert!(matches!(
            outcome,
            memex_analysis::TranslationEnqueueOutcome::Enqueued { .. }
        ));
        assert!(matches!(
            memex_analysis::enqueue_item_summary(&root_path, &item_id, &revision.revision_id,)
                .unwrap(),
            memex_analysis::SummaryEnqueueOutcome::Enqueued { .. }
        ));
        let mut capture_queue = Queue::open(&root_path.join("state/agent.db")).unwrap();
        let read = |queue: &mut Queue| {
            dispatch(
                queue,
                &root_path,
                Command::LibraryItemRead {
                    item_id: item_id.clone(),
                },
            )
            .unwrap()
            .library_item
            .unwrap()
        };
        // Reconciled but unrequested jobs never run, so they read as not requested.
        let unrequested = read(&mut capture_queue);
        assert!(matches!(
            unrequested.translation_state,
            LibraryTranslationState::NotRequested
        ));
        assert!(matches!(
            unrequested.summary_state,
            LibrarySummaryState::NotRequested
        ));
        TranslationQueue::open(&root_path)
            .unwrap()
            .authorize(&item_id, &revision.revision_id)
            .unwrap();
        SummaryQueue::open(&root_path)
            .unwrap()
            .authorize(&item_id, &revision.revision_id)
            .unwrap();

        let queued = dispatch(
            &mut capture_queue,
            &root_path,
            Command::LibraryItemRead {
                item_id: item_id.clone(),
            },
        )
        .unwrap()
        .library_item
        .unwrap();
        assert!(matches!(
            queued.translation_state,
            LibraryTranslationState::Queued
        ));
        assert!(matches!(queued.summary_state, LibrarySummaryState::Queued));
        assert!(queued.summary.is_none());
        assert!(queued.title.is_none() && queued.markdown.is_none());

        let mut translation_queue = TranslationQueue::open(&root_path).unwrap();
        assert!(translation_queue
            .process_next(&mut SyntheticTranslationModel)
            .unwrap());
        let mut summary_queue = SummaryQueue::open(&root_path).unwrap();
        assert!(summary_queue
            .process_next(&mut SyntheticSummaryModel)
            .unwrap());
        let status =
            memex_analysis::translation_status(&root_path, &item_id, &revision.revision_id)
                .unwrap()
                .unwrap();
        assert_eq!(status.state, TranslationJobState::Succeeded);
        assert!(memex_analysis::validated_translation_pair(
            &root_path,
            &item_id,
            &revision.revision_id,
        )
        .unwrap()
        .is_some());
        assert!(memex_analysis::validated_translation_pair(
            &root_path,
            &item_id,
            "revision_stale",
        )
        .unwrap()
        .is_none());
        assert!(
            memex_analysis::validated_summary(&root_path, &item_id, &revision.revision_id,)
                .unwrap()
                .is_some()
        );

        let translated = dispatch(
            &mut capture_queue,
            &root_path,
            Command::LibraryItemRead {
                item_id: item_id.clone(),
            },
        )
        .unwrap()
        .library_item
        .unwrap();
        assert_eq!(translated.item_id, item_id);
        assert_eq!(translated.revision_id, revision.revision_id);
        assert!(matches!(
            translated.translation_state,
            LibraryTranslationState::Succeeded
        ));
        assert!(matches!(
            translated.summary_state,
            LibrarySummaryState::Succeeded
        ));
        assert!(translated
            .summary
            .as_deref()
            .is_some_and(|summary| summary.contains("确定性验证")));
        assert_eq!(translated.title.as_deref(), Some("合成远程文章翻译结果"));
        assert!(translated
            .markdown
            .as_deref()
            .is_some_and(|markdown| markdown.contains("不包含任何私密内容")));
        assert!(!serde_json::to_string(&translated)
            .unwrap()
            .contains("deterministic English translation test"));
    }

    fn minutes_calls(fake: &crate::provider_generation::tests::FakeProvider) -> usize {
        fake.systems
            .lock()
            .unwrap()
            .iter()
            .filter(|system| system.starts_with(memex_analysis::AI_MINUTES_PROMPT))
            .count()
    }

    #[test]
    fn minutes_request_wire_errors() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let item_id = synthetic_stored_item(&root_path, "english-minutes-wire");
        let mut queue = Queue::open(&root_path.join("state/agent.db")).unwrap();
        let request = |item_id: &str| -> Command {
            serde_json::from_value(serde_json::json!({
                "command": "minutes_request",
                "item_id": item_id,
                "max_sensitivity": "restricted"
            }))
            .unwrap()
        };
        let reason = |error: anyhow::Error| Response::error(&error.to_string()).reason_code;
        // No saved summary route (and so no credential read).
        let error = dispatch(&mut queue, &root_path, request(&item_id)).unwrap_err();
        assert_eq!(reason(error).as_deref(), Some("no_minutes_provider"));
        let missing = format!("item_{}", "0".repeat(32));
        let error = dispatch(&mut queue, &root_path, request(&missing)).unwrap_err();
        assert_eq!(reason(error).as_deref(), Some("library_resource_not_found"));
    }

    #[test]
    fn scoped_minutes_status_reads_under_the_ceiling_and_never_authorizes() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let item_id = synthetic_stored_item_at_sensitivity(
            &root_path,
            "long-english-minutes-status",
            Sensitivity::Internal,
        );
        let mut queue = Queue::open(&root_path.join("state/agent.db")).unwrap();
        let status = |queue: &mut Queue, ceiling: &str| {
            let command: Command = serde_json::from_value(serde_json::json!({
                "command": "scoped_minutes_status",
                "item_id": item_id,
                "max_sensitivity": ceiling
            }))
            .unwrap();
            dispatch(queue, &root_path, command)
        };
        let error = status(&mut queue, "public").unwrap_err();
        assert_eq!(
            Response::error(&error.to_string()).reason_code.as_deref(),
            Some("library_resource_not_found")
        );
        let minutes = status(&mut queue, "internal").unwrap().ai_minutes.unwrap();
        assert_eq!(minutes.state, "not_requested");
        // Reading twice authorizes nothing: still not requested, no summary authorization.
        let current = memex_store::read_item(&root_path, &item_id).unwrap().1;
        assert_eq!(
            status(&mut queue, "internal").unwrap().ai_minutes.unwrap(),
            minutes
        );
        assert!(
            !memex_analysis::summary_authorized(&root_path, &item_id, &current.revision_id)
                .unwrap()
        );
        // After the explicit request, the read reports the same queued minutes.
        let requested = crate::translation_request::minutes_request(
            &root_path,
            &item_id,
            Sensitivity::Internal,
            true,
        )
        .unwrap();
        assert_eq!(requested.state, "queued");
        assert_eq!(
            status(&mut queue, "internal").unwrap().ai_minutes.unwrap(),
            requested
        );
    }

    #[test]
    fn chrome_no_generation_survives_commit_restart_and_reconciliation() {
        use memex_intake::url_jobs::UrlEnqueue;
        use crate::fetch::{Resolver, HttpTransport, RawResponse};
        use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
        struct Network(Arc<AtomicUsize>);
        impl Resolver for Network {
            fn resolve(&self, _: &str, _: Duration) -> Result<Vec<std::net::IpAddr>> {
                Ok(vec!["93.184.216.34".parse().unwrap()])
            }
        }
        impl HttpTransport for Network {
            fn get(&self, _: &url::Url, pinned: &[std::net::SocketAddr], _: Duration) -> Result<RawResponse> {
                assert!(!pinned.is_empty());
                assert_eq!(self.0.fetch_add(1, Ordering::SeqCst), 0, "recovery refetched the article");
                Ok(RawResponse { status: 200,
                    headers: vec![("content-type".into(), "text/html".into())],
                    body: b"<article><h1>Synthetic capture</h1><p>This synthetic public article contains enough deterministic prose for the Readability translator to extract a stable document without any network access or private data.</p></article>".to_vec() })
            }
        }
        struct ForbiddenQuality;
        impl memex_analysis::capture_quality::CaptureQualityModel for ForbiddenQuality {
            fn descriptor(&self) -> memex_analysis::capture_quality::CaptureQualityDescriptor {
                panic!("no-generation resolved a quality model")
            }
            fn check(&self, _: memex_analysis::capture_quality::CaptureQualityInput<'_>)
                -> std::result::Result<String, memex_analysis::capture_quality::CaptureQualityFailure> {
                panic!("no-generation invoked a quality model")
            }
        }
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let library = root.join("Library");
        let database = root.join("agent.db");
        let config = AgentConfig { library: library.clone(), database: database.clone(),
            spool: root.join("spool"), socket: root.join("agent.sock"),
            legacy_translation: None, intake: None, metadata_lookup_contact: None, chrome_executable: None };
        let calls = Arc::new(AtomicUsize::new(0));
        let request: UrlEnqueue = serde_json::from_value(serde_json::json!({
            "schema_version":1, "request_key":format!("chrome_capture_{}", "n".repeat(32)),
            "url":"https://example.invalid/no-generation", "include_comments":false,
            "quality_check":"off", "generation_policy":"none"
        })).unwrap();
        let owner = "a".repeat(64);
        let mut capture = Coordinator::new(config.clone(), Network(calls.clone()), Network(calls.clone()))
            .unwrap().with_quality_model(Arc::new(ForbiddenQuality));
        let job = capture.queue.chrome_enqueue(&owner, &request).unwrap();
        // Fail Store after acquisition; retry must reuse frozen bytes and the
        // persisted policy, not an in-memory enqueue option or callback.
        fs::remove_dir(&library).unwrap();
        fs::write(&library, b"synthetic Store failure").unwrap();
        assert!(capture.process_one().unwrap());
        assert_eq!(capture.queue.record(&job.job_id).unwrap().view.state, crate::queue::State::RetryWait);
        drop(capture);
        fs::remove_file(&library).unwrap();
        let mut capture = Coordinator::new(config, Network(calls.clone()), Network(calls.clone()))
            .unwrap().with_quality_model(Arc::new(ForbiddenQuality));
        capture.queue.retry(&job.job_id).unwrap();
        assert!(capture.process_one().unwrap());
        let record = capture.queue.record(&job.job_id).unwrap();
        assert_eq!(record.view.state, crate::queue::State::Succeeded);
        let item = record.view.item_id.unwrap();
        let revision = record.result_revision_id.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(memex_store::list_items(&library).unwrap().len(), 1);
        drop(capture);
        let mut queue = Queue::open(&database).unwrap();
        assert!(queue.record(&job.job_id).unwrap().translation_hook_completed);
        assert!(!queue.translation_intent_pending(&item, &revision).unwrap());
        let fake = crate::provider_generation::tests::FakeProvider::default();
        for _ in 0..3 {
            drop(queue);
            queue = Queue::open(&database).unwrap();
            assert_eq!(queue.chrome_enqueue(&owner, &request).unwrap().job_id, job.job_id);
            assert!(queue.pending_translation_intent().unwrap().is_none());
            let mut provider = ProviderWorker::new(Box::new(fake.clone()));
            let mut reconciler = AnalysisReconciler::new(Duration::from_secs(3600));
            run_worker_until_idle(&library, &database, &mut provider, &mut reconciler);
            assert!(fake.calls.lock().unwrap().is_empty());
        }
        // Positive control: the same worker does call the provider for an ordinary
        // capture. A disabled spy cannot make the no-generation assertion pass.
        let control = synthetic_stored_item(&library, "english-generation-control");
        let control_revision = memex_store::read_item(&library, &control).unwrap().1.revision_id;
        queue.enqueue("remote_iphone", "normal-control", "https://example.invalid/control").unwrap();
        let leased = queue.lease(60).unwrap().unwrap();
        queue.finish_stored(&leased, &control, &control_revision).unwrap();
        let mut provider = ProviderWorker::new(Box::new(fake.clone()));
        let mut reconciler = AnalysisReconciler::new(Duration::from_secs(3600));
        run_worker_until_idle(&library, &database, &mut provider, &mut reconciler);
        assert!(!fake.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn captures_never_trigger_minutes_and_old_automatic_authorizations_are_dropped_once() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let item_id = synthetic_stored_item(&root_path, "english-capture-minutes");
        let (_, revision) = memex_store::read_item(&root_path, &item_id).unwrap();
        let database = root_path.join("state/agent.db");
        let mut queue = Queue::open(&database).unwrap();
        queue
            .enqueue(
                "remote_iphone",
                "minutes-capture-fixture",
                "https://example.invalid/minutes-capture",
            )
            .unwrap();
        let leased = queue.lease(60).unwrap().unwrap();
        queue
            .finish_stored(&leased, &item_id, &revision.revision_id)
            .unwrap();
        let fake = crate::provider_generation::tests::FakeProvider::default();
        let mut provider = ProviderWorker::new(Box::new(fake.clone()));
        let mut reconciler = AnalysisReconciler::new(Duration::from_secs(3600));
        run_worker_until_idle(&root_path, &database, &mut provider, &mut reconciler);
        // The capture was translated (and its metadata enriched) but got no minutes call.
        assert!(!fake.calls.lock().unwrap().is_empty());
        assert_eq!(minutes_calls(&fake), 0);
        let (_, current) = memex_store::read_item(&root_path, &item_id).unwrap();
        let read = library_item(&mut queue, &root_path, &item_id);
        assert_eq!(read.ai_minutes.unwrap().state, "not_requested");

        // A minutes job authorized automatically before this change loses that authorization
        // at the first start, once; a later explicit request is never touched again.
        memex_analysis::enqueue_item_summary(&root_path, &item_id, &current.revision_id).unwrap();
        let mut summaries = SummaryQueue::open(&root_path).unwrap();
        summaries
            .request_minutes(&item_id, &current.revision_id)
            .unwrap();
        assert_eq!(summaries.deauthorize_unstarted_minutes_once().unwrap(), 1);
        assert!(
            !memex_analysis::summary_authorized(&root_path, &item_id, &current.revision_id)
                .unwrap()
        );
        summaries
            .request_minutes(&item_id, &current.revision_id)
            .unwrap();
        assert_eq!(summaries.deauthorize_unstarted_minutes_once().unwrap(), 0);
        drop(summaries);
        run_worker_until_idle(&root_path, &database, &mut provider, &mut reconciler);
        assert_eq!(minutes_calls(&fake), 1);
    }

    #[test]
    fn hook_worker_failure_is_isolated_and_retry_enqueues_the_exact_revision() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let item_id = synthetic_stored_item(&root_path, "translated-library");
        let (_, revision) = memex_store::read_item(&root_path, &item_id).unwrap();
        let database = root_path.join("state/agent.db");
        let mut queue = Queue::open(&database).unwrap();
        let job = queue
            .enqueue(
                "remote_iphone",
                "translation-worker-fixture",
                "https://example.invalid/worker-fixture",
            )
            .unwrap();
        let leased = queue.lease(60).unwrap().unwrap();
        queue
            .finish_stored(&leased, &item_id, &revision.revision_id)
            .unwrap();
        let intent = queue.pending_translation_intent().unwrap().unwrap();
        assert_eq!(intent.job_id, job.job_id);
        assert_eq!(intent.revision_id, revision.revision_id);
        drop(queue);

        let analysis = root_path.join("analysis");
        fs::write(&analysis, b"synthetic Analysis obstruction").unwrap();
        assert!(translation_worker_step(
            &root_path,
            &database,
            None,
            &mut AnalysisReconciler::new(Duration::ZERO),
        )
        .is_err());
        let queue = Queue::open(&database).unwrap();
        assert!(queue.pending_translation_intent().unwrap().is_none());
        assert!(
            !queue
                .record(&job.job_id)
                .unwrap()
                .translation_hook_completed
        );
        drop(queue);

        fs::remove_file(analysis).unwrap();
        std::thread::sleep(Duration::from_millis(2_100));
        assert!(translation_worker_step(
            &root_path,
            &database,
            None,
            &mut AnalysisReconciler::new(Duration::ZERO),
        )
        .unwrap());
        let queue = Queue::open(&database).unwrap();
        assert!(queue.pending_translation_intent().unwrap().is_none());
        let status =
            memex_analysis::translation_status(&root_path, &item_id, &revision.revision_id)
                .unwrap()
                .unwrap();
        assert_eq!(status.state, TranslationJobState::Queued);
        assert_eq!(
            memex_analysis::summary_status(&root_path, &item_id, &revision.revision_id)
                .unwrap()
                .unwrap()
                .state,
            SummaryJobState::Queued
        );
        // Capture authorizes only the title. Full text and minutes still require
        // explicit requests even when the durable capture hook is replayed.
        assert!(!memex_analysis::translation_authorized(
            &root_path,
            &item_id,
            &revision.revision_id
        )
        .unwrap());
        assert!(memex_analysis::TitleTranslationQueue::open(&root_path)
            .unwrap()
            .has_work()
            .unwrap());
        assert!(
            !memex_analysis::summary_authorized(&root_path, &item_id, &revision.revision_id)
                .unwrap()
        );
    }

    #[test]
    fn failing_oldest_hook_is_deferred_without_blocking_later_hook_or_analysis_work() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let database = root_path.join("state/agent.db");
        let mut queue = Queue::open(&database).unwrap();
        let failing = queue
            .enqueue(
                "remote_iphone",
                "failing-oldest-translation-hook",
                "https://example.invalid/failing-hook",
            )
            .unwrap();
        let leased = queue.lease(60).unwrap().unwrap();
        queue
            .finish_stored(
                &leased,
                "item_missing_translation_hook",
                "revision_missing_translation_hook",
            )
            .unwrap();
        std::thread::sleep(Duration::from_millis(2));
        let item_id = synthetic_stored_item(&root_path, "translated-library");
        let (_, revision) = memex_store::read_item(&root_path, &item_id).unwrap();
        let later = queue
            .enqueue(
                "remote_iphone",
                "later-valid-translation-hook",
                "https://example.invalid/later-hook",
            )
            .unwrap();
        let leased = queue.lease(60).unwrap().unwrap();
        queue
            .finish_stored(&leased, &item_id, &revision.revision_id)
            .unwrap();
        drop(queue);

        assert!(translation_worker_step(
            &root_path,
            &database,
            None,
            &mut AnalysisReconciler::new(Duration::ZERO),
        )
        .unwrap());
        let queue = Queue::open(&database).unwrap();
        let next = queue.pending_translation_intent().unwrap().unwrap();
        assert_eq!(next.job_id, later.job_id);
        assert_ne!(next.job_id, failing.job_id);
        drop(queue);
        assert!(translation_worker_step(
            &root_path,
            &database,
            None,
            &mut AnalysisReconciler::new(Duration::ZERO),
        )
        .unwrap());
        let queued =
            memex_analysis::translation_status(&root_path, &item_id, &revision.revision_id)
                .unwrap()
                .unwrap();
        assert_eq!(queued.state, TranslationJobState::Queued);
        assert_eq!(
            memex_analysis::summary_status(&root_path, &item_id, &revision.revision_id)
                .unwrap()
                .unwrap()
                .state,
            SummaryJobState::Queued
        );

        let mut model = SyntheticTranslationModel;
        assert!(process_translation_queue_step(&root_path, &mut model, Duration::ZERO).unwrap());
        assert_eq!(
            memex_analysis::translation_status(&root_path, &item_id, &revision.revision_id)
                .unwrap()
                .unwrap()
                .state,
            TranslationJobState::Succeeded
        );
        let queue = Queue::open(&database).unwrap();
        if let Some(pending) = queue.pending_translation_intent().unwrap() {
            assert_eq!(pending.job_id, failing.job_id);
        }
    }

    #[test]
    fn transient_model_failure_is_automatically_retried_to_validated_success() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let item_id = synthetic_stored_item(&root_path, "translated-library");
        let (_, revision) = memex_store::read_item(&root_path, &item_id).unwrap();
        memex_analysis::enqueue_item_translation(&root_path, &item_id, &revision.revision_id)
            .unwrap();
        let mut model = FlakyTranslationModel { calls: 0 };

        assert!(process_translation_queue_step(&root_path, &mut model, Duration::ZERO).unwrap());
        let failed =
            memex_analysis::translation_status(&root_path, &item_id, &revision.revision_id)
                .unwrap()
                .unwrap();
        assert_eq!(failed.state, TranslationJobState::Failed);
        assert!(failed.retryable);
        assert!(process_translation_queue_step(&root_path, &mut model, Duration::ZERO).unwrap());
        assert!(process_translation_queue_step(&root_path, &mut model, Duration::ZERO).unwrap());

        let succeeded =
            memex_analysis::translation_status(&root_path, &item_id, &revision.revision_id)
                .unwrap()
                .unwrap();
        assert_eq!(succeeded.state, TranslationJobState::Succeeded);
        assert_eq!(succeeded.attempt_count, 2);
        assert_eq!(model.calls, 3);
        assert!(memex_analysis::validated_translation_pair(
            &root_path,
            &item_id,
            &revision.revision_id,
        )
        .unwrap()
        .is_some());
    }

    #[test]
    fn slow_translation_model_step_does_not_own_the_capture_queue_database() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let database = root_path.join("state/agent.db");
        Queue::open(&database).unwrap();
        let item_id = synthetic_stored_item(&root_path, "translated-library");
        let (_, revision) = memex_store::read_item(&root_path, &item_id).unwrap();
        memex_analysis::enqueue_item_translation(&root_path, &item_id, &revision.revision_id)
            .unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let translation_root = root_path.clone();
        let model = std::thread::spawn(move || {
            process_translation_queue_step(
                &translation_root,
                &mut BlockingTranslationModel {
                    entered: entered_tx,
                    release: release_rx,
                    blocked: false,
                },
                Duration::ZERO,
            )
            .unwrap()
        });
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let (capture_tx, capture_rx) = std::sync::mpsc::channel();
        let capture = std::thread::spawn(move || {
            let mut capture_queue = Queue::open(&database).unwrap();
            let job = capture_queue
                .enqueue(
                    "remote_iphone",
                    "capture-during-model-call",
                    "https://example.invalid/not-blocked",
                )
                .unwrap();
            let state = capture_queue.get(&job.job_id).unwrap().state;
            capture_tx.send(state).unwrap();
        });
        assert_eq!(
            capture_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            crate::queue::State::Queued
        );
        release_tx.send(()).unwrap();
        capture.join().unwrap();
        assert!(model.join().unwrap());
        assert_eq!(
            memex_analysis::translation_status(&root_path, &item_id, &revision.revision_id)
                .unwrap()
                .unwrap()
                .state,
            TranslationJobState::Succeeded
        );
    }

    #[test]
    fn exact_remote_job_item_dispatch_reaches_store_after_authorization() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let item_id = synthetic_stored_item(&root_path, "exact");
        let other_item_id = synthetic_stored_item(&root_path, "other");
        let mut queue = Queue::open(&root_path.join("state/agent.db")).unwrap();
        let exact = queue
            .enqueue(
                "remote_iphone",
                "remote_exact",
                "https://example.invalid/exact",
            )
            .unwrap();
        let leased = queue.lease(60).unwrap().unwrap();
        assert_eq!(leased.view.job_id, exact.job_id);
        let (_, revision) = memex_store::read_item(&root_path, &item_id).unwrap();
        queue
            .finish_stored(&leased, &item_id, &revision.revision_id)
            .unwrap();
        let other = queue
            .enqueue(
                "remote_iphone",
                "remote_other",
                "https://example.invalid/other",
            )
            .unwrap();
        let leased = queue.lease(60).unwrap().unwrap();
        assert_eq!(leased.view.job_id, other.job_id);
        let (_, revision) = memex_store::read_item(&root_path, &other_item_id).unwrap();
        queue
            .finish_stored(&leased, &other_item_id, &revision.revision_id)
            .unwrap();

        let mismatch = dispatch(
            &mut queue,
            &root_path,
            Command::RemoteItemRead {
                job_id: exact.job_id.clone(),
                item_id: other_item_id,
            },
        )
        .unwrap_err();
        assert_eq!(mismatch.to_string(), "remote_resource_not_found");

        let response = dispatch(
            &mut queue,
            &root_path,
            Command::RemoteItemRead {
                job_id: exact.job_id,
                item_id: item_id.clone(),
            },
        )
        .unwrap();
        let item = response.item.unwrap();
        assert_eq!(item.item_id, item_id);
        assert_eq!(item.title, "Synthetic remote result exact");
        assert_eq!(item.markdown, "# Synthetic remote result exact");
    }

    fn crossref_work(doi: &str) -> serde_json::Value {
        serde_json::json!({
            "status": "ok", "message-type": "work",
            "message": {
                "DOI": doi,
                "title": ["Synthetic IPC Work"],
                "container-title": ["Journal of Synthetic IPC"],
                "issued": {"date-parts": [[2024]]},
                "published-online": {"date-parts": [[2023, 12, 31]]},
                "link": [{"URL": "https://fulltext.example.invalid/paper.pdf"}]
            }
        })
    }

    fn v2_edit(
        item_id: &str,
        revision_id: &str,
        request_id: &str,
        set: serde_json::Value,
    ) -> memex_store::ItemMetadataEditV2 {
        serde_json::from_value(serde_json::json!({
            "metadata_schema_version": 2, "item_id": item_id, "expected_revision_id": revision_id,
            "request_id": request_id, "set": set, "clear": [], "preview_id": null,
            "selected_candidate_ids": []
        }))
        .unwrap()
    }

    fn edit_receipt(
        q: &mut Queue,
        root: &Path,
        edit: memex_store::ItemMetadataEditV2,
    ) -> Result<memex_store::ItemMetadataUpdateReceipt> {
        Ok(dispatch(
            q,
            root,
            Command::BibliographicMetadataEdit {
                max_sensitivity: Sensitivity::Restricted,
                edit,
            },
        )?
        .item_update
        .unwrap())
    }

    fn lookup_command(item_id: &str, revision_id: &str, kind: &str, value: &str) -> Command {
        serde_json::from_value(serde_json::json!({
            "command": "bibliographic_lookup_preview", "item_id": item_id,
            "expected_revision_id": revision_id,
            "identifier": {"kind": kind, "value": value}, "max_sensitivity": "restricted"
        }))
        .unwrap()
    }

    #[test]
    fn idle_workers_block_until_notified_and_reconcile_passes_skip_seen_revisions() {
        // An idle wait sleeps for its timeout instead of spinning...
        let mut seen = {
            let (count, _) = &WORK_SIGNAL;
            *count.lock().unwrap()
        };
        let started = std::time::Instant::now();
        wait_for_work(&mut seen, Duration::from_millis(200));
        assert!(started.elapsed() >= Duration::from_millis(150));
        // ...and an enqueue in this process wakes it at once.
        let waiter = std::thread::spawn(move || {
            let started = std::time::Instant::now();
            wait_for_work(&mut seen, Duration::from_secs(30));
            started.elapsed()
        });
        std::thread::sleep(Duration::from_millis(50));
        notify_workers();
        assert!(waiter.join().unwrap() < Duration::from_secs(5));

        // A periodic pass re-enqueues nothing for revisions it already reconciled.
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        synthetic_stored_item(&root_path, "english-idle");
        let mut reconciler = AnalysisReconciler::new(Duration::ZERO);
        assert!(reconciler.process_next_page(&root_path).unwrap());
        assert!(!reconciler.process_next_page(&root_path).unwrap());
        assert!(!reconciler.process_next_page(&root_path).unwrap());
    }

    fn run_worker_until_idle(
        root: &Path,
        database: &Path,
        provider: &mut ProviderWorker,
        reconciler: &mut AnalysisReconciler,
    ) {
        for _ in 0..64 {
            if !translation_worker_step(root, database, Some(provider), reconciler).unwrap() {
                return;
            }
        }
        panic!("worker did not become idle");
    }

    fn library_item(queue: &mut Queue, root: &Path, item_id: &str) -> LibraryItemResult {
        dispatch(
            queue,
            root,
            Command::LibraryItemRead {
                item_id: item_id.into(),
            },
        )
        .unwrap()
        .library_item
        .unwrap()
    }

    #[test]
    fn provider_worker_makes_no_model_calls_for_preexisting_items_without_a_request() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let items: Vec<String> = (0..6)
            .map(|index| synthetic_stored_item(&root_path, &format!("english-{index}")))
            .collect();
        let database = root_path.join("state/agent.db");
        let mut queue = Queue::open(&database).unwrap();
        let fake = crate::provider_generation::tests::FakeProvider::default();
        let mut provider = ProviderWorker::new(Box::new(fake.clone()));
        let mut reconciler = AnalysisReconciler::new(Duration::from_secs(3600));
        run_worker_until_idle(&root_path, &database, &mut provider, &mut reconciler);
        // Repeated full reconciliation passes (as the 30 s daemon timer would do).
        for _ in 0..5 {
            translation_worker_step(
                &root_path,
                &database,
                Some(&mut provider),
                &mut AnalysisReconciler::new(Duration::ZERO),
            )
            .unwrap();
        }
        // The reconciler did enqueue every eligible item, yet nothing reached a provider:
        // no route was even resolved (no settings/Keychain read) and no request was made.
        for item in &items {
            let (_, revision) = memex_store::read_item(&root_path, item).unwrap();
            let status =
                memex_analysis::translation_status(&root_path, item, &revision.revision_id)
                    .unwrap()
                    .unwrap();
            assert_eq!(status.state, TranslationJobState::Queued);
            let read = library_item(&mut queue, &root_path, item);
            assert!(matches!(
                read.translation_state,
                LibraryTranslationState::NotRequested
            ));
            assert!(matches!(
                read.summary_state,
                LibrarySummaryState::NotRequested
            ));
        }
        assert_eq!(*fake.routes.lock().unwrap(), 0);
        assert!(fake.calls.lock().unwrap().is_empty());

        // One explicit request translates exactly that item; it never authorizes minutes.
        let requested = crate::translation_request::request(
            &root_path,
            &items[2],
            Sensitivity::Restricted,
            crate::translation_request::RequestRoutes {
                translation: true,
                minutes: true,
                metadata: false,
            },
        )
        .unwrap()
        .0;
        assert_eq!(requested.state, "queued");
        run_worker_until_idle(&root_path, &database, &mut provider, &mut reconciler);
        assert_eq!(
            fake.calls.lock().unwrap().len(),
            2,
            "title + one document batch, no minutes call"
        );
        let read = library_item(&mut queue, &root_path, &items[2]);
        assert_eq!(read.ai_minutes.as_ref().unwrap().state, "not_requested");
        // `minutes_request` is the only authorization: exactly one minutes call.
        let minutes = crate::translation_request::minutes_request(
            &root_path,
            &items[2],
            Sensitivity::Restricted,
            true,
        )
        .unwrap();
        assert_eq!(minutes.state, "queued");
        run_worker_until_idle(&root_path, &database, &mut provider, &mut reconciler);
        let calls = fake.calls.lock().unwrap().len();
        assert_eq!(calls, 3, "title + one document batch + minutes");
        let read = library_item(&mut queue, &root_path, &items[2]);
        assert!(matches!(
            read.translation_state,
            LibraryTranslationState::Succeeded
        ));
        assert!(matches!(read.summary_state, LibrarySummaryState::Succeeded));
        assert_eq!(read.title.as_deref(), Some("合成文章的中文标题"));
        let markdown = read.markdown.unwrap();
        assert_eq!(
            crate::markdown_blocks::split_blocks(&markdown).len(),
            crate::markdown_blocks::split_blocks(
                &memex_store::read_item(&root_path, &items[2])
                    .unwrap()
                    .1
                    .content_markdown
            )
            .len()
        );
        let scoped = dispatch(
            &mut queue,
            &root_path,
            Command::ScopedTranslationRead {
                item_id: items[2].clone(),
                expected_revision_id: read.revision_id.clone(),
                start_byte: 0,
                max_sensitivity: Sensitivity::Restricted,
            },
        )
        .unwrap()
        .translation
        .unwrap();
        assert_eq!(scoped.state, "succeeded");
        for (index, item) in items.iter().enumerate().filter(|(index, _)| *index != 2) {
            let read = library_item(&mut queue, &root_path, item);
            assert!(
                matches!(
                    read.translation_state,
                    LibraryTranslationState::NotRequested
                ),
                "item {index} must stay untouched"
            );
        }
        // Succeeded is returned as-is without another request.
        let again = crate::translation_request::request(
            &root_path,
            &items[2],
            Sensitivity::Restricted,
            crate::translation_request::RequestRoutes {
                translation: true,
                minutes: true,
                metadata: false,
            },
        )
        .unwrap()
        .0;
        assert_eq!(again.state, "succeeded");
        let minutes = crate::translation_request::minutes_request(
            &root_path,
            &items[2],
            Sensitivity::Restricted,
            true,
        )
        .unwrap();
        assert_eq!(minutes.state, "succeeded");
        assert!(minutes.text.is_some());
        run_worker_until_idle(&root_path, &database, &mut provider, &mut reconciler);
        assert_eq!(fake.calls.lock().unwrap().len(), calls);
    }

    #[test]
    fn unknown_outcome_is_retried_with_a_bound_and_an_explicit_request_requeues_it() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let item = synthetic_stored_item(&root_path, "english-unknown");
        let database = root_path.join("state/agent.db");
        Queue::open(&database).unwrap();
        let fake = crate::provider_generation::tests::FakeProvider::default();
        *fake.fail.lock().unwrap() =
            Some(memex_provider::generation::GenerationError::OutcomeUnknown);
        let mut provider = ProviderWorker::new(Box::new(fake.clone()));
        let mut reconciler = AnalysisReconciler::new(Duration::from_secs(3600));
        crate::translation_request::request(
            &root_path,
            &item,
            Sensitivity::Restricted,
            crate::translation_request::RequestRoutes {
                translation: true,
                minutes: false,
                metadata: false,
            },
        )
        .unwrap()
        .0;
        // Subscription route: an unknown outcome is retried automatically, with backoff,
        // and never more than the attempt bound (3 dispatches in total).
        run_worker_until_idle(&root_path, &database, &mut provider, &mut reconciler);
        assert_eq!(fake.calls.lock().unwrap().len(), 1, "backoff, not an immediate loop");
        for _ in 0..8 {
            std::thread::sleep(Duration::from_millis(1_100));
            run_worker_until_idle(&root_path, &database, &mut provider, &mut reconciler);
        }
        assert_eq!(
            fake.calls.lock().unwrap().len(),
            3,
            "automatic retries stop at the attempt bound"
        );
        let (_, revision) = memex_store::read_item(&root_path, &item).unwrap();
        let status = memex_analysis::translation_status(&root_path, &item, &revision.revision_id)
            .unwrap()
            .unwrap();
        assert_eq!(status.state, TranslationJobState::Failed);
        assert_eq!(
            status.reason_code.as_deref(),
            Some("retry_exhausted")
        );
        assert!(!status.retryable);

        *fake.fail.lock().unwrap() = None;
        let requeued = crate::translation_request::request(
            &root_path,
            &item,
            Sensitivity::Restricted,
            crate::translation_request::RequestRoutes {
                translation: true,
                minutes: false,
                metadata: false,
            },
        )
        .unwrap()
        .0;
        assert_eq!(requeued.state, "queued");
        run_worker_until_idle(&root_path, &database, &mut provider, &mut reconciler);
        assert_eq!(
            memex_analysis::translation_status(&root_path, &item, &revision.revision_id)
                .unwrap()
                .unwrap()
                .state,
            TranslationJobState::Succeeded
        );
    }

    #[test]
    fn translation_request_gates_chinese_sources_and_missing_providers_without_enqueue() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let chinese = synthetic_stored_item(&root_path, "chinese-source");
        let english = synthetic_stored_item(&root_path, "english-noprovider");
        let mut queue = Queue::open(&root_path.join("state/agent.db")).unwrap();
        let request = |item: &str| {
            serde_json::from_value::<Command>(serde_json::json!({
                "command": "translation_request", "item_id": item, "max_sensitivity": "restricted"
            }))
            .unwrap()
        };
        // No model settings: no route, and the credential store is never consulted.
        let response = dispatch(&mut queue, &root_path, request(&chinese)).unwrap();
        let value = serde_json::to_value(&response).unwrap();
        assert_eq!(value["translation"]["state"], "not_requested");
        assert_eq!(value["translation"]["reason"], "source_is_chinese");
        let response = dispatch(&mut queue, &root_path, request(&english)).unwrap();
        let value = serde_json::to_value(&response).unwrap();
        assert_eq!(value["translation"]["state"], "unavailable");
        assert_eq!(value["translation"]["reason"], "no_translation_provider");
        for item in [&chinese, &english] {
            let (_, revision) = memex_store::read_item(&root_path, item).unwrap();
            assert!(
                memex_analysis::translation_status(&root_path, item, &revision.revision_id)
                    .unwrap()
                    .is_none()
            );
        }
        let missing = dispatch(&mut queue, &root_path, request("item_missing")).unwrap_err();
        assert_eq!(missing.to_string(), "library_resource_not_found");
    }

    #[test]
    fn group_edit_writes_through_store_with_replay_and_stale_semantics() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let item = synthetic_stored_item(&root_path, "group-member");
        let mut queue = Queue::open(&root_path.join("state/agent.db")).unwrap();
        let mut edit = |request_id: &str, expected: Option<&str>, ops: serde_json::Value| {
            let command: Command = serde_json::from_value(serde_json::json!({
                "command": "group_edit", "max_sensitivity": "restricted",
                "request_id": request_id, "expected_version": expected, "ops": ops
            }))
            .unwrap();
            dispatch(&mut queue, &root_path, command)
                .map(|response| serde_json::to_value(&response).unwrap())
        };
        let created = edit(
            "r1",
            None,
            serde_json::json!([{"op": "create_root", "name": "Reading"}]),
        )
        .unwrap();
        let root_id = created["group_edit"]["group_id"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(created["group_tree"]["total"], 1);
        assert_eq!(created["group_tree"]["nodes"][0]["sensitivity"], "private");
        let replay = edit(
            "r1",
            None,
            serde_json::json!([{"op": "create_root", "name": "Reading"}]),
        )
        .unwrap();
        assert_eq!(replay["group_edit"], created["group_edit"]);
        assert_eq!(replay["group_tree"]["total"], 1);
        assert_eq!(
            edit(
                "r1",
                None,
                serde_json::json!([{"op": "create_root", "name": "Other"}])
            )
            .unwrap_err()
            .to_string(),
            "request_id_conflict"
        );
        let folder = edit(
            "r2",
            Some("v1"),
            serde_json::json!([{"op": "create_folder", "parent_id": root_id, "name": "Papers"}]),
        )
        .unwrap();
        let folder_id = folder["group_edit"]["group_id"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(folder["group_tree"]["total"], 2);
        assert_eq!(
            edit(
                "r3",
                Some("v1"),
                serde_json::json!([{"op": "rename", "id": root_id, "name": "Stale"}])
            )
            .unwrap_err()
            .to_string(),
            "stale_group_version"
        );
        let added = edit(
            "r4",
            Some("v1"),
            serde_json::json!([{"op": "add_members", "group_id": folder_id, "item_ids": [item]}]),
        )
        .unwrap();
        let papers = added["group_tree"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["id"] == folder_id.as_str())
            .unwrap()
            .clone();
        assert_eq!(papers["item_count"], 1);
        assert_eq!(added["group_tree"]["nodes"][0]["subtree_item_count"], 1);
        edit(
            "r5",
            Some("v2"),
            serde_json::json!([{"op": "rename", "id": root_id, "name": "Library"}]),
        )
        .unwrap();
        let removed = edit(
            "r6",
            Some("v2"),
            serde_json::json!([{"op": "remove_members", "group_id": folder_id, "item_ids": [item]}]),
        )
        .unwrap();
        assert_eq!(removed["group_tree"]["nodes"][0]["name"], "Library");
        assert_eq!(removed["group_tree"]["nodes"][0]["subtree_item_count"], 0);
        assert_eq!(
            edit("r7", None, serde_json::json!([]))
                .unwrap_err()
                .to_string(),
            "invalid_group_edit"
        );
        assert_eq!(
            edit(
                "r8",
                None,
                serde_json::json!([{"op": "rename", "id": root_id, "name": "X"}])
            )
            .unwrap_err()
            .to_string(),
            "invalid_group_edit"
        );
        assert!(serde_json::from_value::<Command>(serde_json::json!({
            "command": "group_edit", "max_sensitivity": "restricted", "request_id": "r9",
            "ops": [{"op": "move", "id": root_id, "parent_id": folder_id}]
        }))
        .is_err());
    }

    #[test]
    fn agent_version_reports_the_package_version_and_a_short_binary_digest_only() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let mut queue = Queue::open(&root_path.join("state/agent.db")).unwrap();
        let command: Command =
            serde_json::from_value(serde_json::json!({"command": "agent_version"})).unwrap();
        let response = dispatch(&mut queue, &root_path, command).unwrap();
        let value = serde_json::to_value(&response).unwrap();
        let version = &value["agent_version"];
        assert_eq!(version["version"], env!("CARGO_PKG_VERSION"));
        if !version["binary_sha256"].is_null() {
            let digest = version["binary_sha256"].as_str().unwrap();
            assert!(digest.len() == 12 && digest.bytes().all(|b| b.is_ascii_hexdigit()));
        }
        assert_eq!(version["query_origin"], true);
        assert_eq!(version["group_reorder"], true);
        assert_eq!(version["group_trash"], true);
        assert_eq!(version.as_object().unwrap().len(), 5, "no paths or other fields");
    }

    #[test]
    fn capture_jobs_recent_is_gated_bounded_and_hides_unreadable_results() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let public = synthetic_stored_item(&root_path, "recent_public");
        let private =
            synthetic_stored_item_at_sensitivity(&root_path, "recent_private", Sensitivity::Private);
        let database = root_path.join("state/agent.db");
        let mut queue = Queue::open(&database).unwrap();
        let a = queue.enqueue("local_surface", "recent_a", "https://synthetic.test/a").unwrap();
        let b = queue.enqueue("local_surface", "recent_b", "https://synthetic.test/b").unwrap();
        queue.enqueue("local_surface", "recent_c", "https://synthetic.test/c").unwrap();
        let conn = rusqlite::Connection::open(&database).unwrap();
        for (job, item) in [(&a.job_id, &public), (&b.job_id, &private)] {
            conn.execute(
                "UPDATE jobs SET state='succeeded',stage='commit',result_item_id=?2 WHERE job_id=?1",
                rusqlite::params![job, item],
            )
            .unwrap();
        }
        let command = |sensitivity: &str, limit: u32| -> Command {
            serde_json::from_value(serde_json::json!({
                "command": "capture_jobs_recent", "max_sensitivity": sensitivity, "limit": limit
            }))
            .unwrap()
        };
        let denied = dispatch(&mut queue, &root_path, command("public", 10)).unwrap_err();
        assert_eq!(denied.to_string(), "permission_denied");

        let response = dispatch(&mut queue, &root_path, command("internal", 10)).unwrap();
        let value = serde_json::to_value(&response).unwrap();
        let recent = &value["capture_jobs"];
        assert_eq!(recent["schema_version"], 1);
        assert_eq!(recent["counts"]["queued"], 1);
        assert_eq!(recent["counts"]["succeeded"], 2);
        let jobs = recent["jobs"].as_array().unwrap();
        assert_eq!(jobs.len(), 3);
        assert_eq!(jobs[0]["state"], "queued");
        assert_eq!(jobs[0]["producer"], "other");
        let find = |id: &str| jobs.iter().find(|job| job["job_id"] == id).unwrap();
        assert_eq!(find(&a.job_id)["result_item_id"], public.as_str());
        assert_eq!(find(&a.job_id)["url"], "https://synthetic.test/a");
        assert!(find(&b.job_id)["result_item_id"].is_null(), "above the ceiling");
        assert!(find(&b.job_id)["url"].is_null());
        assert!(value["jobs"].is_null() && value["job"].is_null());

        let restricted = dispatch(&mut queue, &root_path, command("restricted", 1)).unwrap();
        let recent = restricted.capture_jobs.unwrap();
        assert_eq!(recent.jobs.len(), 1);
        let response = dispatch(&mut queue, &root_path, command("restricted", 10)).unwrap();
        let shown = response.capture_jobs.unwrap();
        let b_view = shown.jobs.iter().find(|job| job.job_id == b.job_id).unwrap();
        assert_eq!(b_view.result_item_id.as_deref(), Some(private.as_str()));
        assert!(serde_json::from_value::<Command>(serde_json::json!({
            "command": "capture_jobs_recent", "max_sensitivity": "internal", "limit": 1, "producer": "x"
        }))
        .is_err());
    }

    #[test]
    fn capabilities_advertise_bibliographic_v2_and_old_responses_keep_their_shape() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let item_id = synthetic_stored_item(&root_path, "capability");
        let mut queue = Queue::open(&root_path.join("state/agent.db")).unwrap();
        let command: Command =
            serde_json::from_value(serde_json::json!({"command": "capabilities"})).unwrap();
        let response = dispatch(&mut queue, &root_path, command).unwrap();
        let value = serde_json::to_value(&response).unwrap();
        assert_eq!(
            value["capabilities"]["features"],
            serde_json::json!([
                "bibliographic_metadata_v2",
                "groups_v1",
                "translation_provider_v1",
                "item_chat_v1",
                "ai_insights_v1",
                "dedup_trash_v1",
                "minutes_request_v1",
                "markdown_edit_v1",
                "reading_annotations_v1",
                "item_chat_history_v1",
                "reader_v1",
                "reader_annotations_v1",
                "reader_paragraph_alignment_v1",
                "reader_chapter_repair_v1",
                "reader_chapter_edit_v1",
                "reader_chapter_translation_v1",
                "reader_chapter_translation_metadata_v1",
                "reader_chapter_translation_alignment_v1",
                "reader_revision_text_v1",
                "reader_content_format_v1",
                "reader_content_format_batch_v1",
                "reader_passes_v1",
                "reader_annotations_ai_translation_v1",
                "item_cover_v1",
                "ui_active_group_v1",
                "ai_minutes_suggested_title_v1",
                "item_title_apply_v1",
                "one_step_model_jobs_v1",
                "capture_jobs_recent_v1"
            ])
        );
        assert_eq!(value["capabilities"]["groups_v1"]["writes"], true);
        assert_eq!(value["capabilities"]["groups_v1"]["query_unfiled"], true);
        assert_eq!(
            value["capabilities"]["groups_v1"]["write_ops"],
            serde_json::json!([
                "create_root",
                "create_folder",
                "rename",
                "add_members",
                "remove_members",
                "reorder_root",
                "transfer_folders"
            ])
        );
        // No model settings: no route, no credential lookup.
        assert_eq!(
            value["capabilities"]["translation_provider_v1"],
            serde_json::json!({
                "available": false,
                "purpose_routes": {"translation": null, "summary": null, "classification": null, "chat": null},
                "minutes": false,
                "metadata": false
            })
        );
        assert_eq!(
            value["capabilities"]["ai_insights_v1"],
            serde_json::json!({
                "highlights": false, "quotes": false, "fact_check": false,
                "fact_check_basis": "model_knowledge"
            })
        );
        assert_eq!(value["capabilities"]["groups_v1"]["moves"], false);
        let v2 = &value["capabilities"]["bibliographic_metadata_v2"];
        assert_eq!(v2["metadata_schema_version"], 2);
        assert_eq!(v2["lookup_batch"], false);
        assert_eq!(
            v2["lookup_sources"],
            serde_json::json!([
                {"provider": "crossref", "identifier_kind": "doi", "route": "https://api.crossref.org/works/{doi}"},
                {"provider": "open_library", "identifier_kind": "isbn", "route": "https://openlibrary.org/isbn/{isbn}.json"}
            ])
        );
        let old = dispatch(&mut queue, &root_path, Command::ItemRead { item_id }).unwrap();
        let old = serde_json::to_value(&old).unwrap();
        for key in [
            "capabilities",
            "bibliographic_metadata",
            "identifier_duplicates",
            "lookup_preview",
            "lookup_value",
            "group_tree",
        ] {
            assert!(old.get(key).is_none(), "{key}");
        }
    }

    #[test]
    fn bibliographic_read_edit_duplicates_and_lookup_apply_round_trip() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let item_id = synthetic_stored_item(&root_path, "biblio-ipc");
        let other_id = synthetic_stored_item(&root_path, "biblio-ipc-other");
        let mut queue = Queue::open(&root_path.join("state/agent.db")).unwrap();
        let read = |queue: &mut Queue| {
            dispatch(
                queue,
                &root_path,
                Command::BibliographicMetadataRead {
                    item_id: item_id.clone(),
                    max_sensitivity: Sensitivity::Restricted,
                },
            )
            .unwrap()
            .bibliographic_metadata
            .unwrap()
        };
        let first = read(&mut queue);
        assert_eq!(first.metadata_schema_version, 2);
        assert!(first.identifiers.is_empty());
        let receipt = edit_receipt(
            &mut queue,
            &root_path,
            v2_edit(
                &item_id,
                &first.item.revision_id,
                "ipc-1",
                serde_json::json!({
                    "identifiers": [{"kind": "doi", "value": "https://doi.org/10.1000/IPC"}],
                    "published_date": {"value": null, "precision": "unparsed", "raw": "Spring 2024"}
                }),
            ),
        )
        .unwrap();
        let after = read(&mut queue);
        assert_eq!(after.item.revision_id, receipt.revision_id);
        assert_eq!(
            after.identifiers[0].comparison_key.as_deref(),
            Some("10.1000/ipc")
        );
        assert_eq!(
            after.identifiers[0].raw_value.as_deref(),
            Some("https://doi.org/10.1000/IPC")
        );
        assert_eq!(
            after.identifiers[0].validation.status,
            memex_core::ValidationStatus::Valid
        );
        assert_eq!(
            after.published_date.as_ref().unwrap().precision,
            memex_core::DatePrecision::Unparsed
        );
        assert_eq!(after.published_date_origin.as_deref(), Some("typed"));

        let error = dispatch(
            &mut queue,
            &root_path,
            Command::BibliographicMetadataEdit {
                max_sensitivity: Sensitivity::Restricted,
                edit: v2_edit(
                    &item_id,
                    &receipt.revision_id,
                    "ipc-2",
                    serde_json::json!({"number_of_pages": 0}),
                ),
            },
        )
        .unwrap_err();
        assert_eq!(
            Response::error(&error.to_string()).reason_code.as_deref(),
            Some("invalid_metadata_field:number_of_pages")
        );

        let duplicates = |exclude: Option<String>| {
            dispatch(
                &mut queue_for(&root_path, "dup"),
                &root_path,
                Command::BibliographicDuplicateCheck {
                    identifiers: vec![memex_core::ItemIdentifier::new(
                        memex_core::IdentifierKind::Doi,
                        "doi:10.1000/ipc",
                    )],
                    exclude_item_id: exclude,
                    max_sensitivity: Sensitivity::Restricted,
                },
            )
            .unwrap()
            .identifier_duplicates
            .unwrap()
        };
        assert_eq!(duplicates(None).len(), 1);
        assert_eq!(duplicates(None)[0].item_id, item_id);
        assert!(duplicates(Some(item_id.clone())).is_empty());
        let _ = other_id;

        let (network, calls) = canned(vec![json_response(200, &crossref_work("10.1000/IPC"))]);
        let preview = dispatch_with(
            &mut queue,
            &root_path,
            &network,
            lookup_command(&item_id, &receipt.revision_id, "doi", "10.1000/IPC"),
        )
        .unwrap()
        .lookup_preview
        .unwrap();
        assert_eq!(
            *calls.calls.lock().unwrap(),
            ["https://api.crossref.org/works/10.1000%2Fipc"]
        );
        let dates: Vec<_> = preview
            .candidates
            .iter()
            .filter(|c| c.field == "published_date")
            .collect();
        assert_eq!(dates.len(), 2);
        assert!(dates.iter().all(|c| c.conflict && !c.selected));
        let journal = preview
            .candidates
            .iter()
            .find(|c| c.field == "journal_title")
            .unwrap()
            .candidate_id
            .clone();
        let online = dates[1].candidate_id.clone();

        let mut forged = v2_edit(
            &item_id,
            &receipt.revision_id,
            "ipc-forged",
            serde_json::json!({}),
        );
        forged.preview_id = Some(preview.preview_id.clone());
        forged.selected_candidate_ids = vec!["candidate_forged".into()];
        let error = edit_receipt(&mut queue, &root_path, forged).unwrap_err();
        assert_eq!(
            Response::error(&error.to_string()).reason_code.as_deref(),
            Some("invalid_candidate_selection")
        );

        let mut apply = v2_edit(
            &item_id,
            &receipt.revision_id,
            "ipc-apply",
            serde_json::json!({}),
        );
        apply.preview_id = Some(preview.preview_id.clone());
        apply.selected_candidate_ids = vec![journal, online.clone()];
        let applied = edit_receipt(&mut queue, &root_path, apply).unwrap();
        let after = read(&mut queue);
        assert_eq!(after.item.revision_id, applied.revision_id);
        let journal_title = after.item.metadata.journal_title.as_ref().unwrap();
        assert_eq!(journal_title.value, "Journal of Synthetic IPC");
        assert_eq!(
            journal_title.provenance.origin,
            memex_core::MetadataOrigin::LookupSelected
        );
        assert_eq!(
            after.published_date.as_ref().unwrap().value.as_deref(),
            Some("2023-12-31")
        );
        assert_eq!(after.item.title, "Synthetic remote result biblio-ipc");

        let mut stale = v2_edit(
            &item_id,
            &applied.revision_id,
            "ipc-stale",
            serde_json::json!({}),
        );
        stale.preview_id = Some(preview.preview_id);
        stale.selected_candidate_ids = vec![online];
        let error = edit_receipt(&mut queue, &root_path, stale).unwrap_err();
        assert_eq!(
            Response::error(&error.to_string()).reason_code.as_deref(),
            Some("stale_item_revision")
        );
        assert_eq!(calls.calls.lock().unwrap().len(), 1, "apply never fetches");
    }

    fn queue_for(root: &Path, name: &str) -> Queue {
        Queue::open(&root.join(format!("state/agent-{name}.db"))).unwrap()
    }

    #[test]
    fn bibliographic_lookup_network_policy_rejects_before_or_without_following() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let paper = synthetic_stored_item(&root_path, "lookup-policy-paper");
        let book = synthetic_stored_item(&root_path, "lookup-policy-book");
        let mut setup = queue_for(&root_path, "setup");
        let revision = |item: &str| {
            memex_store::read_item(&root_path, item)
                .unwrap()
                .1
                .revision_id
        };
        let paper_rev = edit_receipt(
            &mut setup,
            &root_path,
            v2_edit(
                &paper,
                &revision(&paper),
                "policy-paper",
                serde_json::json!({
                    "identifiers": [{"kind": "doi", "value": "10.1000/policy"}]
                }),
            ),
        )
        .unwrap()
        .revision_id;
        let book_rev = edit_receipt(
            &mut setup,
            &root_path,
            v2_edit(
                &book,
                &revision(&book),
                "policy-book",
                serde_json::json!({
                    "identifiers": [{"kind": "isbn", "value": "0-306-40615-2"}]
                }),
            ),
        )
        .unwrap()
        .revision_id;
        let run = |name: &str, command: Command, responses: Vec<crate::fetch::RawResponse>| {
            let (network, calls) = canned(responses);
            let result = dispatch_with(
                &mut queue_for(&root_path, name),
                &root_path,
                &network,
                command,
            )
            .map(|response| response.lookup_preview.unwrap())
            .map_err(|error| Response::error(&error.to_string()).reason_code.unwrap());
            let calls = calls.calls.lock().unwrap().clone();
            (result, calls)
        };

        // Invalid or unowned identifiers are never fetched.
        for (name, kind, value, expected) in [
            ("invalid", "doi", "10.1000", "lookup_identifier_invalid"),
            (
                "checksum",
                "isbn",
                "0-306-40615-3",
                "lookup_identifier_invalid",
            ),
            (
                "unowned",
                "doi",
                "10.1000/other",
                "lookup_identifier_not_on_item",
            ),
            ("issn", "issn", "0317-8471", "lookup_identifier_unsupported"),
        ] {
            let (result, calls) = run(
                name,
                lookup_command(&paper, &paper_rev, kind, value),
                vec![json_response(200, &crossref_work(value))],
            );
            assert_eq!(result.unwrap_err(), expected, "{name}");
            assert!(calls.is_empty(), "{name}");
        }
        // Redirects away from the approved host or route are refused.
        let (result, calls) = run(
            "crossref-redirect",
            lookup_command(&paper, &paper_rev, "doi", "10.1000/policy"),
            vec![header_response(
                302,
                &[("location", "https://evil.example/works/10.1000%2Fpolicy")],
            )],
        );
        assert_eq!(result.unwrap_err(), "lookup_redirect_rejected");
        assert_eq!(calls.len(), 1);
        let (result, calls) = run(
            "ol-author",
            lookup_command(&book, &book_rev, "isbn", "0306406152"),
            vec![header_response(302, &[("location", "/authors/OL1A.json")])],
        );
        assert_eq!(result.unwrap_err(), "lookup_redirect_rejected");
        assert_eq!(calls, ["https://openlibrary.org/isbn/9780306406157.json"]);
        let (result, calls) = run(
            "ol-loop",
            lookup_command(&book, &book_rev, "isbn", "9780306406157"),
            vec![
                header_response(302, &[("location", "/books/OL7M.json")]),
                header_response(302, &[("location", "/books/OL8M.json")]),
            ],
        );
        assert_eq!(result.unwrap_err(), "lookup_redirect_rejected");
        assert_eq!(calls.len(), 2);
        // The one approved same-host edition redirect is followed.
        let edition = serde_json::json!({"type": {"key": "/type/edition"}, "isbn_13": ["9780306406157"],
            "publish_date": "Spring 1999", "authors": [{"key": "/authors/OL1A"}], "title": "Synthetic Book"});
        let (result, calls) = run(
            "ol-ok",
            lookup_command(&book, &book_rev, "isbn", "978-0-306-40615-7"),
            vec![
                header_response(302, &[("location", "/books/OL7M.json")]),
                json_response(200, &edition),
            ],
        );
        let preview = result.unwrap();
        assert_eq!(
            calls,
            [
                "https://openlibrary.org/isbn/9780306406157.json",
                "https://openlibrary.org/books/OL7M.json"
            ]
        );
        let date = preview
            .candidates
            .iter()
            .find(|c| c.field == "published_date")
            .unwrap();
        assert_eq!(
            date.proposed_value,
            Some(serde_json::json!({"value": null, "precision": "unparsed", "raw": "Spring 1999"}))
        );
        assert!(preview.candidates.iter().all(|c| c.field != "creators"));
        // Provider status and representation errors.
        let (result, _) = run(
            "missing",
            lookup_command(&paper, &paper_rev, "doi", "10.1000/policy"),
            vec![header_response(404, &[("content-type", "text/plain")])],
        );
        assert_eq!(result.unwrap_err(), "lookup_not_found");
        let (result, _) = run(
            "html",
            lookup_command(&paper, &paper_rev, "doi", "10.1000/policy"),
            vec![header_response(200, &[("content-type", "text/html")])],
        );
        assert_eq!(result.unwrap_err(), "invalid_lookup_response");
        let (result, _) = run(
            "mismatch",
            lookup_command(&paper, &paper_rev, "doi", "10.1000/policy"),
            vec![json_response(200, &crossref_work("10.1000/other"))],
        );
        assert_eq!(result.unwrap_err(), "lookup_identifier_mismatch");
        // Retry-After is recorded per origin; the next request waits without I/O.
        let mut limited = queue_for(&root_path, "limited");
        let (network, calls) = canned(vec![
            header_response(429, &[("retry-after", "120")]),
            json_response(200, &crossref_work("10.1000/policy")),
        ]);
        for _ in 0..2 {
            let error = dispatch_with(
                &mut limited,
                &root_path,
                &network,
                lookup_command(&paper, &paper_rev, "doi", "10.1000/policy"),
            )
            .unwrap_err();
            assert_eq!(error.to_string(), "lookup_rate_limited");
        }
        assert_eq!(calls.calls.lock().unwrap().len(), 1);
        // The fetcher itself refuses any non-approved URL without a request.
        let (network, calls) = canned(vec![json_response(200, &serde_json::json!({}))]);
        let error = crate::fetch::fetch_metadata_lookup(
            &url::Url::parse("https://api.crossref.org/works/10.1000%2Fpolicy?mailto=x").unwrap(),
            network.resolver_for_test(),
            network.transport_for_test(),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "lookup_policy_rejected");
        assert!(calls.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn crossref_contact_is_user_agent_only_and_only_when_configured() {
        let crossref = url::Url::parse("https://api.crossref.org/works/10.1000%2Fx").unwrap();
        let library = url::Url::parse("https://openlibrary.org/isbn/9780306406157.json").unwrap();
        let plain = crate::fetch::METADATA_LOOKUP_USER_AGENT;
        assert_eq!(crate::fetch::metadata_user_agent(&crossref, None), plain);
        assert_eq!(
            crate::fetch::metadata_user_agent(&crossref, Some("owner@example.invalid")),
            format!("{plain} mailto:owner@example.invalid")
        );
        assert_eq!(
            crate::fetch::metadata_user_agent(&library, Some("owner@example.invalid")),
            plain
        );
        assert_eq!(
            crate::fetch::metadata_user_agent(&crossref, Some("bad value\r\nX: y@z.c")),
            plain
        );
        for bad in ["", "no-at", "a@b", "a@@b.c", "a b@c.d", "a@-b.c", "a@b..c"] {
            assert!(!crate::config::valid_lookup_contact(bad), "{bad}");
        }
        let home = tempfile::tempdir().unwrap();
        let mut cfg = crate::config::AgentConfig::standard(&home.path().canonicalize().unwrap());
        assert!(cfg.metadata_lookup_contact.is_none());
        cfg.validate().unwrap();
        cfg.metadata_lookup_contact = Some("owner@example.invalid".into());
        cfg.validate().unwrap();
        cfg.metadata_lookup_contact = Some("not an address".into());
        assert_eq!(
            cfg.validate().unwrap_err().to_string(),
            "invalid_metadata_lookup_contact"
        );
        let json =
            serde_json::to_value(crate::config::AgentConfig::standard(Path::new("/h"))).unwrap();
        assert!(
            json.get("metadata_lookup_contact").is_none(),
            "old config shape unchanged"
        );
    }

    #[test]
    fn large_lookup_preview_is_paged_through_ipc_within_the_response_frame() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let item_id = synthetic_stored_item(&root_path, "large-lookup");
        let mut queue = Queue::open(&root_path.join("state/agent.db")).unwrap();
        let revision = memex_store::read_item(&root_path, &item_id)
            .unwrap()
            .1
            .revision_id;
        let revision = edit_receipt(
            &mut queue,
            &root_path,
            v2_edit(
                &item_id,
                &revision,
                "large-1",
                serde_json::json!({
                    "identifiers": [{"kind": "doi", "value": "10.1000/large"}]
                }),
            ),
        )
        .unwrap()
        .revision_id;
        let mut work = crossref_work("10.1000/large");
        work["message"]["abstract"] = "x".repeat(100 * 1024).into();
        work["message"]["reference"] = (0..2000)
            .map(|n| serde_json::json!({"key": format!("k{n}"), "unstructured": format!("Synthetic reference {n} with some descriptive text for size")}))
            .collect();
        let (network, _) = canned(vec![json_response(200, &work)]);
        let response = dispatch_with(
            &mut queue,
            &root_path,
            &network,
            lookup_command(&item_id, &revision, "doi", "10.1000/large"),
        )
        .unwrap();
        let body = response_body(&response).unwrap();
        assert!(body.len() < MAX_RESPONSE_FRAME / 2, "{}", body.len());
        let page = response.lookup_preview.unwrap();
        assert!(page
            .notes
            .contains(&"abstract_text_exceeds_bound".to_owned()));
        let refs = page
            .candidates
            .iter()
            .find(|c| c.field == "cited_references")
            .unwrap();
        assert_eq!(refs.proposed_value_summary.as_ref().unwrap().total, 2000);
        let read: Command = serde_json::from_value(serde_json::json!({
            "command": "bibliographic_lookup_preview_read", "preview_id": page.preview_id,
            "max_sensitivity": "restricted", "candidate_offset": 1, "candidate_limit": 2
        }))
        .unwrap();
        let second = dispatch(&mut queue, &root_path, read)
            .unwrap()
            .lookup_preview
            .unwrap();
        assert_eq!((second.candidate_offset, second.candidates.len()), (1, 2));
        assert_eq!(
            second.candidates[0].candidate_id,
            page.candidates[1].candidate_id
        );
        let value: Command = serde_json::from_value(serde_json::json!({
            "command": "bibliographic_lookup_value_read", "preview_id": page.preview_id,
            "candidate_id": refs.candidate_id, "side": "proposed", "offset": 0, "max_sensitivity": "restricted"
        }))
        .unwrap();
        let response = dispatch(&mut queue, &root_path, value).unwrap();
        assert!(response_body(&response).unwrap().len() < MAX_RESPONSE_FRAME / 2);
        let window = response.lookup_value.unwrap();
        assert_eq!(window.total, 2000);
        assert!(window
            .next_offset
            .is_some_and(|next| next > 0 && next < 2000));
        let hidden: Command = serde_json::from_value(serde_json::json!({
            "command": "bibliographic_lookup_preview_read", "preview_id": "preview_missing",
            "max_sensitivity": "restricted", "candidate_offset": 0, "candidate_limit": 1
        }))
        .unwrap();
        let error = dispatch(&mut queue, &root_path, hidden).unwrap_err();
        assert_eq!(
            Response::error(&error.to_string()).reason_code.as_deref(),
            Some("preview_unavailable")
        );
    }

    #[test]
    fn lookup_preview_of_a_private_item_is_unavailable_below_its_ceiling() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let item_id = synthetic_stored_item_at_sensitivity(
            &root_path,
            "private-lookup",
            Sensitivity::Private,
        );
        let mut queue = Queue::open(&root_path.join("state/agent.db")).unwrap();
        let revision = memex_store::read_item(&root_path, &item_id)
            .unwrap()
            .1
            .revision_id;
        let revision = edit_receipt(
            &mut queue,
            &root_path,
            v2_edit(
                &item_id,
                &revision,
                "private-1",
                serde_json::json!({
                    "identifiers": [{"kind": "doi", "value": "10.1000/private"}]
                }),
            ),
        )
        .unwrap()
        .revision_id;
        let (network, _) = canned(vec![json_response(200, &crossref_work("10.1000/private"))]);
        let page = dispatch_with(
            &mut queue,
            &root_path,
            &network,
            lookup_command(&item_id, &revision, "doi", "10.1000/private"),
        )
        .unwrap()
        .lookup_preview
        .unwrap();
        for ceiling in ["public", "internal"] {
            let read: Command = serde_json::from_value(serde_json::json!({
                "command": "bibliographic_lookup_preview_read", "preview_id": page.preview_id,
                "max_sensitivity": ceiling, "candidate_offset": 0, "candidate_limit": 1
            }))
            .unwrap();
            let error = dispatch(&mut queue, &root_path, read).unwrap_err();
            assert_eq!(error.to_string(), "preview_unavailable", "{ceiling}");
        }
    }

    #[test]
    fn group_tree_and_group_query_round_trip_through_ipc_without_hidden_leakage() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let shared = synthetic_stored_item(&root_path, "group-shared");
        let hidden =
            synthetic_stored_item_at_sensitivity(&root_path, "group-hidden", Sensitivity::Private);
        let mut queue = Queue::open(&root_path.join("state/agent.db")).unwrap();
        let all = Sensitivity::Restricted;
        let r = memex_store::edit_groups(
            &root_path,
            "ipc-g1",
            memex_store::GroupEditOp::CreateRoot {
                name: "Root".into(),
                sensitivity: Sensitivity::Public,
            },
            all,
        )
        .unwrap();
        let p = memex_store::edit_groups(
            &root_path,
            "ipc-g2",
            memex_store::GroupEditOp::CreateFolder {
                parent_id: r.group_id.clone(),
                expected_parent_version: r.version.clone(),
                name: "Parent".into(),
            },
            all,
        )
        .unwrap();
        let k = memex_store::edit_groups(
            &root_path,
            "ipc-g3",
            memex_store::GroupEditOp::CreateFolder {
                parent_id: p.group_id.clone(),
                expected_parent_version: p.version.clone(),
                name: "Child".into(),
            },
            all,
        )
        .unwrap();
        let tree = memex_store::read_group_tree(&root_path, all, 0, 500, None).unwrap();
        let version = |id: &str| {
            tree.nodes
                .iter()
                .find(|n| n.id == id)
                .unwrap()
                .version
                .clone()
        };
        memex_store::edit_groups(
            &root_path,
            "ipc-g4",
            memex_store::GroupEditOp::AddMembers {
                group_id: p.group_id.clone(),
                expected_version: version(&p.group_id),
                item_ids: vec![shared.clone()],
            },
            all,
        )
        .unwrap();
        memex_store::edit_groups(
            &root_path,
            "ipc-g5",
            memex_store::GroupEditOp::AddMembers {
                group_id: k.group_id.clone(),
                expected_version: k.version.clone(),
                item_ids: vec![shared.clone(), hidden.clone()],
            },
            all,
        )
        .unwrap();

        let read: Command = serde_json::from_value(serde_json::json!({
            "command": "group_tree_read", "max_sensitivity": "public", "offset": 0, "limit": 50, "expected_view": null
        }))
        .unwrap();
        let page = dispatch(&mut queue, &root_path, read)
            .unwrap()
            .group_tree
            .unwrap();
        let counts: Vec<_> = page
            .nodes
            .iter()
            .map(|n| (n.name.as_str(), n.item_count, n.subtree_item_count))
            .collect();
        assert_eq!(counts, [("Root", 0, 1), ("Parent", 1, 1), ("Child", 1, 1)]);

        let query: Command = serde_json::from_value(serde_json::json!({
            "command": "library_query",
            "query": {"text": "", "source": null, "item_type": null, "sort": "title", "descending": false,
                      "group_id": p.group_id, "include_descendants": true},
            "max_sensitivity": "public", "offset": 0, "limit": 50, "expected_view": null
        }))
        .unwrap();
        let result = dispatch(&mut queue, &root_path, query)
            .unwrap()
            .library_query
            .unwrap();
        assert_eq!(result.total, 1);
        assert_eq!(result.items[0].item_id, shared);
        let facets: Vec<_> = result
            .groups
            .iter()
            .map(|f| (f.name.as_str(), f.count, f.subtree_count))
            .collect();
        assert_eq!(facets, [("Child", 1, 1), ("Parent", 1, 1), ("Root", 0, 1)]);
        let bad: Command = serde_json::from_value(serde_json::json!({
            "command": "library_query",
            "query": {"text": "", "source": null, "item_type": null, "sort": "title", "descending": false, "include_descendants": true},
            "max_sensitivity": "public", "offset": 0, "limit": 50, "expected_view": null
        }))
        .unwrap();
        let error = dispatch(&mut queue, &root_path, bad).unwrap_err();
        assert_eq!(
            Response::error(&error.to_string()).reason_code.as_deref(),
            Some("invalid_group_query")
        );
    }

    fn all_routes() -> crate::translation_request::RequestRoutes {
        crate::translation_request::RequestRoutes {
            translation: true,
            minutes: true,
            metadata: true,
        }
    }

    #[test]
    fn chinese_source_gets_requested_minutes_and_metadata_but_no_translation() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let item = synthetic_stored_item(&root_path, "chinese-minutes");
        let database = root_path.join("state/agent.db");
        let mut queue = Queue::open(&database).unwrap();
        let fake = crate::provider_generation::tests::FakeProvider::default();
        *fake.metadata_reply.lock().unwrap() = Some(
            r#"{"keywords":["中文文章","语言判断","测试"],"creators":[],"published_date":null}"#
                .into(),
        );
        let mut provider = ProviderWorker::new(Box::new(fake.clone()));
        let mut reconciler = AnalysisReconciler::new(Duration::from_secs(3600));
        let (translation, minutes) = crate::translation_request::request(
            &root_path,
            &item,
            Sensitivity::Restricted,
            all_routes(),
        )
        .unwrap();
        assert_eq!(translation.state, "not_requested");
        assert_eq!(translation.reason.as_deref(), Some("source_is_chinese"));
        // translation_request reports minutes but never authorizes them.
        assert_eq!(minutes.state, "not_requested");
        let minutes = crate::translation_request::minutes_request(
            &root_path,
            &item,
            Sensitivity::Restricted,
            true,
        )
        .unwrap();
        assert_eq!(minutes.state, "queued");
        run_worker_until_idle(&root_path, &database, &mut provider, &mut reconciler);
        // Metadata filled the empty keywords with model provenance in a new revision; the
        // minutes authorization moved to that revision and ran there.
        let (_, revision) = memex_store::read_item(&root_path, &item).unwrap();
        assert_eq!(revision.revision_number, 2);
        let keywords = revision.metadata.keywords.unwrap();
        assert_eq!(keywords.value, ["中文文章", "语言判断", "测试"]);
        assert_eq!(
            keywords.provenance.origin,
            memex_core::MetadataOrigin::ModelSuggested
        );
        assert!(keywords
            .provenance
            .provider_id
            .ends_with("synthetic-provider-model"));
        let read = library_item(&mut queue, &root_path, &item);
        assert_eq!(read.revision_id, revision.revision_id);
        let minutes = read.ai_minutes.unwrap();
        assert_eq!(minutes.state, "succeeded");
        assert_eq!(
            minutes.text.as_deref(),
            Some(crate::provider_generation::tests::FAKE_MINUTES)
        );
        assert!(matches!(
            read.translation_state,
            LibraryTranslationState::NotRequested
        ));
        assert!(!fake
            .systems
            .lock()
            .unwrap()
            .iter()
            .any(|system| system.starts_with(memex_analysis::FAITHFUL_TRANSLATION_PROMPT)));
        // The whole document was the minutes input.
        assert!(fake
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call.contains(&revision.content_markdown) && call.contains("Title: ")));
    }

    #[test]
    fn model_metadata_fills_only_empty_fields_and_drops_on_a_concurrent_edit() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let item = synthetic_stored_item(&root_path, "english-metadata");
        let mut queue = Queue::open(&root_path.join("state/agent.db")).unwrap();
        let (_, base) = memex_store::read_item(&root_path, &item).unwrap();
        let provenance = memex_core::MetadataProvenance::new(
            memex_core::MetadataOrigin::ModelSuggested,
            "memex-provider-chat:synthetic",
            "2:settings-1:credential-1:prompt-1",
        );
        let suggestion = |words: &[&str]| memex_store::ModelMetadataSuggestion {
            keywords: Some(words.iter().map(|word| (*word).to_owned()).collect()),
            creators: None,
            published_date: None,
        };
        // A user edit lands first: the suggestion for the old revision is dropped.
        let edited = dispatch(
            &mut queue,
            &root_path,
            Command::UpdateItemMetadata {
                item_id: item.clone(),
                expected_revision_id: base.revision_id.clone(),
                title: "Owner title".into(),
                manual_tags: vec!["mine".into()],
            },
        )
        .unwrap()
        .item_update
        .unwrap();
        let stale = memex_store::apply_model_metadata_suggestion(
            &root_path,
            &item,
            &base.revision_id,
            suggestion(&["a", "b", "c"]),
            provenance.clone(),
        )
        .unwrap_err();
        assert_eq!(stale.to_string(), "stale_item_revision");
        let filled = memex_store::apply_model_metadata_suggestion(
            &root_path,
            &item,
            &edited.revision_id,
            suggestion(&["a", "b", "c"]),
            provenance.clone(),
        )
        .unwrap()
        .unwrap();
        // The same suggestion again changes nothing (no new revision).
        assert!(memex_store::apply_model_metadata_suggestion(
            &root_path,
            &item,
            &filled.revision_id,
            suggestion(&["a", "b", "c"]),
            provenance.clone(),
        )
        .unwrap()
        .is_none());
        // A newer model suggestion replaces an earlier model value (never owner values).
        let filled = memex_store::apply_model_metadata_suggestion(
            &root_path,
            &item,
            &filled.revision_id,
            suggestion(&["x", "y", "z"]),
            provenance,
        )
        .unwrap()
        .unwrap();
        let (_, current) = memex_store::read_item(&root_path, &item).unwrap();
        assert_eq!(current.title, "Owner title");
        assert_eq!(current.metadata.manual_tags.unwrap().value, ["mine"]);
        assert_eq!(current.metadata.keywords.unwrap().value, ["x", "y", "z"]);
        // The owner can keep editing on top of the model revision.
        dispatch(
            &mut queue,
            &root_path,
            Command::UpdateItemMetadata {
                item_id: item.clone(),
                expected_revision_id: filled.revision_id,
                title: "Owner title again".into(),
                manual_tags: Vec::new(),
            },
        )
        .unwrap();
        assert!(memex_library::read_entry(&root_path, &item, Sensitivity::Restricted).is_ok());
    }

    fn chat_message(role: &str, content: &str) -> crate::item_chat::ChatMessage {
        serde_json::from_value(serde_json::json!({"role": role, "content": content})).unwrap()
    }

    #[test]
    fn item_chat_grounds_on_the_document_and_validates_requests() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let item = synthetic_stored_item(&root_path, "english-chat");
        let (_, revision) = memex_store::read_item(&root_path, &item).unwrap();
        let fake = crate::provider_generation::tests::FakeProvider::default();
        let database = root_path.join("state/agent.db");
        drop(Queue::open(&database).unwrap());
        let request_id = "chat-request-0001";
        let ask = |messages: &[crate::item_chat::ChatMessage], id: &str| {
            crate::item_chat::handle(
                &root_path,
                &database,
                &fake,
                &item,
                Sensitivity::Restricted,
                id,
                messages,
            )
        };
        let reply = ask(
            &[
                chat_message("user", "What is this about?"),
                chat_message("assistant", "A synthetic article."),
                chat_message("user", "Who wrote it?"),
            ],
            request_id,
        )
        .unwrap();
        assert_eq!(reply.request_id, request_id);
        assert_eq!(reply.model, "Synthetic Sol");
        assert_eq!(reply.reply, "合成回答：共3轮。");
        assert_eq!(reply.finish, memex_provider::generation::Finish::Stop);
        let system = fake.systems.lock().unwrap().last().unwrap().clone();
        assert!(system.contains(&revision.content_markdown));
        assert!(system.contains(&format!("Title: {}", revision.title)));
        assert!(system.contains("Simplified Chinese"));
        let calls = fake.calls.lock().unwrap().len();
        // Validation happens before any provider call.
        assert_eq!(
            ask(
                &[chat_message("user", "q"), chat_message("assistant", "a")],
                request_id
            ),
            Err("invalid_chat_request")
        );
        assert_eq!(
            ask(&[chat_message("user", "q")], "short"),
            Err("invalid_chat_request")
        );
        let many: Vec<_> = (0..41).map(|_| chat_message("user", "q")).collect();
        assert_eq!(ask(&many, request_id), Err("invalid_chat_request"));
        let big = "x".repeat(16 * 1024 + 1);
        assert_eq!(
            ask(&[chat_message("user", &big)], request_id),
            Err("invalid_chat_request")
        );
        assert_eq!(fake.calls.lock().unwrap().len(), calls);
        assert_eq!(
            crate::item_chat::handle(
                &root_path,
                &database,
                &fake,
                "item_missing",
                Sensitivity::Restricted,
                request_id,
                &[chat_message("user", "q")],
            ),
            Err("library_resource_not_found")
        );
        fake.missing
            .lock()
            .unwrap()
            .push(memex_models::Purpose::Chat);
        assert_eq!(
            ask(&[chat_message("user", "q")], "chat-request-0002"),
            Err("no_chat_provider")
        );
        fake.missing.lock().unwrap().clear();
        *fake.fail.lock().unwrap() =
            Some(memex_provider::generation::GenerationError::OutcomeUnknown);
        assert_eq!(
            ask(&[chat_message("user", "q")], "chat-request-0003"),
            Err("provider_outcome_unknown")
        );
        // Wire shape: unknown roles and fields are rejected; content is not in Debug output.
        let command: Command = serde_json::from_value(serde_json::json!({
            "command": "item_chat", "item_id": item, "max_sensitivity": "restricted",
            "request_id": request_id, "messages": [{"role": "user", "content": "secret words"}]
        }))
        .unwrap();
        assert!(!format!("{command:?}").contains("secret words"));
        assert!(serde_json::from_value::<Command>(serde_json::json!({
            "command": "item_chat", "item_id": item, "max_sensitivity": "restricted",
            "request_id": request_id, "messages": [{"role": "system", "content": "x"}]
        }))
        .is_err());
        assert_eq!(
            Response::error("no_chat_provider").reason_code.as_deref(),
            Some("no_chat_provider")
        );
    }

    #[test]
    fn item_chat_history_appends_successes_once_in_order_per_item_under_the_ceiling() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let first =
            synthetic_stored_item_at_sensitivity(&root_path, "english-history", Sensitivity::Internal);
        let second = synthetic_stored_item(&root_path, "english-history-other");
        let database = root_path.join("state/agent.db");
        let mut queue = Queue::open(&database).unwrap();
        let fake = crate::provider_generation::tests::FakeProvider::default();
        let ask = |item: &str, id: &str, question: &str| {
            crate::item_chat::handle(
                &root_path,
                &database,
                &fake,
                item,
                Sensitivity::Restricted,
                id,
                &[chat_message("user", question)],
            )
        };
        let read = |queue: &mut Queue, item: &str, ceiling: &str, cursor: Option<&str>| {
            let command: Command = serde_json::from_value(serde_json::json!({
                "command": "conversation_read", "item_id": item, "max_sensitivity": ceiling,
                "cursor": cursor
            }))
            .unwrap();
            dispatch(queue, &root_path, command).map(|response| response.conversation.unwrap())
        };
        let reason = |error: anyhow::Error| Response::error(&error.to_string()).reason_code;
        let empty = read(&mut queue, &first, "internal", None).unwrap();
        assert!(empty.turns.is_empty());
        assert_eq!(empty.version, "v1:0:0");
        assert_eq!(empty.next_cursor, None);

        // Failed and unknown outcomes append nothing.
        fake.missing.lock().unwrap().push(memex_models::Purpose::Chat);
        assert_eq!(ask(&first, "history-request-fail1", "q1"), Err("no_chat_provider"));
        fake.missing.lock().unwrap().clear();
        *fake.fail.lock().unwrap() =
            Some(memex_provider::generation::GenerationError::OutcomeUnknown);
        assert_eq!(
            ask(&first, "history-request-fail2", "q1"),
            Err("provider_outcome_unknown")
        );
        *fake.fail.lock().unwrap() = None;
        assert!(read(&mut queue, &first, "internal", None).unwrap().turns.is_empty());

        // A success appends the question and the reply, in that order.
        let reply = ask(&first, "history-request-0001", "First question").unwrap();
        let calls = fake.calls.lock().unwrap().len();
        let page = read(&mut queue, &first, "internal", None).unwrap();
        assert_eq!(page.turns.len(), 2);
        assert_eq!(page.turns[0].role, crate::item_chat::ChatMessageRole::User);
        assert_eq!(page.turns[0].content, "First question");
        assert_eq!(page.turns[1].role, crate::item_chat::ChatMessageRole::Assistant);
        assert_eq!(page.turns[1].content, reply.reply);
        assert_eq!(page.turns[1].model.as_deref(), Some("Synthetic Sol"));
        assert!(page.turns.iter().all(|turn| turn.request_id == "history-request-0001"));
        assert!(page.turns[0].created_at_ms <= page.turns[1].created_at_ms);
        assert_eq!(page.version, format!("v1:2:{}", page.turns[1].seq));
        assert!(page.turns.iter().all(|turn| turn.origin.is_none()));
        assert!(!format!("{page:?}").contains("First question"));

        // Replay returns the stored reply without a model call and duplicates nothing.
        assert_eq!(ask(&first, "history-request-0001", "First question"), Ok(reply.clone()));
        assert_eq!(fake.calls.lock().unwrap().len(), calls);
        assert_eq!(read(&mut queue, &first, "internal", None).unwrap(), page);
        // The same id for another item or another question is a conflict, not a new turn.
        assert_eq!(
            ask(&second, "history-request-0001", "First question"),
            Err("request_identity_conflict")
        );
        assert_eq!(
            ask(&first, "history-request-0001", "Other question"),
            Err("request_identity_conflict")
        );
        assert_eq!(fake.calls.lock().unwrap().len(), calls);

        // Two items never mix; each keeps its own order.
        ask(&second, "history-request-0002", "Second item question").unwrap();
        ask(&first, "history-request-0003", "Follow-up").unwrap();
        let first_page = read(&mut queue, &first, "internal", None).unwrap();
        let contents: Vec<&str> =
            first_page.turns.iter().step_by(2).map(|turn| turn.content.as_str()).collect();
        assert_eq!(contents, ["First question", "Follow-up"]);
        assert!(first_page.turns.windows(2).all(|pair| pair[0].seq < pair[1].seq));
        assert_ne!(first_page.version, page.version);
        let second_page = read(&mut queue, &second, "public", None).unwrap();
        assert_eq!(second_page.turns.len(), 2);
        assert_eq!(second_page.turns[0].content, "Second item question");

        // Ceiling: an item above it does not exist; reading authorizes and appends nothing.
        assert_eq!(
            reason(read(&mut queue, &first, "public", None).unwrap_err()).as_deref(),
            Some("library_resource_not_found")
        );
        assert_eq!(
            reason(read(&mut queue, "item_missing", "restricted", None).unwrap_err()).as_deref(),
            Some("library_resource_not_found")
        );
        for bad in ["", "-", "--1", "1a", "+1", "1234567890123456789"] {
            assert_eq!(
                reason(read(&mut queue, &first, "internal", Some(bad)).unwrap_err()).as_deref(),
                Some("invalid_conversation_request")
            );
        }
        assert_eq!(read(&mut queue, &first, "internal", None).unwrap(), first_page);
    }

    #[test]
    fn conversation_import_is_owner_only_idempotent_ordered_and_used_as_chat_context() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let item =
            synthetic_stored_item_at_sensitivity(&root_path, "english-restore", Sensitivity::Private);
        let other = synthetic_stored_item(&root_path, "english-restore-other");
        let database = root_path.join("state/agent.db");
        let mut queue = Queue::open(&database).unwrap();
        let fake = crate::provider_generation::tests::FakeProvider::default();
        let import = |queue: &mut Queue, item: &str, origin: &str, turns: serde_json::Value| {
            let command: Command = serde_json::from_value(serde_json::json!({
                "command": "conversation_import", "item_id": item, "origin": origin, "turns": turns
            }))?;
            dispatch(queue, &root_path, command).map(|response| response.conversation_import.unwrap())
        };
        let read = |queue: &mut Queue, item: &str, ceiling: &str| {
            let command: Command = serde_json::from_value(serde_json::json!({
                "command": "conversation_read", "item_id": item, "max_sensitivity": ceiling
            }))
            .unwrap();
            dispatch(queue, &root_path, command).unwrap().conversation.unwrap()
        };
        let reason = |error: anyhow::Error| Response::error(&error.to_string()).reason_code;
        let origin = crate::item_chat::RESTORED_FROM_USER_SCREENSHOT;
        let screenshot = serde_json::json!([
            {"role": "user", "content": "Restored question?", "created_at_ms": 1_000},
            {"role": "assistant", "content": "Restored **answer**.", "created_at_ms": 2_000}
        ]);

        // Validation precedes any write.
        for (turns, origin) in [
            (serde_json::json!([]), origin),
            (screenshot.clone(), "user_typed"),
            (serde_json::json!([{"role": "user", "content": "  "}]), origin),
            (serde_json::json!([{"role": "user", "content": "x".repeat(16 * 1024 + 1)}]), origin),
            (serde_json::json!([{"role": "user", "content": "a", "created_at_ms": 5},
                                {"role": "assistant", "content": "b"}]), origin),
            (serde_json::json!([{"role": "user", "content": "a", "created_at_ms": 5},
                                {"role": "assistant", "content": "b", "created_at_ms": 4}]), origin),
        ] {
            assert_eq!(
                reason(import(&mut queue, &item, origin, turns).unwrap_err()).as_deref(),
                Some("invalid_conversation_import")
            );
        }
        // Roles other than user/assistant and unknown fields never parse.
        for turns in [
            serde_json::json!([{"role": "system", "content": "x"}]),
            serde_json::json!([{"role": "user", "content": "x", "request_id": "chosen-by-caller"}]),
        ] {
            assert!(import(&mut queue, &item, origin, turns).is_err());
        }
        assert_eq!(
            reason(import(&mut queue, "item_missing", origin, screenshot.clone()).unwrap_err())
                .as_deref(),
            Some("library_resource_not_found")
        );
        assert!(read(&mut queue, &item, "private").turns.is_empty());

        // Imported once, in order, marked; a replay is a no-op.
        let first = import(&mut queue, &item, origin, screenshot.clone()).unwrap();
        assert_eq!((first.imported, first.already_imported), (2, false));
        let page = read(&mut queue, &item, "private");
        assert_eq!(page.version, first.version);
        let contents: Vec<&str> = page.turns.iter().map(|turn| turn.content.as_str()).collect();
        assert_eq!(contents, ["Restored question?", "Restored **answer**."]);
        assert!(page.turns.iter().all(|turn| turn.origin.as_deref() == Some(origin)
            && turn.request_id.starts_with("restored:")));
        assert_eq!(page.turns[0].created_at_ms, 1_000);
        let again = import(&mut queue, &item, origin, screenshot.clone()).unwrap();
        assert_eq!((again.imported, again.already_imported), (0, true));
        assert_eq!(read(&mut queue, &item, "private"), page);
        // The restored namespace cannot be used (or replayed) through item_chat.
        assert_eq!(
            crate::item_chat::handle(&root_path, &database, &fake, &item, Sensitivity::Restricted,
                &page.turns[1].request_id, &[chat_message("user", "Restored question?")]),
            Err("invalid_chat_request")
        );

        // A follow-up chat sends the restored turns as context and lands after them.
        let mut messages: Vec<crate::item_chat::ChatMessage> = page
            .turns
            .iter()
            .map(|turn| chat_message(if turn.role == crate::item_chat::ChatMessageRole::User { "user" } else { "assistant" }, &turn.content))
            .collect();
        messages.push(chat_message("user", "And the follow-up?"));
        let reply = crate::item_chat::handle(&root_path, &database, &fake, &item,
            Sensitivity::Restricted, "restore-followup-0001", &messages).unwrap();
        assert_eq!(reply.reply, "合成回答：共3轮。", "both restored turns were in the request");
        let after = read(&mut queue, &item, "private");
        assert_eq!(after.turns.len(), 4);
        assert_eq!(after.turns[2].content, "And the follow-up?");
        assert_eq!(after.turns[2].origin, None);
        assert_ne!(after.version, page.version);

        // An item with turns: only strictly older restored turns may go before them.
        crate::item_chat::handle(&root_path, &database, &fake, &other, Sensitivity::Restricted,
            "restore-other-0001", &[chat_message("user", "Live question")]).unwrap();
        let live = read(&mut queue, &other, "public");
        let undated = serde_json::json!([{"role": "user", "content": "Old?"}]);
        let newer = serde_json::json!([{"role": "user", "content": "Old?", "created_at_ms": live.turns[0].created_at_ms}]);
        for turns in [undated, newer] {
            assert_eq!(
                reason(import(&mut queue, &other, origin, turns).unwrap_err()).as_deref(),
                Some("conversation_not_empty")
            );
        }
        assert_eq!(read(&mut queue, &other, "public"), live);
        let older = serde_json::json!([
            {"role": "user", "content": "Old?", "created_at_ms": 10},
            {"role": "assistant", "content": "Old answer.", "created_at_ms": 20}
        ]);
        assert_eq!(import(&mut queue, &other, origin, older).unwrap().imported, 2);
        let merged = read(&mut queue, &other, "public");
        let order: Vec<&str> = merged.turns.iter().map(|turn| turn.content.as_str()).collect();
        assert_eq!(&order[..3], ["Old?", "Old answer.", "Live question"]);
        assert!(merged.turns.windows(2).all(|pair| pair[0].seq < pair[1].seq));
        // Paging across negative (restored) sequence numbers resumes exactly.
        let command: Command = serde_json::from_value(serde_json::json!({
            "command": "conversation_read", "item_id": other, "max_sensitivity": "public",
            "cursor": merged.turns[1].seq.to_string()
        }))
        .unwrap();
        let rest = dispatch(&mut queue, &root_path, command).unwrap().conversation.unwrap();
        assert_eq!(rest.turns, merged.turns[2..]);
        // The item's ceiling still applies to restored turns.
        assert_eq!(
            reason({
                let command: Command = serde_json::from_value(serde_json::json!({
                    "command": "conversation_read", "item_id": item, "max_sensitivity": "public"
                })).unwrap();
                dispatch(&mut queue, &root_path, command).unwrap_err()
            }).as_deref(),
            Some("library_resource_not_found")
        );
    }

    #[test]
    fn conversation_read_pages_stay_bounded_and_resume_exactly() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let item = synthetic_stored_item(&root_path, "english-history-pages");
        let database = root_path.join("state/agent.db");
        let mut queue = Queue::open(&database).unwrap();
        for index in 0..30 {
            assert!(crate::item_chat::record_synthetic_exchange(
                &root_path,
                &database,
                &item,
                &format!("history-page-{index:04}-padding"),
                &format!("问题 {index} {}", "q".repeat(1500)),
                &format!("回答 {index} {}", "答".repeat(1200)),
                "Synthetic Sol",
            )
            .unwrap());
        }
        // One reply larger than a whole page is cut honestly, not dropped.
        crate::item_chat::record_synthetic_exchange(
            &root_path,
            &database,
            &item,
            "history-page-huge-padding",
            "Huge?",
            &"长".repeat(30_000),
            "Synthetic Sol",
        )
        .unwrap();
        let mut cursor: Option<String> = None;
        let mut turns = Vec::new();
        let mut pages = 0;
        loop {
            let command: Command = serde_json::from_value(serde_json::json!({
                "command": "conversation_read", "item_id": item, "max_sensitivity": "public",
                "cursor": cursor
            }))
            .unwrap();
            let response = dispatch(&mut queue, &root_path, command).unwrap();
            assert!(response_body(&response).unwrap().len() < 64 * 1024);
            let page = response.conversation.unwrap();
            pages += 1;
            turns.extend(page.turns);
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        assert!(pages > 1);
        assert_eq!(turns.len(), 62);
        assert!(turns.windows(2).all(|pair| pair[0].seq < pair[1].seq));
        assert!(turns[..60].iter().all(|turn| !turn.truncated));
        assert!(turns[61].truncated && turns[61].content.starts_with('长'));
        assert!(turns[59].content.starts_with("回答 29"));
    }

    #[test]
    fn scoped_insights_status_reads_under_the_ceiling_and_never_authorizes() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let item = synthetic_stored_item_at_sensitivity(
            &root_path,
            "long-english-insights-status",
            Sensitivity::Internal,
        );
        let mut queue = Queue::open(&root_path.join("state/agent.db")).unwrap();
        let status = |queue: &mut Queue, ceiling: &str| {
            let command: Command = serde_json::from_value(serde_json::json!({
                "command": "scoped_insights_status", "item_id": item, "max_sensitivity": ceiling
            }))
            .unwrap();
            dispatch(queue, &root_path, command).map(|response| response.ai_insights.unwrap())
        };
        let error = status(&mut queue, "public").unwrap_err();
        assert_eq!(
            Response::error(&error.to_string()).reason_code.as_deref(),
            Some("library_resource_not_found")
        );
        let before = status(&mut queue, "internal").unwrap();
        for view in [&before.highlights, &before.quotes, &before.fact_check] {
            assert_eq!(view.state, "not_requested");
            assert!(view.items.is_none());
        }
        assert_eq!(before.fact_check.basis.as_deref(), Some("model_knowledge"));
        // Reading twice enqueues nothing.
        assert_eq!(status(&mut queue, "internal").unwrap(), before);
        assert!(memex_analysis::InsightQueue::open(&root_path)
            .unwrap()
            .queued_kinds()
            .unwrap()
            .is_empty());
        // After the explicit request, the read reports the same queued states.
        let requested = crate::insights::request(
            &root_path,
            &item,
            Sensitivity::Internal,
            &["quotes".to_owned()],
            |_| true,
        )
        .unwrap();
        assert_eq!(requested.quotes.state, "queued");
        assert_eq!(status(&mut queue, "internal").unwrap(), requested);
    }

    #[test]
    fn scoped_insights_status_drops_the_largest_items_honestly_when_they_would_not_fit() {
        struct Oversized;
        impl memex_analysis::InsightModel for Oversized {
            fn descriptor(&self) -> TranslationModelDescriptor {
                TranslationModelDescriptor {
                    model_id: "synthetic-insights".into(),
                    model_sha256: format!("sha256:{}", "a".repeat(64)),
                    adapter_id: "synthetic-insights".into(),
                    adapter_version: "1".into(),
                    request_timeout: Duration::from_secs(1),
                }
            }
            fn generate(
                &mut self,
                input: memex_analysis::InsightModelInput<'_>,
            ) -> std::result::Result<memex_analysis::InsightOutput, memex_analysis::TranslationModelError>
            {
                // Real item shapes, oversized together: ~105 KiB + 2 x ~65 KiB.
                let count = match input.kind() {
                    memex_analysis::InsightKind::Highlights => 200,
                    _ => 100,
                };
                let items: Vec<serde_json::Value> = (0..count)
                    .map(|index| match input.kind() {
                        memex_analysis::InsightKind::Highlights => serde_json::json!({
                            "block_index": 0, "quote": format!("{index}{}", "h".repeat(480)),
                            "style": "highlight", "label": null}),
                        memex_analysis::InsightKind::Quotes => serde_json::json!({
                            "block_index": 0, "quote": "q".repeat(400),
                            "translation_zh": null, "why": "w".repeat(200)}),
                        memex_analysis::InsightKind::FactCheck => serde_json::json!({
                            "block_index": 0, "quote": "f".repeat(400), "verdict": "opinion",
                            "explanation": "e".repeat(200), "confidence": "low"}),
                    })
                    .collect();
                Ok(memex_analysis::InsightOutput {
                    items: serde_json::Value::Array(items),
                    dropped: 0,
                    translation_used: false,
                })
            }
        }
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let item = synthetic_stored_item(&root_path, "long-english-insights-oversized");
        let all = ["highlights".to_owned(), "quotes".into(), "fact_check".into()];
        crate::insights::request(&root_path, &item, Sensitivity::Public, &all, |_| true).unwrap();
        let mut insight_queue = memex_analysis::InsightQueue::open(&root_path).unwrap();
        for kind in memex_analysis::InsightKind::ALL {
            assert!(insight_queue.process_next(kind, &mut Oversized).unwrap());
        }
        let status = crate::translation_request::insights_status(
            &root_path,
            &item,
            Sensitivity::Public,
        )
        .unwrap();
        assert!(serde_json::to_vec(&status).unwrap().len() <= 192 * 1024);
        let views = [&status.highlights, &status.quotes, &status.fact_check];
        assert!(views.iter().all(|view| view.state == "succeeded"));
        // The largest kind (highlights) lost its items and says so; the others are intact.
        assert!(status.highlights.items.is_none());
        assert_eq!(
            status.highlights.reason.as_deref(),
            Some("insights_too_large_for_read")
        );
        assert_eq!(status.quotes.items.as_ref().unwrap().as_array().unwrap().len(), 100);
        assert_eq!(status.fact_check.reason, None);
    }

    #[test]
    fn slow_item_chat_does_not_block_library_query_on_the_socket() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let item = synthetic_stored_item(&root_path, "english-concurrent");
        let socket = root_path.join("ipc/agent.sock");
        let listener = bind_owner_socket(&socket).unwrap();
        let fake = crate::provider_generation::tests::FakeProvider::default();
        *fake.chat_delay.lock().unwrap() = Duration::from_secs(3);
        let source: std::sync::Arc<dyn crate::provider_generation::ProviderSource> =
            std::sync::Arc::new(fake.clone());
        let server_root = root_path.clone();
        let server = std::thread::spawn(move || {
            let mut queue = Queue::open(&server_root.join("state/agent.db")).unwrap();
            let mut queries = memex_library::LibraryQuery::new(&server_root).unwrap();
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                serve_with_chat(
                    &mut stream,
                    &mut queue,
                    &server_root,
                    &mut queries,
                    &offline_network(),
                    &source,
                )
                .unwrap();
            }
        });
        let send = |command: serde_json::Value, timeout: Duration| {
            let mut stream = UnixStream::connect(&socket).unwrap();
            stream.set_read_timeout(Some(timeout)).unwrap();
            write_frame(&mut stream, &serde_json::to_vec(&command).unwrap()).unwrap();
            stream
        };
        let started = std::time::Instant::now();
        let mut chat = send(
            serde_json::json!({
                "command": "item_chat", "item_id": item, "max_sensitivity": "restricted",
                "request_id": "concurrency-test-0001",
                "messages": [{"role": "user", "content": "Summarize it."}]
            }),
            Duration::from_secs(10),
        );
        let mut query = send(
            serde_json::json!({
                "command": "library_query",
                "query": {"text": "", "source": null, "item_type": null, "sort": "title",
                          "descending": false},
                "max_sensitivity": "restricted", "offset": 0, "limit": 50, "expected_view": null
            }),
            Duration::from_secs(5),
        );
        let query: serde_json::Value =
            serde_json::from_slice(&read_response_frame(&mut query).unwrap()).unwrap();
        assert_eq!(query["ok"], true, "{query}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "library_query waited for the chat call"
        );
        let chat: serde_json::Value =
            serde_json::from_slice(&read_response_frame(&mut chat).unwrap()).unwrap();
        assert!(started.elapsed() >= Duration::from_secs(3));
        assert_eq!(chat["ok"], true, "{chat}");
        assert_eq!(chat["item_chat"]["request_id"], "concurrency-test-0001");
        assert_eq!(chat["item_chat"]["finish"], "stop");
        assert_eq!(chat["item_chat"]["model"], "Synthetic Sol");
        server.join().unwrap();
    }

    #[test]
    fn blocked_index_reports_loading_without_blocking_capabilities_and_recovers() {
        use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let item = synthetic_stored_item(&root, "english-index-loading");
        let lock = fs::OpenOptions::new().read(true).write(true)
            .open(root.join("catalog/.ingest.lock")).unwrap();
        lock.lock().unwrap();
        let socket = root.join("ipc/loading.sock");
        let listener = bind_owner_socket(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let server_stop = stop.clone();
        let server_root = root.clone();
        let server = std::thread::spawn(move || {
            let mut queue = Queue::open(&server_root.join("state/agent.db")).unwrap();
            let mut queries = BackgroundQueries::new(server_root.clone()).unwrap();
            let source: Arc<dyn crate::provider_generation::ProviderSource> =
                Arc::new(crate::provider_generation::tests::FakeProvider::default());
            let incoming = frames::Incoming::new();
            while !server_stop.load(Ordering::Relaxed) {
                if let Ok((stream, _)) = listener.accept() { incoming.admit(stream).unwrap(); }
                if let Some(mut prepared) = incoming.next() {
                    serve_prepared(&mut prepared.stream, &mut queue, &server_root, &mut queries,
                        &offline_network(), &source, prepared.command).unwrap();
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        let mut slow_upload = UnixStream::connect(&socket).unwrap();
        slow_upload.write_all(&(MAX_MARKDOWN_EDIT_FRAME as u32).to_be_bytes()).unwrap();
        slow_upload.write_all(b"{\"command\":\"markdown_edit\"").unwrap();
        let request = || serde_json::from_value::<Command>(serde_json::json!({
            "command":"library_query", "max_sensitivity":"restricted",
            "offset":0, "limit":10, "expected_view":null,
            "query":{"text":"", "sort":"title", "descending":false}
        })).unwrap();
        let mut pending = UnixStream::connect(&socket).unwrap();
        pending.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write_frame(&mut pending, &serde_json::to_vec(&request()).unwrap()).unwrap();
        let start = std::time::Instant::now();
        assert!(send_command(&socket, &Command::Capabilities).unwrap().capabilities.is_some());
        assert!(start.elapsed() < Duration::from_secs(1), "capabilities waited for index or partial upload");
        let busy = send_command(&socket, &request()).unwrap();
        assert_eq!(busy.reason_code.as_deref(), Some("library_loading"));
        let pending: Response = serde_json::from_slice(&read_response_frame(&mut pending).unwrap()).unwrap();
        assert_eq!(pending.reason_code.as_deref(), Some("library_loading"));
        lock.unlock().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let response = send_command(&socket, &request()).unwrap();
            if let Some(page) = response.library_query {
                assert_eq!(page.total, 1);
                break;
            }
            assert_eq!(response.reason_code.as_deref(), Some("library_loading"));
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(20));
        }
        // Reader admission also waits for Store access off the listener. Holding
        // the Store lock must not prevent capabilities or unrelated queue access.
        lock.lock().unwrap();
        let mut admission = UnixStream::connect(&socket).unwrap();
        admission.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write_frame(&mut admission, &serde_json::to_vec(&serde_json::json!({
            "command":"reader", "max_sensitivity":"public", "request": {
                "op":"align_request", "item_id":item, "revision_id":"revision_stale",
                "chapter":"chapter-1", "request_id":"blocked-admission", "expected_alignment_version":"missing"
            }
        })).unwrap()).unwrap();
        let start = std::time::Instant::now();
        assert!(send_command(&socket, &Command::Capabilities).unwrap().capabilities.is_some());
        assert!(start.elapsed() < Duration::from_secs(1));
        lock.unlock().unwrap();
        let rejected: Response = serde_json::from_slice(&read_response_frame(&mut admission).unwrap()).unwrap();
        assert_eq!(rejected.reason_code.as_deref(), Some("stale_item_revision"));
        stop.store(true, Ordering::Relaxed);
        server.join().unwrap();
    }

    #[test]
    fn insights_run_only_on_request_and_keep_only_verbatim_anchors() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let item = synthetic_stored_item(&root_path, "english-insights");
        let chinese = synthetic_stored_item(&root_path, "chinese-insights");
        let database = root_path.join("state/agent.db");
        let mut queue = Queue::open(&database).unwrap();
        let fake = crate::provider_generation::tests::FakeProvider::default();
        let mut provider = ProviderWorker::new(Box::new(fake.clone()));
        let mut reconciler = AnalysisReconciler::new(Duration::from_secs(3600));
        run_worker_until_idle(&root_path, &database, &mut provider, &mut reconciler);
        assert!(
            fake.calls.lock().unwrap().is_empty(),
            "no request, no model call"
        );
        let read = library_item(&mut queue, &root_path, &item);
        let insights = serde_json::to_value(read.ai_insights.unwrap()).unwrap();
        for kind in ["highlights", "quotes", "fact_check"] {
            assert_eq!(insights[kind]["state"], "not_requested");
            assert!(insights[kind]["items"].is_null());
        }
        assert_eq!(insights["fact_check"]["basis"], "model_knowledge");

        let all = [
            "highlights".to_owned(),
            "quotes".into(),
            "fact_check".into(),
        ];
        let request = |item: &str, kinds: &[String], routed: bool| {
            crate::insights::request(&root_path, item, Sensitivity::Restricted, kinds, |_| routed)
        };
        for bad in [
            vec![],
            vec!["summary".to_owned()],
            vec!["quotes".to_owned(), "quotes".into()],
        ] {
            assert_eq!(
                request(&item, &bad, true).unwrap_err().to_string(),
                "invalid_insight_request"
            );
        }
        assert_eq!(
            request(&item, &all, false).unwrap_err().to_string(),
            "no_insight_provider"
        );
        assert_eq!(
            request("item_missing", &all, true).unwrap_err().to_string(),
            "library_resource_not_found"
        );
        let queued = request(&item, &all, true).unwrap();
        assert_eq!(queued.highlights.state, "queued");
        assert_eq!(queued.fact_check.state, "queued");
        run_worker_until_idle(&root_path, &database, &mut provider, &mut reconciler);
        let calls = fake.calls.lock().unwrap().len();
        assert_eq!(calls, 3, "one request per kind for a short document");
        let (_, revision) = memex_store::read_item(&root_path, &item).unwrap();
        let blocks = crate::markdown_blocks::split_blocks(&revision.content_markdown);
        let done = request(&item, &all, true).unwrap();
        for view in [&done.highlights, &done.quotes, &done.fact_check] {
            assert_eq!(view.state, "succeeded");
            assert_eq!(
                view.dropped, 1,
                "the fabricated/out-of-range item is dropped"
            );
            let items = view.items.as_ref().unwrap().as_array().unwrap();
            assert_eq!(items.len(), 1);
            let index = items[0]["block_index"].as_u64().unwrap() as usize;
            let quote = items[0]["quote"].as_str().unwrap();
            assert!(blocks[index].text.contains(quote) && !blocks[index].passthrough);
        }
        assert_eq!(
            done.quotes.items.as_ref().unwrap()[0]["translation_zh"],
            "合成译文"
        );
        assert_eq!(done.highlights.items.as_ref().unwrap()[0]["style"], "bold");
        assert_eq!(done.fact_check.basis.as_deref(), Some("model_knowledge"));
        // Succeeded kinds are returned as-is: no further model call.
        run_worker_until_idle(&root_path, &database, &mut provider, &mut reconciler);
        assert_eq!(fake.calls.lock().unwrap().len(), calls);
        let read = library_item(&mut queue, &root_path, &item);
        assert_eq!(read.ai_insights.unwrap(), done);

        // A Chinese source works; its golden quotes carry no translation.
        request(&chinese, &["quotes".to_owned()], true).unwrap();
        run_worker_until_idle(&root_path, &database, &mut provider, &mut reconciler);
        let quotes = request(&chinese, &["quotes".to_owned()], true)
            .unwrap()
            .quotes;
        assert_eq!(quotes.state, "succeeded");
        assert!(quotes.items.unwrap()[0]["translation_zh"].is_null());
    }

    #[test]
    fn highlights_v2_never_serve_v1_rows_and_mark_the_translation_once_it_exists() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let item = synthetic_stored_item(&root_path, "english-highlights-v2");
        let database = root_path.join("state/agent.db");
        let mut queue = Queue::open(&database).unwrap();
        let fake = crate::provider_generation::tests::FakeProvider::default();
        let mut provider = ProviderWorker::new(Box::new(fake.clone()));
        let mut reconciler = AnalysisReconciler::new(Duration::from_secs(3600));
        let highlights = || {
            crate::insights::request(
                &root_path,
                &item,
                Sensitivity::Restricted,
                &["highlights".to_owned()],
                |_| true,
            )
            .unwrap()
            .highlights
        };
        // Inputs of the highlights requests so far (`calls` and `systems` are pushed together).
        let highlight_inputs = || -> Vec<String> {
            let systems = fake.systems.lock().unwrap();
            let calls = fake.calls.lock().unwrap();
            systems
                .iter()
                .zip(calls.iter())
                .filter(|(system, _)| system.starts_with(memex_analysis::HIGHLIGHTS_PROMPT))
                .map(|(_, input)| input.clone())
                .collect()
        };

        // No translation: original-side highlights only, and the state says so.
        assert_eq!(highlights().state, "queued");
        run_worker_until_idle(&root_path, &database, &mut provider, &mut reconciler);
        let done = highlights();
        assert_eq!(done.state, "succeeded");
        assert_eq!(done.sides.as_deref(), Some("original_only"));
        let items = done.items.as_ref().unwrap().as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert!(items[0]["translation"].is_null());
        assert_eq!(items[0]["original"]["text"], items[0]["quote"]);
        assert_eq!(items[0]["original"]["block"], items[0]["block_index"]);
        assert_eq!(highlight_inputs().len(), 1);
        assert!(!highlight_inputs()[0].contains("[[T"));

        // A stale v1 row of the same revision is never served as v2; a request re-runs it once.
        rusqlite::Connection::open(root_path.join("analysis/translation-jobs.sqlite3"))
            .unwrap()
            .execute(
                "UPDATE insight_jobs SET prompt_version='1' WHERE kind='highlights'",
                [],
            )
            .unwrap();
        let stale = library_item(&mut queue, &root_path, &item)
            .ai_insights
            .unwrap()
            .highlights;
        assert_eq!(stale.state, "not_requested");
        assert!(stale.items.is_none() && stale.sides.is_none());
        assert_eq!(highlights().state, "queued");
        run_worker_until_idle(&root_path, &database, &mut provider, &mut reconciler);
        assert_eq!(highlights().state, "succeeded");
        assert_eq!(highlight_inputs().len(), 2);
        // A current v2 result is served as it is.
        run_worker_until_idle(&root_path, &database, &mut provider, &mut reconciler);
        assert_eq!(highlights().state, "succeeded");
        assert_eq!(highlight_inputs().len(), 2);

        // Once a translation succeeded, a re-request marks it too: one call carrying [[T<n>]].
        request_all(&root_path, &item, false);
        run_worker_until_idle(&root_path, &database, &mut provider, &mut reconciler);
        assert!(matches!(
            library_item(&mut queue, &root_path, &item).translation_state,
            LibraryTranslationState::Succeeded
        ));
        assert_eq!(highlights().state, "queued");
        run_worker_until_idle(&root_path, &database, &mut provider, &mut reconciler);
        let bilingual = highlights();
        assert_eq!(bilingual.state, "succeeded");
        assert_eq!(bilingual.sides.as_deref(), Some("original_and_translation"));
        assert_eq!(highlight_inputs().len(), 3);
        assert!(
            highlight_inputs()[2].contains("[[B0]]") && highlight_inputs()[2].contains("[[T0]]")
        );
        assert_eq!(highlights(), bilingual);
        run_worker_until_idle(&root_path, &database, &mut provider, &mut reconciler);
        assert_eq!(highlight_inputs().len(), 3);
    }

    #[test]
    fn library_item_read_accepts_only_verified_table_repair_assemblies() {
        const TABLE: &str = "| Task description | Estimated cost |\n|---|---:|\n| Read the detailed report | $2.50 |";
        const ZH: &str = "| 任务说明文字 | 预估费用 |\n|---|---:|\n| 阅读这份详细的报告 | $2.50 |";
        struct TableModel;
        impl TranslationModel for TableModel {
            fn descriptor(&self) -> TranslationModelDescriptor { SyntheticTranslationModel.descriptor() }
            fn translate(&mut self, input: TranslationModelInput<'_>) -> std::result::Result<String, TranslationModelError> {
                if input.purpose() == GeneratedTextPurpose::TitleTranslation {
                    return Ok("合成表格的中文文章标题".into());
                }
                if input.source_text() == TABLE { return Ok(ZH.into()); }
                Ok(format!("{}\n\n{TABLE}", "已经翻译好的正文内容，不应重新翻译。".repeat(12)))
            }
        }
        let dir = tempfile::tempdir().unwrap(); let root = dir.path().canonicalize().unwrap();
        let item = synthetic_stored_item(&root, "english-table-repair");
        let (_, source) = memex_store::read_item(&root, &item).unwrap();
        let edited = memex_store::edit_markdown(&root, memex_store::MarkdownEdit {
            item_id: item.clone(), expected_revision_id: source.revision_id,
            request_id: "table-repair-fixture".into(), markdown: format!("{}\n\n{TABLE}", source.content_markdown),
        }, Sensitivity::Restricted).unwrap();
        memex_analysis::enqueue_item_translation(&root, &item, &edited.revision_id).unwrap();
        let mut translation = TranslationQueue::open(&root).unwrap();
        translation.process_next(&mut TableModel).unwrap();
        let pair = memex_analysis::validated_translation_pair(&root, &item, &edited.revision_id).unwrap().unwrap();
        let repaired = memex_analysis::repair_translation_tables(&root, &item, &pair.document_analysis_id, &mut TableModel).unwrap();
        assert_eq!(repaired.state, "succeeded");
        let mut queue = Queue::open(&root.join("state/agent.db")).unwrap();
        let read = library_item(&mut queue, &root, &item);
        assert!(matches!(read.translation_state, LibraryTranslationState::Succeeded));
        assert!(read.markdown.unwrap().contains(ZH));
        assert!(read.title.is_some());
    }

    fn request_all(root: &Path, item: &str, metadata: bool) {
        crate::translation_request::request(
            root,
            item,
            Sensitivity::Restricted,
            crate::translation_request::RequestRoutes {
                translation: true,
                minutes: true,
                metadata,
            },
        )
        .unwrap();
        crate::translation_request::minutes_request(root, item, Sensitivity::Restricted, true)
            .unwrap();
    }

    fn glossary_calls(fake: &crate::provider_generation::tests::FakeProvider) -> usize {
        fake.systems
            .lock()
            .unwrap()
            .iter()
            .filter(|system| system.contains("List at most 40 key terms"))
            .count()
    }

    /// Whole inputs of segment requests, in order.
    fn segment_inputs(fake: &crate::provider_generation::tests::FakeProvider) -> Vec<String> {
        let systems = fake.systems.lock().unwrap();
        let calls = fake.calls.lock().unwrap();
        systems
            .iter()
            .zip(calls.iter())
            .filter(|(system, _)| system.contains("JSON array of N Markdown blocks"))
            .map(|(_, call)| call.clone())
            .collect()
    }

    /// Segment requests of one document (title, glossary, minutes and metadata excluded).
    fn block_calls(fake: &crate::provider_generation::tests::FakeProvider) -> Vec<String> {
        let systems = fake.systems.lock().unwrap();
        let calls = fake.calls.lock().unwrap();
        systems
            .iter()
            .zip(calls.iter())
            .filter(|(system, _)| system.contains("JSON array of N Markdown blocks"))
            .map(|(_, call)| crate::provider_generation::tests::batch_part(call).to_owned())
            .collect()
    }

    #[test]
    fn translation_reads_follow_running_metadata_siblings_but_not_changed_bodies() {
        struct EditDuring<'a> { root: &'a Path, item: &'a str }
        impl TranslationModel for EditDuring<'_> {
            fn descriptor(&self) -> TranslationModelDescriptor { SyntheticTranslationModel.descriptor() }
            fn translate(&mut self, mut input: TranslationModelInput<'_>) -> std::result::Result<String, TranslationModelError> {
                if input.purpose() == GeneratedTextPurpose::DocumentTranslation {
                    assert!(input.progress().unwrap().heartbeat(2, 5));
                    let (_, source) = memex_store::read_item(self.root, self.item).unwrap();
                    let edited = memex_store::revise_item_metadata(self.root, self.item, memex_store::ItemMetadataUpdate {
                        expected_revision_id: source.revision_id, title: source.title,
                        manual_tags: vec!["metadata-only".into()],
                    }).unwrap();
                    memex_analysis::enqueue_item_translation(self.root, self.item, &edited.revision_id).unwrap();
                    let mut queue = Queue::open(&self.root.join("state/agent.db")).unwrap();
                    let read = library_item(&mut queue, self.root, self.item);
                    assert!(matches!(read.translation_state, LibraryTranslationState::Running));
                    assert_eq!(read.revision_id, edited.revision_id);
                    let progress = read.translation_progress.unwrap();
                    assert_eq!((progress.done_batches, progress.total_batches), (2, 5));
                    assert!(progress.started_at.is_some());
                    let scoped = dispatch(&mut queue, self.root, Command::ScopedTranslationStatus {
                        item_id: self.item.into(), max_sensitivity: Sensitivity::Restricted,
                    }).unwrap().translation.unwrap();
                    assert_eq!(scoped.state, "running");
                    assert_eq!(scoped.progress, Some(progress));
                    assert_eq!(scoped.revision_id, edited.revision_id);
                    let changed = memex_store::edit_markdown(self.root, memex_store::MarkdownEdit {
                        item_id: self.item.into(), expected_revision_id: edited.revision_id,
                        request_id: "translation-progress-body-change".into(),
                        markdown: format!("{}\n\nA new paragraph changes the source body.", source.content_markdown),
                    }, Sensitivity::Restricted).unwrap();
                    memex_analysis::enqueue_item_translation(self.root, self.item, &changed.revision_id).unwrap();
                    let read = library_item(&mut queue, self.root, self.item);
                    assert!(matches!(read.translation_state, LibraryTranslationState::NotRequested));
                    assert!(read.translation_progress.is_none());
                    let scoped = dispatch(&mut queue, self.root, Command::ScopedTranslationStatus {
                        item_id: self.item.into(), max_sensitivity: Sensitivity::Restricted,
                    }).unwrap().translation.unwrap();
                    assert_eq!(scoped.state, "not_requested", "a changed body requires a new explicit authorization");
                    assert!(scoped.progress.is_none());
                }
                SyntheticTranslationModel.translate(input)
            }
        }
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let item = synthetic_stored_item(&root, "english-progress-metadata");
        let (_, revision) = memex_store::read_item(&root, &item).unwrap();
        memex_analysis::enqueue_item_translation(&root, &item, &revision.revision_id).unwrap();
        let mut queue = TranslationQueue::open(&root).unwrap();
        assert!(queue.process_next(&mut EditDuring { root: &root, item: &item }).unwrap());
    }

    #[test]
    fn unknown_batch_keeps_finished_batches_and_an_explicit_retry_sends_only_the_rest() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let item = synthetic_stored_item(&root_path, "long-english-resume");
        let database = root_path.join("state/agent.db");
        let mut queue = Queue::open(&database).unwrap();
        let fake = crate::provider_generation::tests::FakeProvider::default();
        // Segment 2 of 6 (paragraphs 6–11) breaks mid-stream (unknown outcome).
        *fake.unknown_marker.lock().unwrap() = Some("Paragraph 08:".into());
        let mut provider = ProviderWorker::new(Box::new(fake.clone()));
        let mut reconciler = AnalysisReconciler::new(Duration::from_secs(3600));
        request_all(&root_path, &item, false);
        run_worker_until_idle(&root_path, &database, &mut provider, &mut reconciler);

        let first = block_calls(&fake);
        assert_eq!(
            first.len(),
            2,
            "segments run in order; the broken one is not retried, the rest never sent"
        );
        assert_eq!(glossary_calls(&fake), 1);
        let read = library_item(&mut queue, &root_path, &item);
        assert!(matches!(
            read.translation_state,
            LibraryTranslationState::Failed
        ));
        assert!(read.title.is_none(), "`title` stays success-only");
        assert_eq!(read.translated_title.as_deref(), Some("合成文章的中文标题"));
        let progress = read.translation_progress.unwrap();
        assert_eq!((progress.done_batches, progress.total_batches), (1, 6));
        let projection = memex_analysis::read_translation_projection(
            &root_path,
            &item,
            Sensitivity::Restricted,
            None,
        )
        .unwrap();
        assert_eq!(
            projection.reason.as_deref(),
            Some("provider_outcome_unknown")
        );
        assert_eq!(
            projection.translated_title.as_deref(),
            Some("合成文章的中文标题")
        );

        // An explicit re-request resumes: only the unfinished segments are sent, with the frozen
        // glossary and no new title or glossary call.
        *fake.unknown_marker.lock().unwrap() = None;
        let calls_before = fake.calls.lock().unwrap().len();
        request_all(&root_path, &item, false);
        let queued = library_item(&mut queue, &root_path, &item);
        assert_eq!(
            queued.translated_title.as_deref(),
            Some("合成文章的中文标题")
        );
        run_worker_until_idle(&root_path, &database, &mut provider, &mut reconciler);
        let all = block_calls(&fake);
        assert_eq!(all.len(), 7);
        assert!(all[2].contains("Paragraph 06:") && all[2].contains("Paragraph 11:"));
        assert!(all[6].contains("Paragraph 30:") && all[6].contains("Paragraph 35:"));
        assert_eq!(fake.calls.lock().unwrap().len(), calls_before + 5);
        assert_eq!(glossary_calls(&fake), 1);
        // The resumed second segment still continues from the stored first one.
        let inputs = segment_inputs(&fake);
        assert!(inputs[2].contains("<<<PREVIOUS TRANSLATION"));
        assert!(inputs[2].contains("合成文章"), "frozen glossary carried");
        let read = library_item(&mut queue, &root_path, &item);
        assert!(matches!(
            read.translation_state,
            LibraryTranslationState::Succeeded
        ));
        assert_eq!(read.translated_title.as_deref(), Some("合成文章的中文标题"));
        assert!(read.translation_progress.is_none());
        let markdown = read.markdown.unwrap();
        assert_eq!(crate::markdown_blocks::split_blocks(&markdown).len(), 36);
    }

    #[test]
    fn segments_run_one_at_a_time_per_document_and_parallel_workers_renew_leases() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let items = [
            synthetic_stored_item(&root_path, "long-english-a"),
            synthetic_stored_item(&root_path, "long-english-b"),
        ];
        let fake = crate::provider_generation::tests::FakeProvider::default();
        *fake.batch_delay.lock().unwrap() = Duration::from_millis(150);
        for item in &items {
            request_all(&root_path, item, false);
        }
        // Two provider workers, as the daemon runs them; each leases one job at a time.
        let workers: Vec<_> = (0..2)
            .map(|_| {
                let (fake, root) = (fake.clone(), root_path.clone());
                std::thread::spawn(move || {
                    let mut provider = ProviderWorker::new(Box::new(fake));
                    let mut idle = 0;
                    while idle < 20 {
                        if provider.step(&root, Duration::ZERO).unwrap() {
                            idle = 0;
                        } else {
                            idle += 1;
                            std::thread::sleep(Duration::from_millis(20));
                        }
                    }
                })
            })
            .collect();
        // While both documents are in flight, an expired-looking lease is renewed by the
        // running job's heartbeat before any worker could recover (and re-send) it.
        let analysis =
            rusqlite::Connection::open(root_path.join("analysis/translation-jobs.sqlite3"))
                .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let running: i64 = analysis
                .query_row(
                    "SELECT COUNT(*) FROM translation_jobs WHERE state='running'
                       AND translated_title IS NOT NULL",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            if running == 2 || std::time::Instant::now() > deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        analysis
            .execute(
                "UPDATE translation_jobs SET lease_until=0 WHERE state='running'",
                [],
            )
            .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let renewed: i64 = analysis
            .query_row(
                "SELECT COUNT(*) FROM translation_jobs WHERE state='running' AND lease_until>?1",
                [chrono::Utc::now().timestamp()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(renewed, 2, "heartbeat renewed both running leases");
        for worker in workers {
            worker.join().unwrap();
        }
        let (_, max_in_flight) = *fake.in_flight.lock().unwrap();
        assert_eq!(
            max_in_flight, 2,
            "two documents at once, one segment of each at a time"
        );
        let calls = block_calls(&fake);
        assert_eq!(
            calls.len(),
            12,
            "every segment of both documents sent exactly once"
        );
        let unique: std::collections::BTreeSet<_> =
            calls.iter().map(|call| call.as_str()).collect();
        assert_eq!(
            unique.len(),
            6,
            "identical documents have identical segments"
        );
        let mut queue = Queue::open(&root_path.join("state/agent.db")).unwrap();
        for item in &items {
            let read = library_item(&mut queue, &root_path, item);
            assert!(matches!(
                read.translation_state,
                LibraryTranslationState::Succeeded
            ));
        }

        // One document alone: segments are strictly sequential.
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let item = synthetic_stored_item(&root_path, "long-english-single");
        let fake = crate::provider_generation::tests::FakeProvider::default();
        *fake.batch_delay.lock().unwrap() = Duration::from_millis(50);
        request_all(&root_path, &item, false);
        let mut provider = ProviderWorker::new(Box::new(fake.clone()));
        while provider.step(&root_path, Duration::ZERO).unwrap() {}
        assert_eq!(fake.in_flight.lock().unwrap().1, 1);
        assert_eq!(block_calls(&fake).len(), 6);
    }

    #[test]
    fn minutes_suggest_a_title_that_an_explicit_apply_writes_without_losing_results() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let item = synthetic_stored_item(&root_path, "english-suggestion");
        let database = root_path.join("state/agent.db");
        let mut queue = Queue::open(&database).unwrap();
        let fake = crate::provider_generation::tests::FakeProvider::default();
        let mut provider = ProviderWorker::new(Box::new(fake.clone()));
        let mut reconciler = AnalysisReconciler::new(Duration::from_secs(3600));
        request_all(&root_path, &item, false);
        run_worker_until_idle(&root_path, &database, &mut provider, &mut reconciler);
        let before = library_item(&mut queue, &root_path, &item);
        let minutes = before.ai_minutes.clone().unwrap();
        assert_eq!(minutes.state, "succeeded");
        // One call yields both; the title line is not part of the minutes text.
        assert_eq!(minutes.text.as_deref(), Some(crate::provider_generation::tests::FAKE_MINUTES));
        let suggested = minutes.suggested_title.clone().unwrap();
        assert_eq!(suggested, crate::provider_generation::tests::FAKE_SUGGESTED_TITLE);
        assert!(matches!(before.translation_state, LibraryTranslationState::Succeeded));
        let calls = fake.calls.lock().unwrap().len();
        let apply = |queue: &mut Queue, expected: &str| {
            dispatch(
                queue,
                &root_path,
                serde_json::from_value(serde_json::json!({
                    "command": "item_title_apply", "item_id": item,
                    "expected_revision_id": expected, "title": suggested,
                    "request_id": "apply-suggested-title"
                }))
                .unwrap(),
            )
            .unwrap()
        };
        let applied = serde_json::to_value(apply(&mut queue, &before.revision_id)).unwrap();
        let (_, current) = memex_store::read_item(&root_path, &item).unwrap();
        assert_eq!(current.title, suggested);
        assert_ne!(current.revision_id, before.revision_id);
        assert!(applied.to_string().contains(&current.revision_id));
        // Replay with the old expected revision: idempotent, no new revision.
        apply(&mut queue, &before.revision_id);
        assert_eq!(memex_store::read_item(&root_path, &item).unwrap().1.revision_id, current.revision_id);
        run_worker_until_idle(&root_path, &database, &mut provider, &mut reconciler);
        let after = library_item(&mut queue, &root_path, &item);
        let kept = after.ai_minutes.unwrap();
        assert_eq!(kept.state, "succeeded");
        assert_eq!(kept.text, minutes.text);
        assert!(matches!(after.translation_state, LibraryTranslationState::Succeeded));
        // At most the title-only translation of the new title is sent; body work is inherited.
        assert!(fake.calls.lock().unwrap().len() <= calls + 1);
    }

    #[test]
    fn an_english_title_edit_translates_only_the_title_and_keeps_the_body_translation() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let item = synthetic_stored_item(&root_path, "english-retitle");
        let database = root_path.join("state/agent.db");
        let mut queue = Queue::open(&database).unwrap();
        let fake = crate::provider_generation::tests::FakeProvider::default();
        let mut provider = ProviderWorker::new(Box::new(fake.clone()));
        let mut reconciler = AnalysisReconciler::new(Duration::from_secs(3600));
        request_all(&root_path, &item, false);
        run_worker_until_idle(&root_path, &database, &mut provider, &mut reconciler);
        let before = library_item(&mut queue, &root_path, &item);
        assert!(matches!(before.translation_state, LibraryTranslationState::Succeeded));
        let calls = fake.calls.lock().unwrap().len();
        dispatch(
            &mut queue,
            &root_path,
            Command::UpdateItemMetadata {
                item_id: item.clone(),
                expected_revision_id: before.revision_id.clone(),
                title: "Episode 7 renamed by the owner".into(),
                manual_tags: vec![],
            },
        )
        .unwrap();
        run_worker_until_idle(&root_path, &database, &mut provider, &mut reconciler);
        let after = library_item(&mut queue, &root_path, &item);
        assert_ne!(after.revision_id, before.revision_id);
        assert!(matches!(after.translation_state, LibraryTranslationState::Succeeded));
        // The body translation is inherited unchanged; only the new title was translated.
        assert_eq!(after.markdown, before.markdown);
        assert_eq!(fake.calls.lock().unwrap().len(), calls + 1, "one title-only request");
        assert_ne!(after.translated_title, before.translated_title);
        assert!(after.translated_title.is_some());
        assert_eq!(after.ai_minutes.unwrap().text, before.ai_minutes.unwrap().text);
    }

    #[test]
    fn late_metadata_keeps_succeeded_minutes_translation_and_insights_on_the_new_revision() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let item = synthetic_stored_item(&root_path, "english-late-metadata");
        let database = root_path.join("state/agent.db");
        let mut queue = Queue::open(&database).unwrap();
        let fake = crate::provider_generation::tests::FakeProvider::default();
        let mut provider = ProviderWorker::new(Box::new(fake.clone()));
        let mut reconciler = AnalysisReconciler::new(Duration::from_secs(3600));
        // Translation and minutes succeed first (metadata not requested yet), plus highlights.
        request_all(&root_path, &item, false);
        crate::insights::request(
            &root_path,
            &item,
            Sensitivity::Restricted,
            &["highlights".to_owned()],
            |_| true,
        )
        .unwrap();
        run_worker_until_idle(&root_path, &database, &mut provider, &mut reconciler);
        let before = library_item(&mut queue, &root_path, &item);
        assert_eq!(before.ai_minutes.as_ref().unwrap().state, "succeeded");
        assert!(matches!(
            before.translation_state,
            LibraryTranslationState::Succeeded
        ));
        let calls = fake.calls.lock().unwrap().len();

        // A metadata job that was skipped by the previous rule is re-run by a re-request.
        let mut metadata = memex_analysis::MetadataQueue::open(&root_path).unwrap();
        metadata.request(&item, &before.revision_id).unwrap();
        drop(metadata);
        rusqlite::Connection::open(root_path.join("analysis/translation-jobs.sqlite3"))
            .unwrap()
            .execute(
                "UPDATE metadata_jobs SET state='skipped',reason_code='derived_text_present'",
                [],
            )
            .unwrap();
        request_all(&root_path, &item, true);
        assert_eq!(
            memex_analysis::metadata_status(&root_path, &item, &before.revision_id)
                .unwrap()
                .unwrap()
                .state,
            "queued"
        );
        run_worker_until_idle(&root_path, &database, &mut provider, &mut reconciler);
        assert_eq!(
            fake.calls.lock().unwrap().len(),
            calls + 1,
            "only the metadata request; carried outputs are applied without a model call"
        );
        let (_, current) = memex_store::read_item(&root_path, &item).unwrap();
        assert_ne!(current.revision_id, before.revision_id);
        assert_eq!(
            current.metadata.keywords.unwrap().value,
            ["synthetic", "translation", "test"]
        );
        let after = library_item(&mut queue, &root_path, &item);
        assert_eq!(after.revision_id, current.revision_id);
        let minutes = after.ai_minutes.unwrap();
        assert_eq!(minutes.state, "succeeded");
        assert_eq!(minutes.text, before.ai_minutes.unwrap().text);
        assert!(matches!(
            after.translation_state,
            LibraryTranslationState::Succeeded
        ));
        assert_eq!(after.title, before.title);
        assert_eq!(after.markdown, before.markdown);
        let highlights = after.ai_insights.unwrap().highlights;
        assert_eq!(highlights.state, "succeeded");
        assert_eq!(
            highlights.items,
            before.ai_insights.unwrap().highlights.items
        );
    }

    fn article(words: &str, index: usize) -> String {
        (0..40)
            .map(|line| format!("{words} sentence {line} of synthetic article {index}."))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn dedup_plan_with(
        root: &Path,
        fake: &crate::provider_generation::tests::FakeProvider,
        use_model: bool,
    ) -> crate::dedup::DedupPlan {
        crate::dedup::plan(
            root,
            &root.join("state/agent.db"),
            fake,
            Sensitivity::Restricted,
            use_model,
        )
        .unwrap()
    }

    fn query_total(queue: &mut Queue, root: &Path) -> (u64, serde_json::Value) {
        let request: memex_library::LibraryQueryRequest = serde_json::from_value(serde_json::json!({
            "query": {"text":"", "source":null, "item_type":null, "sort":"title", "descending":false},
            "max_sensitivity":"restricted", "offset":0, "limit":50, "expected_view":null
        }))
        .unwrap();
        let page = serde_json::to_value(
            dispatch(queue, root, Command::LibraryQuery(request))
                .unwrap()
                .library_query
                .unwrap(),
        )
        .unwrap();
        (page["total"].as_u64().unwrap(), page["sources"].clone())
    }

    #[test]
    fn exact_duplicates_need_no_model_and_ambiguous_pairs_follow_the_model() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let body = article("Identical", 1);
        let with = |suffix: &str, url: &str, title: &str, markdown: &str| {
            stored_item_with(
                &root_path,
                suffix,
                Sensitivity::Public,
                Some((url, title, markdown)),
            )
        };
        // Same URL modulo tracking/scheme/www/slash/fragment, same text: deterministic.
        let a = with(
            "dup-a",
            "https://www.example.invalid/post/?utm_source=x#c",
            "Post",
            &body,
        );
        let b = with("dup-b", "http://example.invalid/post", "Post", &body);
        // Same normalized URL, materially different text: ambiguous (revision?).
        let c = with(
            "rev-c",
            "https://example.invalid/revised",
            "Revised",
            &article("First", 2),
        );
        let d = with(
            "rev-d",
            "https://example.invalid/revised/",
            "Revised",
            &article("Second", 3),
        );
        // Unrelated.
        with(
            "other",
            "https://example.invalid/other",
            "Other",
            &article("Other", 4),
        );

        let fake = crate::provider_generation::tests::FakeProvider::default();
        let plan = dedup_plan_with(&root_path, &fake, false);
        assert_eq!(plan.model_calls, 0);
        assert!(
            fake.calls.lock().unwrap().is_empty(),
            "use_model:false never calls"
        );
        assert_eq!(plan.groups.len(), 1);
        assert_eq!(plan.groups[0].basis, crate::dedup::Basis::SameSourceUrl);
        assert_eq!(plan.groups[0].reason, "identical_url_and_content");
        let members: std::collections::BTreeSet<_> = plan.groups[0]
            .members
            .iter()
            .map(|member| member.item_id.clone())
            .collect();
        assert_eq!(
            members,
            std::collections::BTreeSet::from([a.clone(), b.clone()])
        );
        assert_eq!(plan.ambiguous_unjudged, 1);

        // With the model: the exact pair still needs no call; the ambiguous pair is asked once.
        let plan = dedup_plan_with(&root_path, &fake, true);
        assert_eq!(plan.model_calls, 1);
        assert_eq!(fake.calls.lock().unwrap().len(), 1);
        let call = fake.calls.lock().unwrap()[0].clone();
        assert!(call.contains("Document A (older)") && call.contains("example.invalid/revised"));
        assert_eq!(
            plan.groups.len(),
            1,
            "a revision without strict subset is kept apart"
        );

        *fake.dedup_reply.lock().unwrap() = Some(
            r#"{"verdict":"duplicate","older_is_strict_subset":false,"confidence":"medium"}"#
                .into(),
        );
        let plan = dedup_plan_with(&root_path, &fake, true);
        assert_eq!(plan.groups.len(), 2);
        let judged = plan
            .groups
            .iter()
            .find(|group| group.basis == crate::dedup::Basis::ModelJudged)
            .unwrap();
        assert_eq!(judged.confidence, "medium");
        let members: std::collections::BTreeSet<_> = judged
            .members
            .iter()
            .map(|member| member.item_id.clone())
            .collect();
        assert_eq!(members, std::collections::BTreeSet::from([c, d]));

        // Unparseable model output is never a duplicate.
        *fake.dedup_reply.lock().unwrap() = Some("yes, same".into());
        assert_eq!(dedup_plan_with(&root_path, &fake, true).groups.len(), 1);

        // No classification route: refused before any scan or call.
        fake.missing
            .lock()
            .unwrap()
            .push(memex_models::Purpose::Classification);
        let calls = fake.calls.lock().unwrap().len();
        let error = crate::dedup::plan(
            &root_path,
            &root_path.join("state/agent.db"),
            &fake,
            Sensitivity::Restricted,
            true,
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "no_dedup_provider");
        assert_eq!(fake.calls.lock().unwrap().len(), calls);
    }

    #[test]
    fn apply_merges_tags_and_groups_trashes_recoverably_and_skips_stale_groups() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let database = root_path.join("state/agent.db");
        let mut queue = Queue::open(&database).unwrap();
        let with = |suffix: &str, url: &str, markdown: &str| {
            stored_item_with(
                &root_path,
                suffix,
                Sensitivity::Public,
                Some((url, "Same post", markdown)),
            )
        };
        let body = article("Shared", 1);
        let keep = with("keep", "https://example.invalid/shared", &(body.clone()));
        let dupe = with(
            "dupe",
            "https://example.invalid/shared?utm_campaign=z",
            &body,
        );
        let other_body = article("Second shared", 2);
        let stale_a = with("stale-a", "https://example.invalid/second", &other_body);
        let stale_b = with("stale-b", "https://example.invalid/second/", &other_body);
        with(
            "single",
            "https://example.invalid/single",
            &article("Single", 3),
        );
        // The duplicate carries a manual tag and a group membership; the keep rule picks it.
        let (_, revision) = memex_store::read_item(&root_path, &dupe).unwrap();
        memex_store::revise_item_metadata(
            &root_path,
            &dupe,
            memex_store::ItemMetadataUpdate {
                expected_revision_id: revision.revision_id,
                title: "Same post".into(),
                manual_tags: vec!["from-dupe".into()],
            },
        )
        .unwrap();
        let group = memex_store::edit_groups(
            &root_path,
            "dedup-test-root",
            memex_store::GroupEditOp::CreateRoot {
                name: "Reading".into(),
                sensitivity: Sensitivity::Public,
            },
            Sensitivity::Restricted,
        )
        .unwrap();
        memex_store::edit_groups(
            &root_path,
            "dedup-test-member",
            memex_store::GroupEditOp::AddMembers {
                group_id: group.group_id.clone(),
                expected_version: group.version.clone(),
                item_ids: vec![keep.clone()],
            },
            Sensitivity::Restricted,
        )
        .unwrap();
        let (total_before, _) = query_total(&mut queue, &root_path);
        assert_eq!(total_before, 5);

        let fake = crate::provider_generation::tests::FakeProvider::default();
        let plan = dedup_plan_with(&root_path, &fake, false);
        assert_eq!(plan.groups.len(), 2);
        let shared = plan
            .groups
            .iter()
            .find(|group| group.members.iter().any(|member| member.item_id == dupe))
            .unwrap()
            .clone();
        // Both have user edits (tag vs group membership); equal content; the id breaks the tie.
        let (kept, trashed) = (
            shared.keep_item_id.clone(),
            shared.trash_item_ids[0].clone(),
        );
        assert_eq!(
            std::collections::BTreeSet::from([kept.clone(), trashed.clone()]),
            std::collections::BTreeSet::from([keep.clone(), dupe.clone()])
        );
        // A concurrent edit of the second group after planning: it must be skipped.
        let (_, revision) = memex_store::read_item(&root_path, &stale_b).unwrap();
        memex_store::revise_item_metadata(
            &root_path,
            &stale_b,
            memex_store::ItemMetadataUpdate {
                expected_revision_id: revision.revision_id,
                title: "Edited meanwhile".into(),
                manual_tags: vec![],
            },
        )
        .unwrap();

        let wrong = dispatch(
            &mut queue,
            &root_path,
            Command::DedupApply {
                plan_id: plan.plan_id.clone(),
                expected_group_count: 3,
            },
        )
        .unwrap();
        assert_eq!(
            wrong.reason_code.as_deref(),
            Some("dedup_group_count_mismatch")
        );
        let applied = dispatch(
            &mut queue,
            &root_path,
            Command::DedupApply {
                plan_id: plan.plan_id.clone(),
                expected_group_count: 2,
            },
        )
        .unwrap()
        .dedup_apply
        .unwrap();
        assert_eq!((applied.applied_groups, applied.skipped_groups), (1, 1));
        assert_eq!(applied.trashed_items, 1);
        let skipped = applied
            .groups
            .iter()
            .find(|g| g.state == "skipped")
            .unwrap();
        assert_eq!(skipped.reason.as_deref(), Some("stale_item_revision"));
        for item in [&stale_a, &stale_b] {
            assert!(!memex_store::is_item_trashed(&root_path, item).unwrap());
        }
        // Kept: union of manual tags and of group memberships.
        let (_, kept_revision) = memex_store::read_item(&root_path, &kept).unwrap();
        assert_eq!(
            kept_revision.metadata.manual_tags.unwrap().value,
            ["from-dupe"]
        );
        let memberships = memex_store::with_consistent_snapshot(&root_path, |root| {
            memex_store::read_visible_groups(root, Sensitivity::Restricted)
        })
        .unwrap()
        .memberships;
        assert!(memberships
            .iter()
            .any(|m| m.group_id == group.group_id && m.item_id == kept));
        // Replay by plan id: the same report, nothing re-applied.
        let replay = dispatch(
            &mut queue,
            &root_path,
            Command::DedupApply {
                plan_id: plan.plan_id.clone(),
                expected_group_count: 2,
            },
        )
        .unwrap()
        .dedup_apply
        .unwrap();
        assert_eq!(replay, applied);

        // Trashed: excluded from list, query totals and source facets, reads are not found.
        let (total, sources) = query_total(&mut queue, &root_path);
        assert_eq!(total, 4);
        assert_eq!(sources[0]["count"], 4);
        let listed = dispatch(
            &mut queue,
            &root_path,
            Command::LibraryList { cursor: None },
        )
        .unwrap()
        .library_items
        .unwrap();
        assert!(listed.iter().all(|item| item.item_id != trashed));
        let read = dispatch(
            &mut queue,
            &root_path,
            Command::LibraryItemRead {
                item_id: trashed.clone(),
            },
        )
        .unwrap_err();
        assert_eq!(read.to_string(), "library_resource_not_found");
        let raw = dispatch(
            &mut queue,
            &root_path,
            Command::ItemRead {
                item_id: trashed.clone(),
            },
        )
        .unwrap_err();
        assert_eq!(raw.to_string(), "item_not_found");
        let trash = dispatch(&mut queue, &root_path, Command::TrashList { cursor: None }).unwrap();
        let entries = trash.trash_items.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].item_id, trashed);
        assert_eq!(entries[0].kept_item_id.as_deref(), Some(kept.as_str()));
        assert_eq!(entries[0].plan_id.as_deref(), Some(plan.plan_id.as_str()));
        assert_eq!(entries[0].reason, "duplicate");

        // Restore round-trip: visible again with its unchanged revision; files never left.
        let restored = dispatch(
            &mut queue,
            &root_path,
            Command::ItemTrashRestore {
                item_id: trashed.clone(),
            },
        )
        .unwrap()
        .trash_restore
        .unwrap();
        assert!(restored.restored);
        assert_eq!(query_total(&mut queue, &root_path).0, 5);
        assert!(library_item(&mut queue, &root_path, &trashed).revision_id == restored.revision_id);
        let again = dispatch(
            &mut queue,
            &root_path,
            Command::ItemTrashRestore {
                item_id: trashed.clone(),
            },
        )
        .unwrap_err();
        assert_eq!(again.to_string(), "item_not_trashed");
        assert!(
            dispatch(&mut queue, &root_path, Command::TrashList { cursor: None })
                .unwrap()
                .trash_items
                .unwrap()
                .is_empty()
        );
    }

    fn metadata_calls(fake: &crate::provider_generation::tests::FakeProvider) -> Vec<String> {
        let systems = fake.systems.lock().unwrap();
        let calls = fake.calls.lock().unwrap();
        systems
            .iter()
            .zip(calls.iter())
            .filter(|(system, _)| system.as_str() == memex_analysis::METADATA_PROMPT)
            .map(|(_, call)| call.clone())
            .collect()
    }

    #[test]
    fn drifted_keywords_are_dropped_retried_once_and_old_model_values_are_replaced() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let item = synthetic_stored_item(&root_path, "english-keyword-drift");
        let database = root_path.join("state/agent.db");
        let fake = crate::provider_generation::tests::FakeProvider::default();
        let mut provider = ProviderWorker::new(Box::new(fake.clone()));
        let mut reconciler = AnalysisReconciler::new(Duration::from_secs(3600));
        let spanish = r#"{"keywords":["artículo inofensivo","prueba de traducción","datos sintéticos"],"creators":[],"published_date":null}"#;
        // A version-1 model value in the wrong language is already on the item (live state),
        // beside a version-1 job row that succeeded on the current revision.
        let (_, base) = memex_store::read_item(&root_path, &item).unwrap();
        let old = memex_store::apply_model_metadata_suggestion(
            &root_path,
            &item,
            &base.revision_id,
            memex_store::ModelMetadataSuggestion {
                keywords: Some(vec!["agentes de IA".into(), "cultura de equipo".into()]),
                creators: None,
                published_date: None,
            },
            memex_core::MetadataProvenance::new(
                memex_core::MetadataOrigin::ModelSuggested,
                "memex-provider-chat:synthetic",
                "2:settings-1:credential-1:prompt-1",
            ),
        )
        .unwrap()
        .unwrap();
        let mut metadata = memex_analysis::MetadataQueue::open(&root_path).unwrap();
        metadata.request(&item, &old.revision_id).unwrap();
        drop(metadata);
        rusqlite::Connection::open(root_path.join("analysis/translation-jobs.sqlite3"))
            .unwrap()
            .execute(
                "UPDATE metadata_jobs SET state='succeeded',prompt_version='1'",
                [],
            )
            .unwrap();
        // First reply drifts into Spanish (all dropped) → one retry with the reminder.
        fake.metadata_replies
            .lock()
            .unwrap()
            .extend([spanish.to_owned(), r#"{"keywords":["harmless article","translation test","artículo"],"creators":[],"published_date":null}"#.to_owned()]);
        request_all(&root_path, &item, true);
        run_worker_until_idle(&root_path, &database, &mut provider, &mut reconciler);
        let calls = metadata_calls(&fake);
        assert_eq!(calls.len(), 2, "one request plus one in-attempt retry");
        assert!(!calls[0].contains(memex_analysis::METADATA_RETRY_REMINDER));
        assert!(calls[1].contains(memex_analysis::METADATA_RETRY_REMINDER));
        assert!(
            !calls[0].contains("agentes de IA"),
            "earlier model values are not offered as existing metadata"
        );
        let status = memex_analysis::metadata_status(&root_path, &item, &old.revision_id)
            .unwrap()
            .unwrap();
        assert_eq!(status.state, "succeeded");
        assert_eq!(status.dropped_keywords, 1);
        let (_, current) = memex_store::read_item(&root_path, &item).unwrap();
        let keywords = current.metadata.keywords.unwrap();
        assert_eq!(keywords.value, ["harmless article", "translation test"]);
        assert!(keywords.provenance.provider_version.ends_with(":prompt-2"));
        // The version-2 revision is not enriched again by the next request (no model call).
        request_all(&root_path, &item, true);
        run_worker_until_idle(&root_path, &database, &mut provider, &mut reconciler);
        assert_eq!(metadata_calls(&fake).len(), 2);
        assert_eq!(
            memex_analysis::metadata_status(&root_path, &item, &current.revision_id)
                .unwrap()
                .unwrap()
                .reason_code
                .as_deref(),
            Some("metadata_current")
        );

        // Never replaced: an intake-provided value; nothing grounded after the retry writes
        // nothing (and fails nothing).
        let intake = synthetic_stored_item(&root_path, "english-intake-keywords");
        fake.metadata_replies
            .lock()
            .unwrap()
            .extend([spanish.to_owned(), spanish.to_owned()]);
        request_all(&root_path, &intake, true);
        run_worker_until_idle(&root_path, &database, &mut provider, &mut reconciler);
        assert_eq!(metadata_calls(&fake).len(), 4);
        let (_, revision) = memex_store::read_item(&root_path, &intake).unwrap();
        let keywords = revision.metadata.keywords.unwrap();
        assert_eq!(keywords.value, ["harmless article"]);
        assert_eq!(
            keywords.provenance.origin,
            memex_core::MetadataOrigin::IntakeProvided
        );
        let status = memex_analysis::metadata_status(&root_path, &intake, &revision.revision_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            (status.state.as_str(), status.dropped_keywords),
            ("succeeded", 3)
        );
        // A model suggestion may not overwrite the intake value even when asked directly.
        assert!(memex_store::apply_model_metadata_suggestion(
            &root_path,
            &intake,
            &revision.revision_id,
            memex_store::ModelMetadataSuggestion {
                keywords: Some(vec!["translation test".into()]),
                creators: None,
                published_date: None,
            },
            memex_core::MetadataProvenance::new(
                memex_core::MetadataOrigin::ModelSuggested,
                "memex-provider-chat:synthetic",
                "2:settings-1:credential-1:prompt-2",
            ),
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn a_metadata_revision_mid_flight_keeps_the_job_and_its_batches_are_not_sent_twice() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let item = synthetic_stored_item(&root_path, "long-english-sup");
        let database = root_path.join("state/agent.db");
        let fake = crate::provider_generation::tests::FakeProvider::default();
        *fake.batch_delay.lock().unwrap() = Duration::from_millis(150);
        request_all(&root_path, &item, false);
        let (_, first) = memex_store::read_item(&root_path, &item).unwrap();
        let workers: Vec<_> = (0..2)
            .map(|_| {
                let (fake, root) = (fake.clone(), root_path.clone());
                std::thread::spawn(move || {
                    let mut provider = ProviderWorker::new(Box::new(fake));
                    let mut idle = 0;
                    while idle < 30 {
                        if provider.step(&root, Duration::ZERO).unwrap() {
                            idle = 0;
                        } else {
                            idle += 1;
                            std::thread::sleep(Duration::from_millis(20));
                        }
                    }
                })
            })
            .collect();
        // Once the first batches are in flight, a metadata-only revision replaces the
        // revision (as the metadata job does) and carries the work to it.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while block_calls(&fake).is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        let receipt = memex_store::apply_model_metadata_suggestion(
            &root_path,
            &item,
            &first.revision_id,
            memex_store::ModelMetadataSuggestion {
                keywords: Some(vec!["synthetic article".into()]),
                creators: None,
                published_date: None,
            },
            memex_core::MetadataProvenance::new(
                memex_core::MetadataOrigin::ModelSuggested,
                "memex-provider-chat:synthetic",
                "2:settings-1:credential-1:prompt-2",
            ),
        )
        .unwrap()
        .unwrap();
        crate::provider_generation::carry_metadata_only_revision(
            &root_path,
            &item,
            &first.revision_id,
            &receipt.revision_id,
        )
        .unwrap();
        for worker in workers {
            worker.join().unwrap();
        }
        // A metadata-only revision (same body) neither stops nor supersedes the running job:
        // it completes on its own revision and the new revision inherits the result.
        let old = memex_analysis::translation_status(&root_path, &item, &first.revision_id)
            .unwrap()
            .unwrap();
        assert_eq!(old.state, TranslationJobState::Succeeded);
        let mut queue = Queue::open(&database).unwrap();
        let read = library_item(&mut queue, &root_path, &item);
        assert_eq!(read.revision_id, receipt.revision_id);
        assert!(matches!(
            read.translation_state,
            LibraryTranslationState::Succeeded
        ));
        // Every segment was sent exactly once across both revisions' jobs (stored segments and
        // the glossary are reused by content), and the title once.
        let blocks = block_calls(&fake);
        assert_eq!(blocks.len(), 6, "stored segments reused by content");
        let unique: std::collections::BTreeSet<_> = blocks.iter().collect();
        assert_eq!(unique.len(), 6);
        assert_eq!(glossary_calls(&fake), 1);
        let titles = fake
            .systems
            .lock()
            .unwrap()
            .iter()
            .filter(|system| system.contains("one source title"))
            .count();
        assert_eq!(titles, 1);
    }
}
