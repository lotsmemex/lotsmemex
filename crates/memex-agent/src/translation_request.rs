//! `translation_request`: the explicit owner request that authorizes provider-backed work for
//! exactly one item's current revision: translation (non-Chinese sources only) and metadata
//! enrichment. AI minutes are manual only: `minutes_request` (also here) is their sole
//! authorization; `translation_request` only reports their current state. Background
//! reconciliation never authorizes provider work.

use crate::ipc::{Command, Response};
use crate::provider_generation::saved_route_model;
use crate::queue::Queue;
use anyhow::{Context, Result};
use memex_analysis::{
    SourceClassification, SummaryEnqueueOutcome, SummaryJobState, SummaryQueue,
    TranslationEnqueueOutcome, TranslationJobState, TranslationProjection, TranslationQueue,
    TranslationSkipReason,
};
use memex_core::Sensitivity;
use memex_models::Purpose;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Text bound of one AI minutes reply (the prompt allows at most 1,200 characters).
const MAX_MINUTES_TEXT_BYTES: usize = 16 * 1024;

/// `ai_minutes` in `library_item_read` and `translation_request` replies. `state` uses the
/// translation vocabulary; `text` is present only when `state` is `succeeded`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AiMinutes {
    pub state: String,
    pub reason: Option<String>,
    pub text: Option<String>,
    /// Additive, read-only: the title the minutes call suggested (prompt version 3). Absent
    /// for version-2 minutes. Applied only by an explicit `item_title_apply`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_title: Option<String>,
}

impl AiMinutes {
    fn of(state: &str, reason: Option<&str>) -> Self {
        Self {
            state: state.into(),
            reason: reason.map(Into::into),
            text: None,
            suggested_title: None,
        }
    }
}

/// Saved routes (no network, no Keychain secret read) for the purposes a request may use.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct RequestRoutes {
    pub translation: bool,
    pub minutes: bool,
    pub metadata: bool,
}

pub(crate) fn dispatch(
    q: &mut Queue,
    library: &Path,
    c: Command,
) -> Result<std::result::Result<Response, Command>> {
    let (item_id, max_sensitivity, minutes_only) = match c {
        Command::TranslationRequest {
            item_id,
            max_sensitivity,
        } => (item_id, max_sensitivity, false),
        Command::MinutesRequest {
            item_id,
            max_sensitivity,
        } => (item_id, max_sensitivity, true),
        // A read: no model settings, credential or queue write.
        Command::ScopedMinutesStatus {
            item_id,
            max_sensitivity,
        } => {
            let mut response = Response::empty_ok();
            response.ai_minutes = Some(minutes_status(library, &item_id, max_sensitivity)?);
            return Ok(Ok(response));
        }
        Command::TitleTranslationBackfill {
            batch_size,
            dry_run,
        } => {
            // A dry run never reads model settings or credentials.
            let routed = !dry_run
                && q.model_settings().ok().is_some_and(|snapshot| {
                    saved_route_model(q.database_path(), &snapshot, Purpose::Translation).is_some()
                });
            let mut response = Response::empty_ok();
            response.title_backfill =
                Some(title_backfill(library, batch_size, dry_run, routed)?);
            return Ok(Ok(response));
        }
        // A read: no model settings, credential or queue write.
        Command::ScopedInsightsStatus {
            item_id,
            max_sensitivity,
        } => {
            let mut response = Response::empty_ok();
            response.ai_insights = Some(insights_status(library, &item_id, max_sensitivity)?);
            return Ok(Ok(response));
        }
        other => return Ok(Err(other)),
    };
    // Settings with no route never touch the credential store.
    let routes = match q.model_settings() {
        Ok(snapshot) => {
            let route =
                |purpose| saved_route_model(q.database_path(), &snapshot, purpose).is_some();
            RequestRoutes {
                translation: route(Purpose::Translation),
                minutes: route(Purpose::Summary),
                metadata: route(Purpose::Classification),
            }
        }
        Err(_) => RequestRoutes::default(),
    };
    if minutes_only {
        let mut response = Response::empty_ok();
        response.ai_minutes = Some(minutes_request(
            library,
            &item_id,
            max_sensitivity,
            routes.minutes,
        )?);
        return Ok(Ok(response));
    }
    let (translation, minutes) = request(library, &item_id, max_sensitivity, routes)?;
    let mut response = Response::empty_ok();
    response.translation = Some(translation);
    response.ai_minutes = Some(minutes);
    Ok(Ok(response))
}

