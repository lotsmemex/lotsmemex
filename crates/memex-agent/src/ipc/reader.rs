//! Owner-socket long-document API; no model dispatch or external I/O.
use super::*;
use memex_store::reader as store;

#[cfg(test)]
mod extensions;

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum BatchPreviewRequest {
    Selection { item_id: String, revision_id: String },
    Status { item_id: String, batch_id: String },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum FormatPreviewRequest {
    /// `auto_request_id` (additive): one step — the batch is admitted as soon as the preview
    /// is ready; `format_preview` status then carries `batch_id`.
    Selection { selection: crate::queue::content_format::Selection,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auto_request_id: Option<String> },
    Status { preview_id: String },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "view", rename_all = "snake_case", deny_unknown_fields)]
pub enum FormatChangesRequest {
    List { batch_id: String, target_id: String, expected_version: String, offset: usize, limit: usize },
    Text { batch_id: String, target_id: String, expected_version: String, change_id: String,
        part: String, start_byte: usize, limit_bytes: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnotationSpace {
    AiTranslation,
}

/// Each ledger's strict change shape; which one is accepted depends on the space.
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AnnotationChange {
    Edition(store::annotations::Change),
    Ai(store::annotations_ai::Change),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReaderRequest {
    AlignBatchPreview(BatchPreviewRequest),
    AlignBatchStart { item_id: String, batch_id: String, request_id: String,
        accept_cost: bool, accept_model_processing: bool },
    AlignBatchStatus { item_id: String, batch_id: String, offset: usize, limit: usize },
    AlignBatchStop { item_id: String, batch_id: String },
    TranslateMapPreview(crate::queue::reader_translation_map::Selection),
    TranslateMapRequest(crate::queue::reader_translation_map::Request),
    TranslateMapStatus { item_id: String, job_id: String },
    TranslateMapStop { item_id: String, job_id: String },
    TranslateMapRead { item_id: String, revision_id: String, chapter: String,
        translation_version: String, expected_version: String, offset: usize, limit: usize },
    FormatPreview(FormatPreviewRequest),
    FormatTargets { preview_id: String, offset: usize, limit: usize },
    FormatRequest { quote_id: String, request_id: String, #[serde(default)] accept_cost: bool, accept_model_processing: bool },
    FormatStatus { batch_id: String },
    FormatResults { batch_id: String, offset: usize, limit: usize },
    FormatChanges(FormatChangesRequest),
    FormatRestore { batch_id: String, target_id: String, expected_revision_id: String, request_id: String },
    FormatResume { batch_id: String },
    RevisionChunk { item_id: String, revision_id: String, target: store::revision_text::Target,
        start_byte: usize, limit_bytes: usize, expected_sha256: String },
    TranslatePreview { item_id: String, revision_id: String, chapter: String },
    /// `quote_id`/`accept_cost` may be omitted: one step (the server issues the quote).
    TranslateRequest { item_id: String, revision_id: String, chapter: String,
        request_id: String, #[serde(default)] quote_id: String, #[serde(default)] accept_cost: bool,
        expected_translation_version: String },
    TranslateStatus { item_id: String, job_id: String },
    TranslateChunk { item_id: String, revision_id: String, chapter: String,
        expected_version: String, offset: usize, limit: usize },
    TranslateMetadata { item_id: String, revision_id: String, chapter: String },
    TranslateAlignment { item_id: String, revision_id: String, chapter: String,
        expected_version: String, offset: usize, limit: usize },
    EditBegin {
        item_id: String, revision_id: String, side: String, chapter: String,
        expected_chapter_sha256: String, request_id: String, bytes: usize, sha256: String,
    },
    EditChunk { item_id: String, edit_id: String, offset: usize, markdown: String },
    EditCommit { item_id: String, edit_id: String },
    AlignRequest {
        item_id: String,
        revision_id: String,
        chapter: String,
        request_id: String,
        expected_alignment_version: String,
    },
    AlignStatus { item_id: String, job_id: String },
    Toc {
        item_id: String,
        revision_id: String,
    },
    Alignment {
        item_id: String,
        revision_id: String,
        chapter: String,
        offset: usize,
        limit: usize,
        expected_version: String,
    },
    Chunk {
        item_id: String,
        revision_id: String,
        side: String,
        chapter: String,
        offset: usize,
        limit: usize,
    },
    State {
        item_id: String,
    },
    Save {
        item_id: String,
        expected_version: String,
        state: store::ReaderState,
    },
    Resume,
    /// Without `annotation_space` this is exactly the edition-ledger v1 wire; the AI
    /// filter and view version exist only in the `ai_translation` space.
    AnnotationsList { item_id: String, offset: usize, limit: usize, expected_version: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        annotation_space: Option<AnnotationSpace>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        projection: Option<store::annotations_ai::ProjectionFilter>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_view_version: Option<String> },
    AnnotationsEdit { item_id: String, expected_version: String, change: AnnotationChange,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        annotation_space: Option<AnnotationSpace> },
    PassesList { item_id: String, offset: usize, limit: usize, expected_version: Option<String> },
    PassesEdit { item_id: String, expected_version: String, change: store::passes::Change },
    PassesStats { item_id: String, pass_id: String, offset: usize, limit: usize, expected_timing_version: Option<String> },
    PassesTimingStart { item_id: String, pass_id: String },
    PassesTimingReport { item_id: String, pass_id: String, session_id: String, segments: Vec<store::passes::SegmentReport> },
}

/// Server clock for reading-pass timing sessions: a per-process boot identity and a
/// monotonic origin. A daemon restart is a new boot and invalidates live sessions.
fn timing_clock() -> store::passes::TimingClock {
    static BOOT: std::sync::OnceLock<(String, std::time::Instant)> = std::sync::OnceLock::new();
    let (boot_id, origin) = BOOT.get_or_init(|| (format!("boot_{}", uuid::Uuid::now_v7().simple()), std::time::Instant::now()));
    store::passes::TimingClock {
        boot_id: boot_id.clone(),
        utc_ms: chrono::Utc::now().timestamp_millis(),
        mono_ms: origin.elapsed().as_millis() as i64,
    }
}

pub(super) fn dispatch(
    library: &Path,
    request: ReaderRequest,
    ceiling: Sensitivity,
    principal: Option<String>,
) -> Result<Response> {
    let principal = principal.as_deref().unwrap_or("local_owner");
    let value = match request {
        // Parsing is shared with the synthetic host. No trusted production
        // authority channel exists yet, even for requests arriving over UDS.
        ReaderRequest::AlignBatchPreview(_) | ReaderRequest::AlignBatchStart { .. }
            | ReaderRequest::AlignBatchStatus { .. } | ReaderRequest::AlignBatchStop { .. }
            | ReaderRequest::TranslateMapPreview(_) | ReaderRequest::TranslateMapRequest(_)
            | ReaderRequest::TranslateMapStatus { .. } | ReaderRequest::TranslateMapStop { .. }
            | ReaderRequest::TranslateMapRead { .. } => anyhow::bail!("permission_denied"),
        ReaderRequest::FormatPreview(_) | ReaderRequest::FormatTargets { .. }
            | ReaderRequest::FormatRequest { .. } | ReaderRequest::FormatStatus { .. }
            | ReaderRequest::FormatResults { .. } | ReaderRequest::FormatChanges(_)
            | ReaderRequest::FormatRestore { .. } | ReaderRequest::FormatResume { .. } =>
            anyhow::bail!("invalid_content_format_request"),
        ReaderRequest::RevisionChunk { item_id, revision_id, target, start_byte, limit_bytes, expected_sha256 } =>
            serde_json::to_value(store::revision_text::chunk(library, &item_id, &revision_id,
                target, start_byte, limit_bytes, &expected_sha256, ceiling)?)?,
        ReaderRequest::TranslateChunk { item_id, revision_id, chapter, expected_version, offset, limit } =>
            serde_json::to_value(store::chapter_translation::chunk(library, &item_id, &revision_id,
                &chapter, &expected_version, offset, limit, ceiling)?)?,
        ReaderRequest::TranslateMetadata { item_id, revision_id, chapter } => {
            let translation = store::chapter_translation::metadata(library, &item_id, &revision_id, &chapter, ceiling)?;
            serde_json::json!({"item_id": item_id, "revision_id": revision_id, "chapter": chapter, "translation": translation})
        }
        ReaderRequest::TranslateAlignment { item_id, revision_id, chapter, expected_version, offset, limit } =>
            serde_json::to_value(store::chapter_translation::alignment(library, &item_id, &revision_id,
                &chapter, &expected_version, offset, limit, ceiling)?)?,
        ReaderRequest::TranslatePreview { .. } | ReaderRequest::TranslateRequest { .. }
            | ReaderRequest::TranslateStatus { .. } => anyhow::bail!("invalid_reader_translation_request"),
        ReaderRequest::EditBegin { item_id, revision_id, side, chapter, expected_chapter_sha256,
            request_id, bytes, sha256 } => serde_json::to_value(store::chapter_edit::begin(library,
                store::chapter_edit::Begin { item_id, revision_id, side, chapter, expected_chapter_sha256,
                    request_id, bytes, sha256 }, principal, ceiling)?)?,
        ReaderRequest::EditChunk { item_id, edit_id, offset, markdown } => {
            let received_bytes = store::chapter_edit::chunk(library, &item_id, &edit_id, offset, &markdown, principal, ceiling)?;
            serde_json::json!({"edit_id": edit_id, "received_bytes": received_bytes})
        }
        ReaderRequest::EditCommit { .. } => anyhow::bail!("invalid_reader_edit"),
        ReaderRequest::AlignRequest { .. } | ReaderRequest::AlignStatus { .. } => anyhow::bail!("invalid_reader_alignment_request"),
        ReaderRequest::Alignment {
            item_id,
            revision_id,
            chapter,
            offset,
            limit,
            expected_version,
        } => serde_json::to_value(store::paragraphs::read(
            library,
            &item_id,
            &revision_id,
            &chapter,
            offset,
            limit,
            &expected_version,
            ceiling,
        )?)?,
        ReaderRequest::Toc {
            item_id,
            revision_id,
        } => serde_json::to_value(store::reader_document(
            library,
            &item_id,
            &revision_id,
            ceiling,
        )?)?,
        ReaderRequest::Chunk {
            item_id,
            revision_id,
            side,
            chapter,
            offset,
            limit,
        } => serde_json::to_value(store::reader_chunk(
            library,
            &item_id,
            &revision_id,
            &side,
            &chapter,
            offset,
            limit,
            ceiling,
        )?)?,
        ReaderRequest::State { item_id } => {
            let state = store::reader_state(library, &item_id, ceiling)?;
            serde_json::json!({"version":state.version()?,"state":state})
        }
        ReaderRequest::Save {
            item_id,
            expected_version,
            state,
        } => {
            let state =
                store::save_reader_state(library, &item_id, &expected_version, state, ceiling)?;
            serde_json::json!({"version":state.version()?,"state":state})
        }
        ReaderRequest::Resume => serde_json::to_value(store::reader_resume(library, ceiling)?)?,
        ReaderRequest::AnnotationsList { item_id, offset, limit, expected_version, annotation_space: None,
            projection: None, expected_view_version: None } => serde_json::to_value(
            store::annotations::list(library, &item_id, offset, limit, expected_version.as_deref(), ceiling)?)?,
        ReaderRequest::AnnotationsList { annotation_space: None, .. } => anyhow::bail!("invalid_reader_annotation"),
        ReaderRequest::AnnotationsList { item_id, offset, limit, expected_version,
            annotation_space: Some(AnnotationSpace::AiTranslation), projection, expected_view_version } =>
            serde_json::to_value(store::annotations_ai::list(library, &item_id, projection.as_ref(), offset, limit,
                expected_version.as_deref(), expected_view_version.as_deref(), ceiling)?)?,
        ReaderRequest::AnnotationsEdit { item_id, expected_version, change, annotation_space } => {
            use store::annotations_ai as ai;
            match (annotation_space, change) {
                (None, AnnotationChange::Edition(change)) => serde_json::to_value(
                    store::annotations::edit(library, &item_id, &expected_version, change, principal, ceiling)?)?,
                // A delete names its ledger explicitly; an edition-shaped put is never an AI mark.
                (Some(AnnotationSpace::AiTranslation), AnnotationChange::Edition(store::annotations::Change::Delete { id }))
                    => serde_json::to_value(ai::edit(library, &item_id, &expected_version,
                        ai::Change::Delete { id }, principal, ceiling)?)?,
                (Some(AnnotationSpace::AiTranslation), AnnotationChange::Ai(change @ ai::Change::Put { .. })) =>
                    serde_json::to_value(ai::edit(library, &item_id, &expected_version, change, principal, ceiling)?)?,
                _ => anyhow::bail!("invalid_reader_annotation"),
            }
        }
        ReaderRequest::PassesList { item_id, offset, limit, expected_version } => serde_json::to_value(
            store::passes::list(library, &item_id, offset, limit, expected_version.as_deref(), ceiling)?)?,
        ReaderRequest::PassesEdit { item_id, expected_version, change } => serde_json::to_value(
            store::passes::edit(library, &item_id, &expected_version, change, principal, ceiling,
                chrono::Utc::now().timestamp_millis())?)?,
        ReaderRequest::PassesStats { item_id, pass_id, offset, limit, expected_timing_version } => serde_json::to_value(
            store::passes::stats(library, &item_id, &pass_id, offset, limit, expected_timing_version.as_deref(), ceiling)?)?,
        ReaderRequest::PassesTimingStart { item_id, pass_id } => serde_json::to_value(
            store::passes::timing_start(library, &item_id, &pass_id, principal, ceiling, &timing_clock())?)?,
        ReaderRequest::PassesTimingReport { item_id, pass_id, session_id, segments } => serde_json::to_value(
            store::passes::timing_report(library, &item_id, &pass_id, &session_id, &segments, principal, ceiling, &timing_clock())?)?,
    };
    let mut response = Response::empty_ok();
    response.reader = Some(value);
    Ok(response)
}

pub(super) fn dispatch_job(q: &mut Queue, library: &Path, command: Command,
    translation_generation: fn(&Path, &memex_models::Snapshot) -> Result<u64>,
    format_generation: fn(&Path, &memex_models::Snapshot) -> Result<u64>,
) -> Result<std::result::Result<Response, Command>> {
    use crate::queue::{content_format, reader_repair, reader_translation};
    let value = match command {
        Command::Reader { max_sensitivity, principal, request: ReaderRequest::FormatPreview(request) } => {
            let principal = principal.as_deref().unwrap_or("local_owner");
            serde_json::to_value(match request {
                FormatPreviewRequest::Selection { selection, auto_request_id } => content_format::preview_auto(q, library, principal, selection, auto_request_id, max_sensitivity)?,
                FormatPreviewRequest::Status { preview_id } => content_format::preview_status(q, library, principal, &preview_id, max_sensitivity)?,
            })?
        }
        Command::Reader { max_sensitivity, principal, request: ReaderRequest::FormatTargets { preview_id, offset, limit } } =>
            content_format::targets(q, library, principal.as_deref().unwrap_or("local_owner"), &preview_id, offset, limit, max_sensitivity)?,
        Command::Reader { max_sensitivity, principal, request: ReaderRequest::FormatRequest { quote_id, request_id, accept_cost, accept_model_processing } } =>
            content_format::jobs::request_with(q, library, principal.as_deref().unwrap_or("local_owner"), &quote_id, &request_id, accept_cost, accept_model_processing, max_sensitivity, format_generation)?,
        Command::Reader { max_sensitivity, principal, request: ReaderRequest::FormatStatus { batch_id } } =>
            content_format::jobs::status(q, library, principal.as_deref().unwrap_or("local_owner"), &batch_id, max_sensitivity)?,
        Command::Reader { max_sensitivity, principal, request: ReaderRequest::FormatResults { batch_id, offset, limit } } =>
            content_format::jobs::results(q, library, principal.as_deref().unwrap_or("local_owner"), &batch_id, offset, limit, max_sensitivity)?,
        Command::Reader { max_sensitivity, principal, request: ReaderRequest::FormatChanges(request) } => {
            let principal = principal.as_deref().unwrap_or("local_owner");
            match request {
                FormatChangesRequest::List { batch_id, target_id, expected_version, offset, limit } =>
                    serde_json::to_value(content_format::jobs::changes(q, library, principal, &batch_id, &target_id, &expected_version, offset, limit, max_sensitivity)?)?,
                FormatChangesRequest::Text { batch_id, target_id, expected_version, change_id, part, start_byte, limit_bytes } =>
                    serde_json::to_value(content_format::jobs::change_text(q, library, principal, &batch_id, &target_id, &expected_version, &change_id, &part, start_byte, limit_bytes, max_sensitivity)?)?,
            }
        }
        Command::Reader { max_sensitivity, principal, request: ReaderRequest::FormatRestore { batch_id, target_id, expected_revision_id, request_id } } =>
            serde_json::to_value(content_format::jobs::restore(q, library, principal.as_deref().unwrap_or("local_owner"), &batch_id, &target_id, &expected_revision_id, &request_id, max_sensitivity)?)?,
        Command::Reader { max_sensitivity, principal, request: ReaderRequest::FormatResume { batch_id } } =>
            content_format::jobs::resume_with(q, library, principal.as_deref().unwrap_or("local_owner"), &batch_id, max_sensitivity, format_generation)?,
        Command::Reader { max_sensitivity, request: ReaderRequest::TranslatePreview { item_id, revision_id, chapter }, .. } =>
            serde_json::to_value(reader_translation::preview_with(q, library, reader_translation::Selection { item_id, revision_id, chapter }, max_sensitivity, chrono::Utc::now().timestamp_millis(), translation_generation)?)?,
        Command::Reader { max_sensitivity, request: ReaderRequest::TranslateRequest {
            item_id, revision_id, chapter, request_id, quote_id, accept_cost, expected_translation_version }, .. } =>
            serde_json::json!({"job": reader_translation::jobs::request_with(q, library, reader_translation::jobs::Request {
                item_id, revision_id, chapter, request_id, quote_id, accept_cost, expected_translation_version }, max_sensitivity, chrono::Utc::now().timestamp_millis(), translation_generation)?}),
        Command::Reader { max_sensitivity, request: ReaderRequest::TranslateStatus { item_id, job_id }, .. } =>
            serde_json::json!({"job": reader_translation::jobs::status(q, library, &item_id, &job_id, max_sensitivity)?}),
        Command::Reader { max_sensitivity, principal, request: ReaderRequest::EditCommit { item_id, edit_id } } => {
            let receipt = reader_repair::commit_edit(q, library, &item_id, &edit_id,
                principal.as_deref().unwrap_or("local_owner"), max_sensitivity)?;
            let mut response = Response::empty_ok();
            response.reader = Some(serde_json::to_value(receipt)?);
            return Ok(Ok(response));
        }
        Command::Reader { max_sensitivity, request: ReaderRequest::AlignRequest {
            item_id, revision_id, chapter, request_id, expected_alignment_version,
        }, .. } => serde_json::json!({"job": reader_repair::request(q, library, reader_repair::Request {
            item_id, revision_id, chapter, request_id, expected_alignment_version,
        }, max_sensitivity, true)?}),
        Command::Reader { max_sensitivity, request: ReaderRequest::AlignStatus { item_id, job_id }, .. } =>
            serde_json::json!({"job": reader_repair::status(q, library, &item_id, &job_id, max_sensitivity)?}),
        other => return Ok(Err(other)),
    };
    let mut response = Response::empty_ok();
    response.reader = Some(value);
    Ok(Ok(response))
}

#[cfg(test)]
#[path = "../../examples/fixtures/reader_book.rs"]
mod format_fixture;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn translation_discovery_miss_bypasses_credentials_and_queue_admission() {
        let temp = tempfile::tempdir().unwrap();
        let library = temp.path().join("Library");
        let document = format_fixture::seed(&library).unwrap();
        let mut q = Queue::open(&temp.path().join("queue.db")).unwrap();
        let command = Command::Reader { max_sensitivity: Sensitivity::Public, principal: None,
            request: ReaderRequest::TranslateMetadata { item_id: document.item_id.clone(),
                revision_id: document.revision_id.clone(), chapter: "appendix".into() } };
        let unhandled = dispatch_job(&mut q, &library, command,
            |_, _| panic!("discovery must not read translation credentials"),
            |_, _| panic!("discovery must not read format credentials")).unwrap().unwrap_err();
        let Command::Reader { request, .. } = unhandled else { panic!("reader expected") };
        let response = dispatch(&library, request, Sensitivity::Public, None).unwrap().reader.unwrap();
        assert_eq!(response, json!({"item_id": document.item_id, "revision_id": document.revision_id,
            "chapter": "appendix", "translation": null}));
        let conn = rusqlite::Connection::open(temp.path().join("queue.db")).unwrap();
        let counts: (u64, u64) = conn.query_row(
            "SELECT (SELECT count(*) FROM reader_translation_quotes), (SELECT count(*) FROM reader_translation_jobs)",
            [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(counts, (0, 0));
    }

    #[test]
    fn generation_callbacks_keep_translation_and_low_dial_independent() {
        use crate::queue::content_format as format;
        use memex_models::{Configuration, Model, Provider, Purpose, Selection, Snapshot};
        fn translation(_: &Path, settings: &Snapshot) -> Result<u64> {
            assert_eq!(settings.configuration.target(Purpose::Translation).unwrap().0.id, "translator");
            Ok(11)
        }
        fn formatting(_: &Path, settings: &Snapshot) -> Result<u64> {
            assert_eq!(settings.configuration.target(Purpose::CaptureQuality).unwrap().0.id, "formatter");
            Ok(7)
        }
        let temp = tempfile::tempdir().unwrap();
        let library = temp.path().join("Library");
        let doc = format_fixture::seed(&library).unwrap();
        let translated = format_fixture::seed_named(&library, "translation-route-test").unwrap();
        let database = temp.path().join("queue.db");
        let mut q = Queue::open(&database).unwrap();
        let mut conn = rusqlite::Connection::open(&database).unwrap();
        memex_models::replace(&mut conn, 0, Configuration {
            providers: ["translator", "formatter"].iter().map(|id| Provider {
                id: (*id).into(), label: (*id).into(), api_base: "https://synthetic.invalid/v1/".into(),
            }).collect(),
            models: ["translator", "formatter"].iter().map(|id| Model {
                id: (*id).into(), label: (*id).into(), provider_id: (*id).into(), model_id: (*id).into(),
            }).collect(),
            selections: vec![Selection { purpose: Purpose::Translation, model: "translator".into(), reasoning_effort: None }],
        }).unwrap();
        memex_models::low_dial::replace(&mut conn, 0, memex_models::low_dial::Profile {
            revision: 1, model: "formatter".into(), reasoning_effort: None,
            max_input_bytes: 8192, max_completion_tokens: 1024,
        }).unwrap();
        let command = |request| Command::Reader { max_sensitivity: Sensitivity::Public, principal: None, request };
        let preview = dispatch_job(&mut q, &library, command(ReaderRequest::TranslatePreview {
            item_id: translated.item_id.clone(), revision_id: translated.revision_id.clone(), chapter: "appendix".into(),
        }), translation, |_, _| panic!("translation preview called formatting callback"))
            .unwrap().unwrap().reader.unwrap();
        let translated_job = dispatch_job(&mut q, &library, command(ReaderRequest::TranslateRequest {
            item_id: translated.item_id, revision_id: translated.revision_id, chapter: "appendix".into(),
            request_id: "translation_request".into(), quote_id: preview["quote"]["quote_id"].as_str().unwrap().into(),
            accept_cost: true, expected_translation_version: "missing".into(),
        }), translation, |_, _| panic!("translation request called formatting callback"))
            .unwrap().unwrap().reader.unwrap();
        assert_eq!(translated_job["job"]["state"], "queued");
        let preview = format::preview(&q, &library, "local_owner", format::Selection::Item {
            item_id: doc.item_id, revision_id: doc.revision_id,
            target: store::content_format::Target::Chapter { side: "original".into(), chapter: "chapter-1".into() },
        }, Sensitivity::Public).unwrap();
        format::prepare_one(&q, &library, formatting).unwrap();
        let quote = format::preview_status(&q, &library, "local_owner", &preview.preview_id, Sensitivity::Public)
            .unwrap().quote.unwrap();
        assert!(quote.budget["max_requests"].as_u64().unwrap() > 0);
        let batch = dispatch_job(&mut q, &library, command(ReaderRequest::FormatRequest {
            quote_id: quote.quote_id, request_id: "format_request".into(), accept_cost: true, accept_model_processing: true,
        }), |_, _| panic!("format request called translation callback"), formatting)
            .unwrap().unwrap().reader.unwrap();
        let batch_id = batch["batch"]["batch_id"].as_str().unwrap().to_owned();
        // Configuration-generation drift pauses the real durable batch without inference.
        format::jobs::step_with(&mut q, &library, |_, _| Ok(99),
            |_, _, _, _, _| panic!("generation drift must not dispatch"),
            store::content_format::receipt, store::content_format::publish).unwrap();
        assert_eq!(format::jobs::status(&q, &library, "local_owner", &batch_id, Sensitivity::Public).unwrap()["batch"]["state"], "paused");
        let resumed = dispatch_job(&mut q, &library, command(ReaderRequest::FormatResume { batch_id }),
            |_, _| panic!("format resume called translation callback"), formatting)
            .unwrap().unwrap().reader.unwrap();
        assert_eq!(resumed["batch"]["state"], "queued");
        assert_eq!(resumed["batch"]["usage"]["requests_dispatched"], 0);
    }

    #[test]
    fn format_dispatch_preview_is_principal_bound_and_never_resolves_credentials() {
        let temp = tempfile::tempdir().unwrap();
        let library = temp.path().join("Library");
        let document = format_fixture::seed(&library).unwrap();
        let mut q = Queue::open(&temp.path().join("queue.db")).unwrap();
        let principal = "a".repeat(64);
        let dispatch = |q: &mut Queue, request, principal: String| {
            dispatch_job(q, &library, Command::Reader {
                max_sensitivity: Sensitivity::Public, principal: Some(principal), request,
            }, |_, _| panic!("read routing must not resolve credentials"),
                |_, _| panic!("read routing must not resolve formatting credentials"))
        };
        let preview = dispatch(&mut q, ReaderRequest::FormatPreview(FormatPreviewRequest::Selection {
            selection: crate::queue::content_format::Selection::Item {
                item_id: document.item_id.clone(), revision_id: document.revision_id,
                target: store::content_format::Target::Chapter { side: "original".into(), chapter: "chapter-1".into() },
            },
            auto_request_id: None,
        }), principal.clone()).unwrap().unwrap().reader.unwrap();
        assert_eq!(preview["state"], "preparing");
        let preview_id = preview["preview_id"].as_str().unwrap().to_owned();
        let read = dispatch(&mut q, ReaderRequest::FormatPreview(FormatPreviewRequest::Status {
            preview_id: preview_id.clone(),
        }), principal.clone()).unwrap().unwrap().reader.unwrap();
        assert_eq!(read, preview);
        let rows = dispatch(&mut q, ReaderRequest::FormatTargets {
            preview_id: preview_id.clone(), offset: 0, limit: 100,
        }, principal.clone()).unwrap().unwrap().reader.unwrap();
        assert_eq!(rows["total"], 1);
        assert_eq!(rows["targets"][0]["item_id"], document.item_id);
        let denied = dispatch(&mut q, ReaderRequest::FormatTargets {
            preview_id, offset: 0, limit: 100,
        }, "b".repeat(64)).unwrap_err();
        assert_eq!(denied.to_string(), "content_format_preview_not_found");
    }

    #[test]
    fn ai_translation_marks_are_a_separate_space_and_legacy_wire_keeps_its_shape() {
        let temp = tempfile::tempdir().unwrap();
        let library = temp.path().join("Library");
        let doc = format_fixture::seed(&library).unwrap();
        let source = store::chapter_translation::source(&library, &doc.item_id, &doc.revision_id, "appendix",
            Sensitivity::Public).unwrap();
        let meta = store::chapter_translation::publish(&library, source.identity, store::chapter_translation::Provenance {
            route: store::chapter_translation::Route { provider_id: "fixture".into(), model_id: "synthetic".into(),
                reasoning_effort: None },
            prompt_version: "synthetic".into(), prompt_sha256: "a".repeat(64), plan_sha256: "b".repeat(64),
        }, "# 附录\n\n合成🌹正文。\n".into(), Sensitivity::Public).unwrap();
        let call = |request: serde_json::Value| -> Result<serde_json::Value> {
            let command: Command = serde_json::from_value(json!({"command":"reader","max_sensitivity":"public",
                "principal":"a".repeat(64),"request":request}))?;
            let Command::Reader { request, principal, max_sensitivity } = command else { unreachable!() };
            Ok(dispatch(&library, request, max_sensitivity, principal)?.reader.unwrap())
        };
        let item = doc.item_id.clone();
        let legacy = call(json!({"op":"annotations_list","item_id":item,"offset":0,"limit":100,"expected_version":null})).unwrap();
        let mut keys: Vec<_> = legacy.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["annotations", "item_id", "next_offset", "total", "version"]);
        let ai = call(json!({"op":"annotations_list","item_id":item,"offset":0,"limit":100,"expected_version":null,
            "annotation_space":"ai_translation"})).unwrap();
        assert_eq!(ai["annotation_space"], "ai_translation");
        assert!(ai["view_version"].as_str().unwrap().len() == 64);
        let seg = json!({"chapter":"appendix","translation_version":meta.version,"sha256":meta.sha256,"start":0,"end":9});
        let put = |id: &str| json!({"op":"put","annotation":{"id":id,"style":"red","anchor":{
            "revision_id":doc.revision_id,"side":"ai_translation","segments":[seg]}}});
        // AI anchor without space, or an edition anchor in the AI space, rejects.
        assert_eq!(call(json!({"op":"annotations_edit","item_id":item,"expected_version":legacy["version"],
            "change":put("m")})).unwrap_err().to_string(), "invalid_reader_annotation");
        let e = &doc.editions[0];
        let edition_put = json!({"op":"put","annotation":{"id":"m","style":"red","anchor":{"revision_id":doc.revision_id,
            "side":e.side,"sha256":e.sha256,"segments":[{"chapter":"chapter-1","start":0,"end":3}]}}});
        assert_eq!(call(json!({"op":"annotations_edit","item_id":item,"expected_version":ai["version"],
            "change":edition_put,"annotation_space":"ai_translation"})).unwrap_err().to_string(), "invalid_reader_annotation");
        // Legacy list never accepts AI-only fields.
        assert_eq!(call(json!({"op":"annotations_list","item_id":item,"offset":0,"limit":100,"expected_version":null,
            "expected_view_version":ai["view_version"]})).unwrap_err().to_string(), "invalid_reader_annotation");
        let saved = call(json!({"op":"annotations_edit","item_id":item,"expected_version":ai["version"],
            "change":put("m"),"annotation_space":"ai_translation"})).unwrap();
        assert_eq!((saved["outcome"].as_str(), saved["annotation_space"].as_str()), (Some("created"), Some("ai_translation")));
        let ed = call(json!({"op":"annotations_edit","item_id":item,"expected_version":legacy["version"],
            "change":edition_put})).unwrap();
        // The legacy list holds only the edition mark, in its original shape.
        let legacy = call(json!({"op":"annotations_list","item_id":item,"offset":0,"limit":100,"expected_version":null})).unwrap();
        assert_eq!((legacy["total"].as_u64(), legacy["version"].clone()), (Some(1), ed["version"].clone()));
        assert!(legacy["annotations"][0]["anchor"].get("sha256").is_some() && legacy["annotations"][0].get("projections").is_none());
        // A delete without space never reaches the AI ledger, even for the same id.
        call(json!({"op":"annotations_edit","item_id":item,"expected_version":ed["version"],
            "change":{"op":"delete","id":"m"}})).unwrap();
        let ai = call(json!({"op":"annotations_list","item_id":item,"offset":0,"limit":100,"expected_version":null,
            "annotation_space":"ai_translation","projection":{"chapter":"appendix","translation_version":meta.version,"sha256":meta.sha256}})).unwrap();
        assert_eq!(ai["total"], 1);
        assert_eq!(ai["annotations"][0]["projections"][0]["status"], "current");
        let deleted = call(json!({"op":"annotations_edit","item_id":item,"expected_version":ai["version"],
            "change":{"op":"delete","id":"m"},"annotation_space":"ai_translation"})).unwrap();
        assert_eq!(deleted["outcome"], "deleted");
        // Strict wire: unknown space, unknown keys and a partial filter reject at parse.
        for bad in [
            json!({"op":"annotations_list","item_id":item,"offset":0,"limit":1,"expected_version":null,"annotation_space":"summary"}),
            json!({"op":"annotations_list","item_id":item,"offset":0,"limit":1,"expected_version":null,"annotation_space":"ai_translation","extra":1}),
            json!({"op":"annotations_list","item_id":item,"offset":0,"limit":1,"expected_version":null,"annotation_space":"ai_translation",
                "projection":{"translation_version":meta.version}}),
        ] {
            assert!(call(bad).is_err());
        }
        assert!(crate::ipc::query_worker::is_store_read(&serde_json::from_value(json!({"op":"annotations_list",
            "item_id":item,"offset":0,"limit":1,"expected_version":null,"annotation_space":"ai_translation"})).unwrap()));
    }

    #[test]
    fn format_wire_shapes_are_exclusive_and_strict() {
        let selection = json!({"kind":"item","item_id":"item_test","revision_id":"revision_test","target":{"kind":"body"}});
        let requests = vec![
            json!({"op":"format_preview","selection":selection}),
            json!({"op":"format_preview","preview_id":"preview_test"}),
            json!({"op":"format_targets","preview_id":"preview_test","offset":0,"limit":100}),
            json!({"op":"format_request","quote_id":"quote_test","request_id":"request_test","accept_cost":true,"accept_model_processing":true}),
            json!({"op":"format_status","batch_id":"batch_test"}),
            json!({"op":"format_results","batch_id":"batch_test","offset":0,"limit":100}),
            json!({"op":"format_changes","batch_id":"batch_test","target_id":"target_test","expected_version":"hash","view":"list","offset":0,"limit":64}),
            json!({"op":"format_changes","batch_id":"batch_test","target_id":"target_test","expected_version":"hash","view":"text","change_id":"change_test","part":"before","start_byte":0,"limit_bytes":8192}),
            json!({"op":"format_restore","batch_id":"batch_test","target_id":"target_test","expected_revision_id":"revision_test","request_id":"restore_test"}),
            json!({"op":"format_resume","batch_id":"batch_test"}),
        ];
        for value in requests {
            let parsed: ReaderRequest = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(parsed).unwrap(), value);
            let mut extra = value.clone();
            extra["principal"] = json!("local_owner");
            assert!(serde_json::from_value::<ReaderRequest>(extra).is_err());
            // accept_cost is no longer a gate (owner subscription); old clients still send it.
            for key in value.as_object().unwrap().keys().filter(|key| *key != "op" && *key != "accept_cost") {
                let mut missing = value.clone();
                missing.as_object_mut().unwrap().remove(key);
                assert!(serde_json::from_value::<ReaderRequest>(missing).is_err(), "missing {key}");
            }
        }
        assert!(serde_json::from_value::<ReaderRequest>(json!({"op":"format_request","quote_id":"quote_test",
            "request_id":"request_test","accept_model_processing":true})).is_ok());
        let one_step: ReaderRequest = serde_json::from_value(json!({"op":"format_preview","selection":selection,
            "auto_request_id":"request_test"})).unwrap();
        assert_eq!(serde_json::to_value(one_step).unwrap()["auto_request_id"], "request_test");
        for invalid in [
            json!({"op":"format_preview","selection":selection,"preview_id":"preview_test"}),
            json!({"op":"format_preview","selection":{"kind":"group","group_id":"group_test"}}),
            json!({"op":"format_preview","selection":{"kind":"item","item_id":"item_test","revision_id":"revision_test","target":{"kind":"body","side":"original"}}}),
            json!({"op":"format_changes","batch_id":"batch_test","target_id":"target_test","expected_version":"hash","view":"list","offset":0,"limit":1,"change_id":"change_test"}),
        ] {
            assert!(serde_json::from_value::<ReaderRequest>(invalid).is_err());
        }
    }
}
