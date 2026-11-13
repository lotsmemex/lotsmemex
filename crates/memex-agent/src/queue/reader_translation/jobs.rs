//! Durable bounded chapter work. A failed or uncertain model step is retried automatically at
//! most `AUTO_RETRIES` times with backoff, resuming from the validated batches.
use super::*;
use crate::queue::reader_repair::{self, DispatchUsage, Usage};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub item_id: String,
    pub revision_id: String,
    pub chapter: String,
    pub request_id: String,
    /// Empty or absent: one step — the server issues and accepts the quote itself.
    #[serde(default)]
    pub quote_id: String,
    /// Kept for older clients; no longer required (no per-request cost gate).
    #[serde(default)]
    pub accept_cost: bool,
    pub expected_translation_version: String,
}
impl Request {
    fn selection(&self) -> Selection {
        Selection {
            item_id: self.item_id.clone(),
            revision_id: self.revision_id.clone(),
            chapter: self.chapter.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub job_id: String,
    pub operation: String,
    #[serde(flatten)]
    pub request: Request,
    pub state: String,
    pub stage: String,
    pub result_revision_id: Option<String>,
    pub alignment_version: Option<String>,
    pub alignment_status: Option<String>,
    pub error_code: Option<String>,
    pub failure_stage: Option<String>,
    pub failure_reason: Option<String>,
    pub budget: planner::Budget,
    pub usage: Usage,
    pub elapsed_ms: u64,
    pub completed_blocks: usize,
    pub total_blocks: usize,
    pub translation: Option<projection::Metadata>,
}

#[derive(Serialize, Deserialize)]
struct Record {
    job: Job,
    library: PathBuf,
    ceiling: Sensitivity,
    frozen: FrozenQuote,
    // Validated generated batches are private durable recovery data, never logs.
    outputs: Vec<String>,
    in_flight: bool,
    started_at: Option<i64>,
    #[serde(default)]
    retry_at: i64,
    #[serde(default)]
    auto_retries: u32,
}

fn permitted(library: &Path, item: &str, ceiling: Sensitivity) -> Result<()> {
    anyhow::ensure!(
        memex_store::read_item_at_sensitivity(library, item, ceiling)?.is_some(),
        "library_resource_not_found"
    );
    Ok(())
}
fn put(q: &Queue, r: &Record) -> Result<()> {
    q.conn.execute(
        "UPDATE reader_translation_jobs SET state=?1,payload=?2 WHERE job_id=?3",
        params![r.job.state, serde_json::to_string(r)?, r.job.job_id],
    )?;
    Ok(())
}
fn replay(conn: &Connection, library: &Path, request: &Request) -> Result<Option<Job>> {
    let json: Option<String> = conn
        .query_row(
            "SELECT payload FROM reader_translation_jobs WHERE request_id=?1",
            [&request.request_id],
            |r| r.get(0),
        )
        .optional()?;
    json.map(|json| {
        let r: Record = serde_json::from_str(&json)?;
        anyhow::ensure!(
            r.library == library && r.job.request == *request,
            "request_identity_conflict"
        );
        Ok(r.job)
    })
    .transpose()
}

pub fn request(
    q: &mut Queue,
    library: &Path,
    request: Request,
    ceiling: Sensitivity,
) -> Result<Job> {
    request_with(
        q,
        library,
        request,
        ceiling,
        Utc::now().timestamp_millis(),
        credential_generation,
    )
}

pub(crate) fn request_with(
    q: &mut Queue,
    library: &Path,
    request: Request,
    ceiling: Sensitivity,
    now: i64,
    generation: impl Fn(&Path, &Snapshot) -> Result<u64>,
) -> Result<Job> {
    anyhow::ensure!(
        request.expected_translation_version == "missing"
            && (1..=128).contains(&request.request_id.len())
            && request
                .request_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')),
        "invalid_reader_translation_request"
    );
    let library = library.canonicalize()?;
    permitted(&library, &request.item_id, ceiling)?;
    if let Some(job) = replay(&q.conn, &library, &request)? {
        return Ok(job);
    }
    // One step: the server issues the quote the owner's single click accepts.
    let quote_id = if request.quote_id.is_empty() {
        super::issue_quote(q, &library, request.selection(), ceiling, now, &generation)?
    } else {
        request.quote_id.clone()
    };
    // No queue writer transaction across disk planning or Keychain reads.
    let completed = projection::metadata(
        &library,
        &request.item_id,
        &request.revision_id,
        &request.chapter,
        ceiling,
    )?;
    let (frozen, plan) = if completed.is_some() {
        // Reusing a published result spends nothing, even after the original quote
        // expired or the provider was removed. Still bind its authorized resource.
        let json: String = q
            .conn
            .query_row(
                "SELECT payload FROM reader_translation_quotes WHERE quote_id=?1",
                [&quote_id],
                |r| r.get(0),
            )
            .optional()?
            .context("reader_translation_quote_stale")?;
        let frozen: FrozenQuote = serde_json::from_str(&json)?;
        let source = projection::source(
            &library,
            &request.item_id,
            &request.revision_id,
            &request.chapter,
            ceiling,
        )?;
        anyhow::ensure!(
            frozen.library == library && frozen.identity == source.identity,
            "reader_translation_quote_stale"
        );
        let plan = planner::plan(&source.markdown, limits())?;
        (frozen, plan)
    } else {
        validate_quote(
            q,
            &library,
            &request.selection(),
            &quote_id,
            ceiling,
            now,
            &generation,
        )?
    };
    let tx = q
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(job) = replay(&tx, &library, &request)? {
        return Ok(job);
    }
    anyhow::ensure!(
        !reader_item_busy(&tx, &library, &request.item_id)?,
        "reader_job_in_progress"
    );
    let mut outputs = Vec::new();
    // Explicit retries reuse only validated complete prefix batches with exactly
    // the same immutable source, prompt, route and plan. Uncertain work fences retry.
    let previous = tx
        .prepare("SELECT payload FROM reader_translation_jobs WHERE library=?1 AND item_id=?2")?
        .query_map(params![library.to_string_lossy(), request.item_id], |r| {
            r.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for json in previous {
        let old: Record = serde_json::from_str(&json)?;
        if old.frozen.identity == frozen.identity
            && old.frozen.plan_sha256 == frozen.plan_sha256
            && old.frozen.quote.route == frozen.quote.route
        {
            anyhow::ensure!(
                completed.is_some() || old.job.state != "outcome_unknown",
                "reader_translation_unavailable"
            );
            if old.outputs.len() > outputs.len() {
                anyhow::ensure!(
                    old.outputs.len() <= plan.batches().len(),
                    "reader_translation_unavailable"
                );
                for (batch, answer) in plan.batches().iter().zip(&old.outputs) {
                    batch.validate_output(answer)?;
                }
                outputs = old.outputs;
            }
        }
    }
    let reused = completed.is_some();
    let total_blocks = plan.batches().iter().map(|b| b.blocks().len()).sum();
    let completed_blocks = if reused {
        total_blocks
    } else {
        plan.batches()
            .iter()
            .take(outputs.len())
            .map(|b| b.blocks().len())
            .sum()
    };
    let record = Record {
        job: Job {
            job_id: format!("job_{}", uuid::Uuid::now_v7().simple()),
            operation: "chapter_translation".into(),
            result_revision_id: reused.then(|| request.revision_id.clone()),
            request,
            state: if reused { "succeeded" } else { "queued" }.into(),
            stage: if reused { "done" } else { "queued" }.into(),
            alignment_version: None,
            alignment_status: None,
            error_code: None,
            failure_stage: None,
            failure_reason: None,
            budget: plan.budget().clone(),
            usage: Usage {
                dispatches: Some(vec![]),
                input_tokens: Some(0),
                output_tokens: Some(0),
                ..Usage::default()
            },
            elapsed_ms: 0,
            completed_blocks,
            total_blocks,
            translation: completed,
        },
        library,
        ceiling,
        frozen,
        outputs,
        in_flight: false,
        started_at: None,
        retry_at: 0,
        auto_retries: 0,
    };
    tx.execute("INSERT INTO reader_translation_jobs(job_id,request_id,library,item_id,state,payload) VALUES(?1,?2,?3,?4,?5,?6)",
        params![record.job.job_id, record.job.request.request_id, record.library.to_string_lossy(), record.job.request.item_id, record.job.state, serde_json::to_string(&record)?])?;
    tx.commit()?;
    Ok(record.job)
}

pub fn status(
    q: &Queue,
    library: &Path,
    item: &str,
    job_id: &str,
    ceiling: Sensitivity,
) -> Result<Job> {
    permitted(library, item, ceiling)?;
    let json: String = q
        .conn
        .query_row(
            "SELECT payload FROM reader_translation_jobs WHERE job_id=?1",
            [job_id],
            |r| r.get(0),
        )
        .optional()?
        .context("reader_job_not_found")?;
    let r: Record = serde_json::from_str(&json)?;
    anyhow::ensure!(
        r.library == library.canonicalize()? && r.job.request.item_id == item,
        "reader_job_not_found"
    );
    Ok(r.job)
}

/// Requeues the job for an automatic retry when the budget allows; the next run resumes after
/// the validated batches in `outputs`.
fn retry_later(r: &mut Record) -> bool {
    if r.auto_retries >= crate::queue::AUTO_RETRIES {
        return false;
    }
    r.auto_retries += 1;
    r.in_flight = false;
    r.job.state = "queued".into();
    r.retry_at = Utc::now().timestamp_millis() + crate::queue::auto_retry_delay_ms(r.auto_retries);
    true
}

fn finish_error(r: &mut Record, unknown: bool, reason: &str) {
    r.job.failure_stage = Some(r.job.stage.clone());
    r.job.failure_reason = Some(reason.into());
    r.job.error_code = Some(reason.into());
    r.job.state = if unknown { "outcome_unknown" } else { "failed" }.into();
    r.job.stage = "done".into();
}

fn recover_locked(q: &Queue, library: &Path) -> Result<()> {
    let rows = q
        .conn
        .prepare(
            "SELECT payload FROM reader_translation_jobs WHERE library=?1 AND state='running'",
        )?
        .query_map([library.to_string_lossy()], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for row in rows {
        let mut r: Record = serde_json::from_str(&row)?;
        if let Some(start) = r.started_at.take() {
            r.job.elapsed_ms = r
                .job
                .elapsed_ms
                .saturating_add(Utc::now().timestamp_millis().saturating_sub(start).max(0) as u64);
        }
        if r.in_flight && !retry_later(&mut r) {
            finish_error(&mut r, true, "provider_outcome_unknown");
        } else if !r.in_flight {
            r.job.state = "queued".into();
        }
        put(q, &r)?;
    }
    Ok(())
}

pub fn step(q: &mut Queue, library: &Path) -> Result<bool> {
    let database = q.database_path().to_owned();
    step_with(
        q,
        library,
        |settings, expected_generation, batch, dispatch| {
            use memex_provider::{
                credentials::Credentials,
                generation::{complete_with_optional_effort, TextRequest},
            };
            let (provider, _) = settings
                .configuration
                .target(Purpose::Translation)
                .context("no_translation_provider")?;
            anyhow::ensure!(
                credential_generation(&database, settings)? == expected_generation,
                "reader_translation_quote_stale"
            );
            let credential = Credentials::for_database(&database, &provider.id)
                .resolve(provider, expected_generation)?;
            dispatch()?;
            let response = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(complete_with_optional_effort(
                    settings,
                    &credential,
                    TextRequest {
                        purpose: Purpose::Translation,
                        system: planner::PROMPT,
                        input: batch.input(),
                        max_completion_tokens: 8192,
                    },
                ))?;
            Ok((
                response.text,
                response
                    .usage
                    .map(|u| DispatchUsage {
                        input_tokens: Some(u.prompt_tokens),
                        output_tokens: Some(u.completion_tokens),
                        total_tokens: Some(u.total_tokens),
                    })
                    .unwrap_or_default(),
            ))
        },
    )
}

/// One batch per step, with durable dispatch intent before the supplied generator.
pub fn step_with(
    q: &mut Queue,
    library: &Path,
    generate: impl FnMut(
        &Snapshot,
        u64,
        &planner::Batch,
        &mut dyn FnMut() -> Result<()>,
    ) -> Result<(String, DispatchUsage)>,
) -> Result<bool> {
    step_with_publish(q, library, generate, projection::publish_mapped)
}

fn step_with_publish(
    q: &mut Queue,
    library: &Path,
    mut generate: impl FnMut(
        &Snapshot,
        u64,
        &planner::Batch,
        &mut dyn FnMut() -> Result<()>,
    ) -> Result<(String, DispatchUsage)>,
    mut publish: impl FnMut(
        &Path,
        projection::Identity,
        projection::Provenance,
        String,
        Vec<projection::TranslationBlock>,
        Sensitivity,
    ) -> Result<projection::Metadata>,
) -> Result<bool> {
    let Some(_executor) = reader_repair::function_lock(q, reader_repair::TRANSLATION_LOCK)? else {
        return Ok(false);
    };
    let library = library.canonicalize()?;
    recover_locked(q, &library)?;
    let json: Option<String> = q.conn.query_row("SELECT payload FROM reader_translation_jobs WHERE library=?1 AND state='queued' AND COALESCE(json_extract(payload,'$.retry_at'),0)<=?2 ORDER BY job_id LIMIT 1",
        params![library.to_string_lossy(), Utc::now().timestamp_millis()], |r| r.get(0)).optional()?;
    let Some(json) = json else { return Ok(false) };
    let mut r: Record = serde_json::from_str(&json)?;
    r.job.state = "running".into();
    r.started_at = Some(Utc::now().timestamp_millis());
    put(q, &r)?;
    let run = (|| -> Result<()> {
        let source = projection::source(
            &library,
            &r.job.request.item_id,
            &r.job.request.revision_id,
            &r.job.request.chapter,
            r.ceiling,
        )?;
        let plan = planner::plan(&source.markdown, limits())?;
        anyhow::ensure!(
            source.identity == r.frozen.identity
                && plan.identity().sha256() == r.frozen.plan_sha256,
            "stale_reader_translation"
        );
        let mut validated = plan
            .batches()
            .iter()
            .zip(&r.outputs)
            .map(|(b, text)| b.validate_output(text))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        anyhow::ensure!(
            r.outputs.len() <= plan.batches().len(),
            "reader_translation_unavailable"
        );
        if r.outputs.len() < plan.batches().len() {
            r.job.stage = "translating".into();
            put(q, &r)?;
            let batch = &plan.batches()[r.outputs.len()];
            let settings = r.frozen.settings.clone();
            let expected_generation = r.frozen.credential_generation;
            let mut dispatched = false;
            let (answer, usage) = generate(&settings, expected_generation, batch, &mut || {
                anyhow::ensure!(
                    !dispatched
                        // Each automatic retry re-sends at most one batch.
                        && (r.job.usage.requests_dispatched as usize)
                            < r.job.budget.max_requests + r.auto_retries as usize,
                    "reader_translation_budget_exceeded"
                );
                let current = projection::source(
                    &library,
                    &r.job.request.item_id,
                    &r.job.request.revision_id,
                    &r.job.request.chapter,
                    r.ceiling,
                )?;
                anyhow::ensure!(
                    current.identity == r.frozen.identity
                        && memex_models::read(&q.conn)? == settings,
                    "reader_translation_quote_stale"
                );
                r.in_flight = true;
                r.job.usage.dispatched();
                put(q, &r)?;
                dispatched = true;
                Ok(())
            })?;
            anyhow::ensure!(dispatched, "reader_translation_unavailable");
            r.job.usage.observed(usage);
            r.in_flight = false;
            // Clear the durable in-flight marker only with the validated batch or
            // terminal failure below. A crash before that remains unknown, not retry.
            validated.push(batch.validate_output(&answer)?);
            r.outputs.push(answer);
            r.job.completed_blocks += batch.blocks().len();
            put(q, &r)?;
        }
        if r.outputs.len() == plan.batches().len() {
            r.job.stage = "committing".into();
            put(q, &r)?;
            let (markdown, blocks) = plan.assemble_mapped(&validated)?;
            r.job.translation = Some(publish(
                &library,
                r.frozen.identity.clone(),
                projection::Provenance {
                    route: r.frozen.quote.route.clone(),
                    prompt_version: planner::PROMPT_VERSION.into(),
                    prompt_sha256: plan.identity().prompt_sha256.clone(),
                    plan_sha256: r.frozen.plan_sha256.clone(),
                },
                markdown,
                blocks,
                r.ceiling,
            )?);
            r.job.result_revision_id = Some(r.job.request.revision_id.clone());
            r.job.state = "succeeded".into();
            r.job.stage = "done".into();
        } else {
            r.job.state = "queued".into();
        }
        Ok(())
    })();
    if let Err(error) = run {
        // A typed rejection/invalid completed response is known to have ended,
        // unlike transport loss or an interrupted durable dispatch intent.
        let known_provider_failure = error
            .downcast_ref::<memex_provider::generation::GenerationError>()
            .is_some_and(|e| {
                !matches!(
                    e,
                    memex_provider::generation::GenerationError::OutcomeUnknown
                )
            });
        let unknown = r.in_flight && !known_provider_failure;
        let reason = if unknown {
            "provider_outcome_unknown"
        } else {
            match error.to_string().as_str() {
                "library_resource_not_found" => "library_resource_not_found",
                "stale_item_revision" => "stale_item_revision",
                "stale_reader_translation" => "stale_reader_translation",
                "reader_translation_quote_stale" => "reader_translation_quote_stale",
                "reader_translation_budget_exceeded" => "reader_translation_budget_exceeded",
                _ => "reader_translation_unavailable",
            }
        };
        if r.job.stage == "committing" && error.downcast_ref::<std::io::Error>().is_some() {
            // Publication may already be visible. Keep the complete outputs and
            // retry only immutable publication, never another model request.
            r.job.state = "queued".into();
            r.retry_at = Utc::now().timestamp_millis() + 30_000;
        } else if (unknown || reason == "reader_translation_unavailable")
            && crate::queue::auto_retryable(&error)
            && retry_later(&mut r)
        {
            // Bounded automatic retry of the model step (validated batches are kept).
        } else {
            r.in_flight = unknown;
            finish_error(&mut r, unknown, reason);
            // error_code keeps the generic code clients already know; the typed site
            // (never provider prose) stays in failure_reason for diagnosis.
            if !unknown && reason == "reader_translation_unavailable" {
                if let Some(detail) = error.downcast_ref::<planner::Error>() {
                    r.job.failure_reason = Some(detail.to_string());
                } else if known_provider_failure {
                    r.job.failure_reason = Some("provider_failure".into());
                }
            }
        }
    }
    if let Some(start) = r.started_at.take() {
        r.job.elapsed_ms = r
            .job
            .elapsed_ms
            .saturating_add(Utc::now().timestamp_millis().saturating_sub(start).max(0) as u64);
    }
    put(q, &r)?;
    // A pending publication error must yield to the daemon's idle/backoff wait.
    Ok(!(r.job.state == "queued" && r.job.stage == "committing"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (tempfile::TempDir, Queue, PathBuf, Selection) {
        let tmp = tempfile::tempdir().unwrap();
        let library = tmp.path().join("Library");
        let doc = super::super::book::seed(&library).unwrap();
        let mut q = Queue::open(&tmp.path().join("queue.db")).unwrap();
        memex_models::replace(
            &mut q.conn,
            0,
            memex_models::Configuration {
                providers: vec![memex_models::Provider {
                    id: "fixture".into(),
                    label: "Synthetic".into(),
                    api_base: "https://model.example.invalid/v1/".into(),
                }],
                models: vec![memex_models::Model {
                    id: "translation".into(),
                    label: "Synthetic".into(),
                    provider_id: "fixture".into(),
                    model_id: "fixture-model".into(),
                }],
                selections: vec![memex_models::Selection {
                    purpose: Purpose::Translation,
                    model: "translation".into(),
                    reasoning_effort: None,
                }],
            },
        )
        .unwrap();
        (
            tmp,
            q,
            library,
            Selection {
                item_id: doc.item_id,
                revision_id: doc.revision_id,
                chapter: "appendix".into(),
            },
        )
    }
    fn admitted(q: &mut Queue, library: &Path, selection: &Selection, id: &str) -> Job {
        let quote = preview_with(
            q,
            library,
            selection.clone(),
            Sensitivity::Public,
            100,
            |_, _| Ok(7),
        )
        .unwrap()
        .quote
        .unwrap();
        request_with(
            q,
            library,
            Request {
                item_id: selection.item_id.clone(),
                revision_id: selection.revision_id.clone(),
                chapter: selection.chapter.clone(),
                request_id: id.into(),
                quote_id: quote.quote_id,
                accept_cost: true,
                expected_translation_version: "missing".into(),
            },
            Sensitivity::Public,
            101,
            |_, _| Ok(7),
        )
        .unwrap()
    }
    fn answer(batch: &planner::Batch) -> String {
        let input: serde_json::Value = serde_json::from_str(batch.input()).unwrap();
        serde_json::json!({ "source_sha256": input["source_sha256"], "batch_id": batch.batch_id(),
            "blocks": batch.blocks().iter().map(|b| serde_json::json!({"id": b.id, "translation": "合成译文"})).collect::<Vec<_>>() }).to_string()
    }
    fn usage() -> DispatchUsage {
        DispatchUsage {
            input_tokens: Some(11),
            output_tokens: Some(3),
            total_tokens: Some(17),
        }
    }

    #[test]
    fn one_click_request_without_a_quote_runs_in_one_step() {
        let (_tmp, mut q, library, selection) = setup();
        // An old-style body still parses; a new client omits quote_id and accept_cost.
        let request: Request = serde_json::from_value(serde_json::json!({
            "item_id": selection.item_id, "revision_id": selection.revision_id,
            "chapter": selection.chapter, "request_id": "one-click",
            "expected_translation_version": "missing"
        }))
        .unwrap();
        assert!(request.quote_id.is_empty() && !request.accept_cost);
        let job = request_with(&mut q, &library, request.clone(), Sensitivity::Public, 101, |_, _| Ok(7)).unwrap();
        assert_eq!(job.state, "queued");
        assert!(step_with(&mut q, &library, |_, _, batch, dispatch| {
            dispatch()?;
            Ok((answer(batch), usage()))
        })
        .unwrap());
        let done = status(&q, &library, &selection.item_id, &job.job_id, Sensitivity::Public).unwrap();
        assert_eq!(done.state, "succeeded");
        // Replay is idempotent and dispatches nothing.
        let replay = request_with(&mut q, &library, request, Sensitivity::Public, 9999999, |_, _| panic!("replay credential read")).unwrap();
        assert_eq!(replay.job_id, job.job_id);
    }

    #[test]
    fn invalid_model_output_keeps_the_generic_code_and_a_typed_reason() {
        let (_tmp, mut q, library, selection) = setup();
        let job = admitted(&mut q, &library, &selection, "invalid-reason");
        crate::queue::TEST_RETRY_DELAY_MS.with(|d| d.set(Some(0)));
        let mut calls = 0;
        while step_with(&mut q, &library, |_, _, _, dispatch| {
            dispatch()?;
            calls += 1;
            Ok(("{}".into(), usage()))
        })
        .unwrap()
        {}
        // Malformed output is retried automatically, at most AUTO_RETRIES times.
        assert_eq!(calls, 1 + crate::queue::AUTO_RETRIES as usize);
        let result = status(&q, &library, &selection.item_id, &job.job_id, Sensitivity::Public).unwrap();
        assert_eq!(result.state, "failed");
        assert_eq!(result.error_code.as_deref(), Some("reader_translation_unavailable"));
        assert_eq!(result.failure_reason.as_deref(), Some("reader_translation_invalid_output"));
        assert_eq!(result.failure_stage.as_deref(), Some("translating"));
    }

    #[test]
    fn chapter_translation_runs_while_repair_and_format_executors_are_busy() {
        let (_tmp, mut q, library, selection) = setup();
        let job = admitted(&mut q, &library, &selection, "concurrent");
        // Another function's executor holds its own lock (as a running repair or
        // format job does); chapter translation must not queue behind it.
        let _repair = reader_repair::execution_lock(&q).unwrap().expect("repair lock");
        let _format = reader_repair::function_lock(&q, reader_repair::FORMAT_LOCK)
            .unwrap()
            .expect("format lock");
        // The same function still has a single executor.
        let held = reader_repair::function_lock(&q, reader_repair::TRANSLATION_LOCK)
            .unwrap()
            .expect("translation lock");
        assert!(!step_with(&mut q, &library, |_, _, _, _| panic!("second translation executor")).unwrap());
        drop(held);
        assert!(step_with(&mut q, &library, |_, _, batch, dispatch| {
            dispatch()?;
            Ok((answer(batch), usage()))
        })
        .unwrap());
        let result = status(&q, &library, &selection.item_id, &job.job_id, Sensitivity::Public).unwrap();
        assert_eq!(result.state, "succeeded");
    }

    #[test]
    fn terminal_replay_unknown_and_cache_do_not_dispatch_again() {
        for outcome in ["success", "invalid", "unknown"] {
            let (_tmp, mut q, library, selection) = setup();
            let before = memex_store::read_item(&library, &selection.item_id)
                .unwrap()
                .1;
            let job = admitted(&mut q, &library, &selection, "first");
            assert!(reader_item_busy(
                &q.conn,
                &library.canonicalize().unwrap(),
                &selection.item_id
            )
            .unwrap());
            let mut count = 0;
            // Invalid/unknown model steps are retried automatically, at most AUTO_RETRIES times.
            crate::queue::TEST_RETRY_DELAY_MS.with(|d| d.set(Some(0)));
            while step_with(&mut q, &library, |_, generation, batch, dispatch| {
                assert_eq!(generation, 7);
                dispatch()?;
                count += 1;
                if outcome == "unknown" {
                    bail!("synthetic transport lost")
                }
                Ok((
                    if outcome == "invalid" {
                        "{}".into()
                    } else {
                        answer(batch)
                    },
                    usage(),
                ))
            })
            .unwrap()
            {}
            let attempts: u64 = if outcome == "success" { 1 } else { 1 + crate::queue::AUTO_RETRIES as u64 };
            let result = status(
                &q,
                &library,
                &selection.item_id,
                &job.job_id,
                Sensitivity::Public,
            )
            .unwrap();
            assert_eq!(
                result.state,
                match outcome {
                    "success" => "succeeded",
                    "invalid" => "failed",
                    _ => "outcome_unknown",
                }
            );
            assert_eq!(result.usage.requests_dispatched as u64, attempts);
            assert_eq!(
                result.usage.reported_total_tokens,
                if outcome == "unknown" { 0 } else { 17 * attempts }
            );
            assert_eq!(
                result.usage.input_tokens,
                if outcome == "unknown" { None } else { Some(11 * attempts) }
            );
            assert_eq!(count as u64, attempts);
            assert!(!step_with(&mut q, &library, |_, _, _, _| panic!("terminal retry")).unwrap());
            let replay = request_with(
                &mut q,
                &library,
                job.request.clone(),
                Sensitivity::Public,
                9999999,
                |_, _| panic!("replay credential read"),
            )
            .unwrap();
            assert_eq!(replay.job_id, job.job_id);
            assert_eq!(
                memex_store::read_item(&library, &selection.item_id)
                    .unwrap()
                    .1,
                before
            );
            if outcome == "success" {
                let mut new = job.request.clone();
                new.request_id = "cache-reuse".into();
                let reused = request_with(
                    &mut q,
                    &library,
                    new,
                    Sensitivity::Public,
                    9999999,
                    |_, _| panic!("cached credential read"),
                )
                .unwrap();
                assert_eq!(reused.state, "succeeded");
                assert_eq!(reused.translation, result.translation);
                assert_eq!(reused.usage.requests_dispatched, 0);
            } else {
                assert!(projection::metadata(
                    &library,
                    &selection.item_id,
                    &selection.revision_id,
                    &selection.chapter,
                    Sensitivity::Public
                )
                .unwrap()
                .is_none());
            }
        }
    }

    #[test]
    fn interrupted_dispatch_is_retried_automatically_and_before_dispatch_can_resume() {
        crate::queue::TEST_RETRY_DELAY_MS.with(|d| d.set(Some(0)));
        for dispatch_first in [false, true] {
            let (_tmp, mut q, library, selection) = setup();
            let job = admitted(&mut q, &library, &selection, "crash");
            let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = step_with(&mut q, &library, |_, _, _, dispatch| {
                    if dispatch_first {
                        dispatch()?;
                    }
                    panic!("synthetic interruption");
                });
            }));
            assert!(interrupted.is_err());
            let db = q.database_path().to_owned();
            drop(q);
            let mut q = Queue::open(&db).unwrap();
            // An interrupted dispatch is an uncertain outcome; on the owner's subscription
            // route it is resent once automatically (bounded by AUTO_RETRIES).
            let worked = step_with(&mut q, &library, |_, _, batch, dispatch| {
                dispatch()?;
                Ok((answer(batch), usage()))
            })
            .unwrap();
            assert!(worked);
            let result = status(
                &q,
                &library,
                &selection.item_id,
                &job.job_id,
                Sensitivity::Public,
            )
            .unwrap();
            assert_eq!(result.state, "succeeded");
            assert_eq!(result.usage.requests_dispatched as u64, 1 + u64::from(dispatch_first));
        }
    }

    #[test]
    fn explicit_retry_reuses_validated_prefix_not_failed_batch_costs() {
        for rejected in [false, true] {
            let (_tmp, mut q, library, mut selection) = setup();
            use memex_store::reader::{chapter_edit as edit, reader_sha256};
            let old = projection::source(
                &library,
                &selection.item_id,
                &selection.revision_id,
                &selection.chapter,
                Sensitivity::Public,
            )
            .unwrap();
            let markdown = format!("{}\n\n{}", "A".repeat(20000), "B".repeat(20000));
            let upload = edit::begin(
                &library,
                edit::Begin {
                    item_id: selection.item_id.clone(),
                    revision_id: selection.revision_id.clone(),
                    side: "original".into(),
                    chapter: selection.chapter.clone(),
                    expected_chapter_sha256: old.identity.source_sha256,
                    request_id: "long-source".into(),
                    bytes: markdown.len(),
                    sha256: reader_sha256(markdown.as_bytes()),
                },
                "local_owner",
                Sensitivity::Public,
            )
            .unwrap();
            for offset in (0..markdown.len()).step_by(8192) {
                edit::chunk(
                    &library,
                    &selection.item_id,
                    &upload.edit_id,
                    offset,
                    &markdown[offset..(offset + 8192).min(markdown.len())],
                    "local_owner",
                    Sensitivity::Public,
                )
                .unwrap();
            }
            selection.revision_id = edit::commit(
                &library,
                &selection.item_id,
                &upload.edit_id,
                "local_owner",
                Sensitivity::Public,
            )
            .unwrap()
            .revision_id;
            let job = admitted(&mut q, &library, &selection, "two-batches");
            assert_eq!(job.budget.max_requests, 2);
            step_with(&mut q, &library, |_, _, batch, dispatch| {
                dispatch()?;
                Ok((answer(batch), usage()))
            })
            .unwrap();
            crate::queue::TEST_RETRY_DELAY_MS.with(|d| d.set(Some(0)));
            while step_with(&mut q, &library, |_, _, _, dispatch| {
                dispatch()?;
                if rejected {
                    return Err(memex_provider::generation::GenerationError::Rejected.into());
                }
                Ok(("{}".into(), usage()))
            })
            .unwrap()
            {}
            let attempts = 1 + crate::queue::AUTO_RETRIES as u64;
            let failed = status(
                &q,
                &library,
                &selection.item_id,
                &job.job_id,
                Sensitivity::Public,
            )
            .unwrap();
            assert_eq!(failed.state, "failed");
            assert_eq!(failed.completed_blocks, 1);
            // A typed rejection is final; an invalid reply on the second batch is retried
            // automatically (bounded); the validated first batch is never resent.
            let second = if rejected { 1 } else { attempts };
            let reported = if rejected { 17 } else { 17 + 17 * attempts };
            assert_eq!(failed.usage.reported_total_tokens, reported);
            assert_eq!(failed.usage.requests_dispatched as u64, 1 + second);
            assert_eq!(
                failed.usage.input_tokens,
                if rejected { None } else { Some(11 + 11 * attempts) }
            );
            assert!(!step_with(&mut q, &library, |_, _, _, _| panic!("automatic retry")).unwrap());
            assert!(projection::metadata(
                &library,
                &selection.item_id,
                &selection.revision_id,
                &selection.chapter,
                Sensitivity::Public
            )
            .unwrap()
            .is_none());
            let retry = admitted(&mut q, &library, &selection, "explicit-retry");
            assert_eq!(retry.completed_blocks, 1);
            assert_eq!(retry.usage.requests_dispatched, 0);
            step_with(&mut q, &library, |_, _, batch, dispatch| {
                assert_eq!(batch.blocks()[0].id, "block-1");
                dispatch()?;
                Ok((answer(batch), usage()))
            })
            .unwrap();
            let done = status(
                &q,
                &library,
                &selection.item_id,
                &retry.job_id,
                Sensitivity::Public,
            )
            .unwrap();
            assert_eq!(done.state, "succeeded");
            assert_eq!(done.usage.requests_dispatched, 1);
            assert_eq!(done.completed_blocks, 2);
            assert_eq!(
                status(
                    &q,
                    &library,
                    &selection.item_id,
                    &job.job_id,
                    Sensitivity::Public
                )
                .unwrap()
                .usage
                .reported_total_tokens,
                reported
            );
        }
    }

    #[test]
    fn active_translation_allows_upload_but_blocks_commit_then_edit_stales_projection() {
        use memex_store::reader::{chapter_edit as edit, reader_sha256};
        let (_tmp, mut q, library, selection) = setup();
        let source = projection::source(
            &library,
            &selection.item_id,
            &selection.revision_id,
            &selection.chapter,
            Sensitivity::Public,
        )
        .unwrap();
        let job = admitted(&mut q, &library, &selection, "translation-before-edit");
        let markdown = "# Appendix\n\nOwner adds a new sentence. 原文🙂\n";
        let upload = edit::begin(
            &library,
            edit::Begin {
                item_id: selection.item_id.clone(),
                revision_id: selection.revision_id.clone(),
                side: "original".into(),
                chapter: selection.chapter.clone(),
                expected_chapter_sha256: source.identity.source_sha256,
                request_id: "edit-during-translation".into(),
                bytes: markdown.len(),
                sha256: reader_sha256(markdown.as_bytes()),
            },
            "local_owner",
            Sensitivity::Public,
        )
        .unwrap();
        edit::chunk(
            &library,
            &selection.item_id,
            &upload.edit_id,
            0,
            markdown,
            "local_owner",
            Sensitivity::Public,
        )
        .unwrap();
        assert_eq!(
            reader_repair::commit_edit(
                &mut q,
                &library,
                &selection.item_id,
                &upload.edit_id,
                "local_owner",
                Sensitivity::Public
            )
            .unwrap_err()
            .to_string(),
            "reader_job_in_progress"
        );
        assert!(step_with(&mut q, &library, |_, _, batch, dispatch| {
            dispatch()?;
            Ok((answer(batch), usage()))
        })
        .unwrap());
        let translated = status(
            &q,
            &library,
            &selection.item_id,
            &job.job_id,
            Sensitivity::Public,
        )
        .unwrap();
        assert_eq!(translated.state, "succeeded");
        let projection = translated.translation.unwrap();
        let receipt = reader_repair::commit_edit(
            &mut q,
            &library,
            &selection.item_id,
            &upload.edit_id,
            "local_owner",
            Sensitivity::Public,
        )
        .unwrap();
        assert_ne!(receipt.revision_id, selection.revision_id);
        assert_eq!(
            reader_repair::commit_edit(
                &mut q,
                &library,
                &selection.item_id,
                &upload.edit_id,
                "local_owner",
                Sensitivity::Public
            )
            .unwrap(),
            receipt
        );
        assert!(projection::chunk(
            &library,
            &selection.item_id,
            &selection.revision_id,
            &selection.chapter,
            &projection.version,
            0,
            32768,
            Sensitivity::Public
        )
        .is_err());
        assert!(projection::metadata(
            &library,
            &selection.item_id,
            &receipt.revision_id,
            &selection.chapter,
            Sensitivity::Public
        )
        .unwrap()
        .is_none());
        assert!(!step_with(&mut q, &library, |_, _, _, _| panic!(
            "edit retranslates automatically"
        ))
        .unwrap());
        assert_eq!(
            status(
                &q,
                &library,
                &selection.item_id,
                &job.job_id,
                Sensitivity::Public
            )
            .unwrap()
            .usage
            .requests_dispatched,
            1
        );
    }

    #[test]
    fn settings_drift_after_admission_rejects_before_dispatch() {
        let (_tmp, mut q, library, selection) = setup();
        let job = admitted(&mut q, &library, &selection, "settings-drift");
        let mut changed = memex_models::read(&q.conn).unwrap();
        changed.configuration.models[0].model_id = "another-model".into();
        memex_models::replace(&mut q.conn, changed.revision, changed.configuration).unwrap();
        assert!(step_with(&mut q, &library, |_, _, _, dispatch| {
            dispatch()?;
            panic!("stale route reached paid work");
        })
        .unwrap());
        let failed = status(
            &q,
            &library,
            &selection.item_id,
            &job.job_id,
            Sensitivity::Public,
        )
        .unwrap();
        assert_eq!(failed.state, "failed");
        assert_eq!(
            failed.error_code.as_deref(),
            Some("reader_translation_quote_stale")
        );
        assert_eq!(failed.usage.requests_dispatched, 0);
        assert_eq!(failed.usage.dispatches.as_ref().map(Vec::len), Some(0));
        assert_eq!(failed.usage.input_tokens, Some(0));
        assert_eq!(failed.usage.output_tokens, Some(0));
    }

    #[test]
    fn pending_publication_yields_to_another_item() {
        let (_tmp, mut q, library, selection) = setup();
        let first = admitted(&mut q, &library, &selection, "blocked-publication");
        assert!(!step_with_publish(
            &mut q,
            &library,
            |_, _, batch, dispatch| {
                dispatch()?;
                Ok((answer(batch), usage()))
            },
            |_, _, _, _, _, _| Err(
                std::io::Error::from(std::io::ErrorKind::PermissionDenied).into()
            )
        )
        .unwrap());
        let other = super::super::book::seed_named(&library, "another-book").unwrap();
        assert_ne!(other.item_id, selection.item_id);
        let second_selection = Selection {
            item_id: other.item_id,
            revision_id: other.revision_id,
            chapter: "appendix".into(),
        };
        let second = admitted(&mut q, &library, &second_selection, "unblocked-publication");
        assert!(step_with(&mut q, &library, |_, _, batch, dispatch| {
            dispatch()?;
            Ok((answer(batch), usage()))
        })
        .unwrap());
        assert_eq!(
            status(
                &q,
                &library,
                &second_selection.item_id,
                &second.job_id,
                Sensitivity::Public
            )
            .unwrap()
            .state,
            "succeeded"
        );
        let pending = status(
            &q,
            &library,
            &selection.item_id,
            &first.job_id,
            Sensitivity::Public,
        )
        .unwrap();
        assert_eq!(pending.state, "queued");
        assert_eq!(pending.usage.requests_dispatched, 1);
    }

    #[test]
    fn visible_publication_recovers_original_receipt_without_generation() {
        for crash in [false, true] {
            let (_tmp, mut q, library, selection) = setup();
            let job = admitted(&mut q, &library, &selection, "publish-window");
            let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                step_with_publish(
                    &mut q,
                    &library,
                    |_, _, batch, dispatch| {
                        dispatch()?;
                        Ok((answer(batch), usage()))
                    },
                    |root, identity, provenance, markdown, blocks, ceiling| {
                        projection::publish_mapped(
                            root, identity, provenance, markdown, blocks, ceiling,
                        )?;
                        if crash {
                            panic!("after publication before receipt");
                        }
                        Err(
                            std::io::Error::other("synthetic post-install directory sync failure")
                                .into(),
                        )
                    },
                )
            }));
            if crash {
                assert!(attempt.is_err());
            } else {
                assert!(!attempt.unwrap().unwrap());
                let pending = status(
                    &q,
                    &library,
                    &selection.item_id,
                    &job.job_id,
                    Sensitivity::Public,
                )
                .unwrap();
                assert_eq!(pending.state, "queued");
                assert_eq!(pending.stage, "committing");
                assert!(pending.failure_reason.is_none());
            }
            let installed = projection::metadata(
                &library,
                &selection.item_id,
                &selection.revision_id,
                &selection.chapter,
                Sensitivity::Public,
            )
            .unwrap()
            .unwrap();
            let db = q.database_path().to_owned();
            drop(q);
            let mut q = Queue::open(&db).unwrap();
            if !crash {
                assert!(
                    !step_with(&mut q, &library, |_, _, _, _| panic!("backoff dispatched"))
                        .unwrap()
                );
                // Advance only this synthetic job's persisted deadline, not a sleep.
                q.conn.execute("UPDATE reader_translation_jobs SET payload=json_set(payload,'$.retry_at',0) WHERE job_id=?1", [&job.job_id]).unwrap();
            }
            assert!(step_with(&mut q, &library, |_, _, _, _| panic!(
                "publication recovery billed again"
            ))
            .unwrap());
            let receipt = request_with(
                &mut q,
                &library,
                job.request,
                Sensitivity::Public,
                9999999,
                |_, _| panic!("receipt resolved credentials"),
            )
            .unwrap();
            assert_eq!(receipt.job_id, job.job_id);
            assert_eq!(receipt.state, "succeeded");
            assert_eq!(receipt.translation, Some(installed));
            assert_eq!(receipt.usage.requests_dispatched, 1);
            assert_eq!(receipt.usage.input_tokens, Some(11));
            assert_eq!(receipt.usage.output_tokens, Some(3));
            assert_eq!(receipt.usage.reported_total_tokens, 17);
        }
    }
}