/// Largest `batch_size` of one `title_translation_backfill`.
const MAX_TITLE_BACKFILL_BATCH: usize = 5_000;

/// `title_translation_backfill` report. Token figures are estimates (see
/// `memex_analysis::estimate_title_tokens`): `estimated_*` for this batch, `all_*` for every
/// current candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TitleBackfill {
    pub dry_run: bool,
    /// Items whose title is translatable and that have no Chinese title yet (nor a queued one).
    pub candidates: usize,
    /// Items in this batch: queued now, or that would be queued (dry run).
    pub batch: usize,
    pub queued: usize,
    pub remaining: usize,
    /// Model requests for this batch (`titles_per_request` titles each).
    pub requests: usize,
    pub titles_per_request: usize,
    pub estimated_input_tokens: u64,
    pub estimated_output_tokens: u64,
    pub all_input_tokens: u64,
    pub all_output_tokens: u64,
}

/// Owner backfill over the whole library (oldest item ids first). A dry run queues and sends
/// nothing; a real run only queues rows the provider worker sends in batches.
pub(crate) fn title_backfill(
    library: &Path,
    batch_size: usize,
    dry_run: bool,
    routed: bool,
) -> Result<TitleBackfill> {
    if batch_size == 0 || batch_size > MAX_TITLE_BACKFILL_BATCH {
        anyhow::bail!("invalid_request")
    }
    if !dry_run && !routed {
        anyhow::bail!("no_translation_provider")
    }
    let items: Vec<(String, String, String)> =
        memex_store::with_consistent_snapshot(library, |root| {
            memex_store::item_change_stamps(root)?
                .into_iter()
                .map(|(item_id, _)| {
                    let (_, revision) = memex_store::read_item(root, &item_id)?;
                    Ok((item_id, revision.revision_id, revision.title))
                })
                .collect()
        })?;
    let keys: Vec<(&str, &str, &str)> = items
        .iter()
        .map(|(item, revision, title)| (item.as_str(), revision.as_str(), title.as_str()))
        .collect();
    let selected = memex_analysis::title_backfill_candidates(library, &keys)?;
    let candidates: Vec<(&str, &str)> = keys
        .iter()
        .zip(selected)
        .filter(|(_, selected)| *selected)
        .map(|((item, _, title), _)| (*item, *title))
        .collect();
    let batch = &candidates[..candidates.len().min(batch_size)];
    let (estimated_input_tokens, estimated_output_tokens) =
        memex_analysis::estimate_title_tokens(batch.iter().map(|(_, title)| *title));
    let (all_input_tokens, all_output_tokens) =
        memex_analysis::estimate_title_tokens(candidates.iter().map(|(_, title)| *title));
    let mut queued = 0;
    if !dry_run {
        let mut queue = memex_analysis::TitleTranslationQueue::open(library)?;
        for (item_id, title) in batch {
            queued += usize::from(queue.request_backfill(item_id, title)?);
        }
    }
    Ok(TitleBackfill {
        dry_run,
        candidates: candidates.len(),
        batch: batch.len(),
        queued,
        remaining: candidates.len() - queued,
        requests: batch.len().div_ceil(memex_analysis::TITLES_PER_REQUEST),
        titles_per_request: memex_analysis::TITLES_PER_REQUEST,
        estimated_input_tokens,
        estimated_output_tokens,
        all_input_tokens,
        all_output_tokens,
    })
}

/// Best effort: fills `translated_title`/`title_source` of query results from analysis.
pub(crate) fn fill_translated_titles(library: &Path, page: &mut memex_library::QueryPage) {
    let items = &mut page.items;
    let keys: Vec<(&str, &str, &str)> = items
        .iter()
        .map(|item| {
            (
                item.item_id.as_str(),
                item.revision_id.as_str(),
                item.title.as_str(),
            )
        })
        .collect();
    let Ok(views) = memex_analysis::translated_titles(library, &keys) else {
        return;
    };
    for (item, view) in items.iter_mut().zip(views) {
        if let Some(view) = view {
            item.translated_title = Some(view.translated_title);
            item.title_source = Some(view.title_source);
        }
    }
}

fn skip_reason(reason: TranslationSkipReason) -> String {
    match reason {
        TranslationSkipReason::AlreadyChinese => "source_is_chinese".into(),
        other => serde_json::to_value(other)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_else(|| "translation_not_applicable".into()),
    }
}

pub(crate) fn request(
    library: &Path,
    item_id: &str,
    max_sensitivity: Sensitivity,
    routes: RequestRoutes,
) -> Result<(TranslationProjection, AiMinutes)> {
    let (_, revision) = memex_store::read_item_at_sensitivity(library, item_id, max_sensitivity)?
        .filter(|(item, revision)| item.sensitivity.max(revision.sensitivity) <= max_sensitivity)
        .context("library_resource_not_found")?;
    let revision_id = revision.revision_id.as_str();
    let classification =
        memex_analysis::classify_source(&revision.title, &revision.content_markdown);
    let translation = translation_request(
        library,
        item_id,
        max_sensitivity,
        &revision,
        classification,
        routes,
    )?;
    // Requested last, after every authorization it may carry to its new revision (see
    // `provider_generation::carry_metadata_only_revision`); generated text for the item waits
    // for it (the queues' metadata gate).
    if routes.metadata
        && matches!(
            classification,
            SourceClassification::Chinese
                | SourceClassification::English
                | SourceClassification::Other
        )
    {
        memex_analysis::MetadataQueue::open(library)?.request(item_id, revision_id)?;
    }
    // Reported, never authorized here.
    let mut minutes = ai_minutes(library, item_id, revision_id)?;
    if minutes.state == "not_requested" && !routes.minutes {
        minutes = AiMinutes::of("unavailable", Some("no_summary_provider"));
    }
    Ok((translation, minutes))
}

/// `minutes_request`: succeeded minutes for the current revision are returned as they are (no
/// model call); otherwise the minutes job is authorized, or a failed one requeued with a fresh
/// budget. Chinese and non-Chinese sources alike; documents the summary classifier rejects
/// (too little text, challenge pages) report `not_requested / minutes_not_applicable`.
pub(crate) fn minutes_request(
    library: &Path,
    item_id: &str,
    max_sensitivity: Sensitivity,
    has_route: bool,
) -> Result<AiMinutes> {
    let (_, revision) = memex_store::read_item_at_sensitivity(library, item_id, max_sensitivity)?
        .filter(|(item, revision)| item.sensitivity.max(revision.sensitivity) <= max_sensitivity)
        .context("library_resource_not_found")?;
    let revision_id = revision.revision_id.as_str();
    let current = ai_minutes(library, item_id, revision_id)?;
    if current.state == "succeeded" {
        return Ok(current);
    }
    if !has_route {
        anyhow::bail!("no_minutes_provider");
    }
    match memex_analysis::enqueue_item_summary(library, item_id, revision_id)? {
        SummaryEnqueueOutcome::Enqueued { .. } => {
            SummaryQueue::open(library)?.request_minutes(item_id, revision_id)?;
        }
        SummaryEnqueueOutcome::Skipped { .. } => {
            return Ok(AiMinutes::of(
                "not_requested",
                Some("minutes_not_applicable"),
            ));
        }
    }
    ai_minutes(library, item_id, revision_id)
}

/// `scoped_minutes_status`: the current revision's minutes as `library_item_read` reports them,
/// for an item visible under `max_sensitivity`. Authorizes nothing.
pub(crate) fn minutes_status(
    library: &Path,
    item_id: &str,
    max_sensitivity: Sensitivity,
) -> Result<AiMinutes> {
    let (_, revision) = memex_store::read_item_at_sensitivity(library, item_id, max_sensitivity)?
        .filter(|(item, revision)| item.sensitivity.max(revision.sensitivity) <= max_sensitivity)
        .context("library_resource_not_found")?;
    ai_minutes(library, item_id, &revision.revision_id)
}

/// Serialized insights a `scoped_insights_status` reply may carry (the gateway channel and the
/// daemon frame both allow more; this keeps a reply and its envelope comfortably inside).
const MAX_INSIGHTS_STATUS_BYTES: usize = 192 * 1024;

/// `scoped_insights_status`: the current revision's insights (states and validated items) for an
/// item visible under `max_sensitivity`. Authorizes nothing. When the items would not fit,
/// the largest kinds lose their items first and say so (`insights_too_large_for_read`), never
/// an empty list.
pub(crate) fn insights_status(
    library: &Path,
    item_id: &str,
    max_sensitivity: Sensitivity,
) -> Result<crate::insights::AiInsights> {
    let (_, revision) = memex_store::read_item_at_sensitivity(library, item_id, max_sensitivity)?
        .filter(|(item, revision)| item.sensitivity.max(revision.sensitivity) <= max_sensitivity)
        .context("library_resource_not_found")?;
    let mut insights = crate::insights::projection(library, &revision)?;
    while serde_json::to_vec(&insights)?.len() > MAX_INSIGHTS_STATUS_BYTES {
        let largest = [
            &mut insights.highlights,
            &mut insights.quotes,
            &mut insights.fact_check,
        ]
        .into_iter()
        .filter(|view| view.items.is_some())
        .max_by_key(|view| {
            view.items
                .as_ref()
                .map(|items| items.to_string().len())
                .unwrap_or(0)
        });
        match largest {
            Some(view) => {
                view.items = None;
                view.reason = Some("insights_too_large_for_read".into());
            }
            None => anyhow::bail!("result_too_large"),
        }
    }
    Ok(insights)
}

fn translation_request(
    library: &Path,
    item_id: &str,
    max_sensitivity: Sensitivity,
    revision: &memex_core::ItemRevision,
    classification: SourceClassification,
    routes: RequestRoutes,
) -> Result<TranslationProjection> {
    let outcome = |state: &str, reason: String| TranslationProjection {
        state: state.into(),
        reason: Some(reason),
        revision_id: revision.revision_id.clone(),
        translated_title: None,
        progress: None,
        source_revision_id: None,
        failure_detail: None,
        failure_context: None,
        untranslated_blocks: None,
        title_untranslated: None,
        page: None,
    };
    // Language gate before any provider check or enqueue: no translation call for Chinese.
    if classification == SourceClassification::Chinese {
        return Ok(outcome("not_requested", "source_is_chinese".into()));
    }
    if !routes.translation {
        return Ok(outcome("unavailable", "no_translation_provider".into()));
    }
    match memex_analysis::enqueue_item_translation(library, item_id, &revision.revision_id)? {
        TranslationEnqueueOutcome::Skipped { reason, .. } => {
            return Ok(outcome("not_requested", skip_reason(reason)))
        }
        TranslationEnqueueOutcome::Enqueued { job } => {
            let mut queue = TranslationQueue::open(library)?;
            match job.state {
                TranslationJobState::Succeeded => {}
                TranslationJobState::Failed => {
                    queue.requeue_failed(item_id, &revision.revision_id)?;
                }
                TranslationJobState::Queued | TranslationJobState::Running => {
                    queue.authorize(item_id, &revision.revision_id)?;
                }
            }
        }
    }
    memex_analysis::read_translation_projection(library, item_id, max_sensitivity, None)
}

/// Current-revision AI minutes. A version-1 faithful summary is never reported as minutes.
pub(crate) fn ai_minutes(library: &Path, item_id: &str, revision_id: &str) -> Result<AiMinutes> {
    if !memex_analysis::summary_prompt_version(library, item_id, revision_id)?
        .as_deref()
        .is_some_and(memex_analysis::is_ai_minutes_version)
    {
        return Ok(AiMinutes::of("not_requested", None));
    }
    let Some(status) = memex_analysis::summary_status(library, item_id, revision_id)? else {
        return Ok(AiMinutes::of("not_requested", None));
    };
    Ok(match status.state {
        // Reconciled jobs nobody requested never run.
        SummaryJobState::Queued
            if !memex_analysis::summary_authorized(library, item_id, revision_id)? =>
        {
            AiMinutes::of("not_requested", None)
        }
        SummaryJobState::Queued => AiMinutes::of("queued", status.reason_code.as_deref()),
        SummaryJobState::Running => AiMinutes::of("running", None),
        SummaryJobState::Failed => AiMinutes::of("failed", status.reason_code.as_deref()),
        SummaryJobState::Succeeded => {
            let text = memex_analysis::validated_summary(library, item_id, revision_id)?
                .map(|summary| {
                    memex_analysis::read_record_at_sensitivity(
                        library,
                        &summary.analysis_id,
                        Sensitivity::Restricted,
                    )
                })
                .transpose()?
                .flatten()
                .filter(|record| {
                    record.generated_text.as_ref().is_some_and(|generated| {
                        generated.prompt_id == memex_analysis::AI_MINUTES_PROMPT_ID
                    })
                })
                .map(|record| record.body_markdown)
                .filter(|text| text.len() <= MAX_MINUTES_TEXT_BYTES);
            match text {
                Some(text) => {
                    let (text, suggested_title) = memex_analysis::split_suggested_title(&text);
                    AiMinutes {
                        state: "succeeded".into(),
                        reason: None,
                        text: Some(text),
                        suggested_title,
                    }
                }
                None => AiMinutes::of("unavailable", Some("minutes_unavailable")),
            }
        }
    })
}
