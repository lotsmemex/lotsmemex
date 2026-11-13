//! Durable, explicit selected-chapter jobs. A failed, invalid or uncertain model step of a
//! single-chapter job is retried automatically at most `AUTO_RETRIES` times with backoff.
use super::*;
use memex_core::Sensitivity;
use memex_models::{low_dial::Profile, Purpose, Selection, Snapshot};
use memex_store::reader::{self, repair::Plan};
use std::time::Instant;

pub(super) mod matching;
pub mod batch;
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub item_id: String,
    pub revision_id: String,
    pub chapter: String,
    pub request_id: String,
    pub expected_alignment_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Budget {
    pub max_requests: u8,
    pub max_input_bytes_per_request: usize,
    pub max_completion_tokens_per_request: u32,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DispatchUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub requests_dispatched: u8,
    pub reported_total_tokens: u64,
    pub unknown_usage_requests: u8,
    #[serde(default)]
    pub dispatches: Option<Vec<DispatchUsage>>,
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
}

impl Usage {
    pub(super) fn dispatched(&mut self) {
        self.requests_dispatched += 1;
        self.unknown_usage_requests += 1;
        if let Some(dispatches) = &mut self.dispatches {
            dispatches.push(DispatchUsage::default());
        }
        self.input_tokens = None;
        self.output_tokens = None;
    }

    pub(super) fn observed(&mut self, mut usage: DispatchUsage) {
        const SAFE: u64 = (1 << 53) - 1;
        usage.input_tokens = usage.input_tokens.filter(|n| *n <= SAFE);
        usage.output_tokens = usage.output_tokens.filter(|n| *n <= SAFE);
        usage.total_tokens = usage.total_tokens.filter(|n| *n <= SAFE);
        if let Some(tokens) = usage.total_tokens {
            self.reported_total_tokens += tokens;
            self.unknown_usage_requests -= 1;
        }
        if let Some(dispatches) = &mut self.dispatches {
            if let Some(last) = dispatches.last_mut() { *last = usage; }
            self.input_tokens = dispatches.iter().try_fold(0u64, |sum, d| sum.checked_add(d.input_tokens?)).filter(|n| *n <= SAFE);
            self.output_tokens = dispatches.iter().try_fold(0u64, |sum, d| sum.checked_add(d.output_tokens?)).filter(|n| *n <= SAFE);
        }
    }
}
fn repair_operation() -> String { "repair".into() }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub job_id: String,
    #[serde(default = "repair_operation")]
    pub operation: String,
    #[serde(default)]
    pub failure_stage: Option<String>,
    #[serde(default)]
    pub failure_reason: Option<String>,
    #[serde(flatten)]
    pub request: Request,
    pub state: String,
    pub stage: String,
    pub result_revision_id: Option<String>,
    pub alignment_version: Option<String>,
    pub alignment_status: Option<String>,
    pub error_code: Option<String>,
    pub budget: Budget,
    pub usage: Usage,
    pub elapsed_ms: u64,
}
#[derive(Clone, Serialize, Deserialize)]
struct Record {
    // Separate storage, never a field ignored inside the old executor's table.
    #[serde(default)]
    batch_id: Option<String>,
    #[serde(default)]
    dispatch_in_flight: bool,
    job: Job,
    library: PathBuf,
    ceiling: Sensitivity,
    settings: Snapshot,
    profile: Profile,
    plan: Plan,
    apply: bool,
    started_at_ms: Option<i64>,
    // Local diagnostic codes only: never persist provider prose or source text.
    #[serde(default)]
    failure_stage: Option<String>,
    #[serde(default)]
    failure_reason: Option<String>,
    #[serde(default)]
    rejected_groups: Vec<String>,
    /// matching::PARSER_POLICY at admission; absent (0) for older jobs.
    #[serde(default)]
    parser_policy: u32,
    #[serde(default)]
    auto_retries: u32,
    #[serde(default)]
    retry_at: i64,
}

fn put(q: &Queue, r: &Record) -> Result<()> {
    if r.batch_id.is_some() {
        return batch::put_child(q, r);
    }
    q.conn.execute(
        "UPDATE reader_repair_jobs SET state=?1,payload=?2 WHERE job_id=?3",
        params![r.job.state, serde_json::to_string(r)?, r.job.job_id],
    )?;
    Ok(())
}
fn load(q: &Queue, id: &str) -> Result<Record> {
    let json: String = q
        .conn
        .query_row(
            "SELECT payload FROM reader_repair_jobs WHERE job_id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()?
        .context("reader_job_not_found")?;
    Ok(serde_json::from_str(&json)?)
}
fn permitted(library: &Path, item: &str, ceiling: Sensitivity) -> Result<()> {
    anyhow::ensure!(
        memex_store::read_item_at_sensitivity(library, item, ceiling)?.is_some(),
        "library_resource_not_found"
    );
    Ok(())
}

/// Per-function executor locks. Each reader function (repair, chapter translation,
/// content format) has its own lock so different functions never queue behind each
/// other; per-item uniqueness across all of them is enforced at admission by
/// `reader_item_busy` inside the IMMEDIATE queue transaction, not by these locks.
pub(super) const REPAIR_LOCK: &str = "reader-repair.lock";
pub(super) const TRANSLATION_LOCK: &str = "reader-translation.lock";
pub(super) const FORMAT_LOCK: &str = "content-format.lock";

// Repair (and repair batches): shared by CLI, daemon execution and recovery.
pub(super) fn execution_lock(q: &Queue) -> Result<Option<std::fs::File>> {
    function_lock(q, REPAIR_LOCK)
}

// Shared by CLI, daemon execution and recovery of one function. Socket ownership
// alone cannot prove a CLI executor has stopped. The OS releases it on process exit.
pub(super) fn function_lock(q: &Queue, name: &str) -> Result<Option<std::fs::File>> {
    use std::os::unix::fs::OpenOptionsExt;
    let path = q.database_path().canonicalize()?.with_extension(name);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    // Tests run in parallel threads while others fork children; a child holds an inherited
    // lock descriptor until it execs. Wait that window out in tests only; production keeps
    // the immediate answer (a real holder is another executor).
    let attempts = if cfg!(test) { 200 } else { 1 };
    for attempt in 1..=attempts {
        match file.try_lock() {
            Ok(()) => return Ok(Some(file)),
            Err(std::fs::TryLockError::WouldBlock) if attempt < attempts => std::thread::sleep(std::time::Duration::from_millis(10)),
            Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
        }
    }
    unreachable!()
}

fn replay(
    conn: &Connection,
    library: &Path,
    request: &Request,
    apply: bool,
) -> Result<Option<Job>> {
    let previous: Option<String> = conn
        .query_row(
            "SELECT payload FROM reader_repair_jobs WHERE request_id=?1",
            [&request.request_id],
            |r| r.get(0),
        )
        .optional()?;
    previous
        .map(|json| {
            let previous: Record = serde_json::from_str(&json)?;
            anyhow::ensure!(
                previous.job.request == *request
                    && previous.library == library
                    && previous.apply == apply,
                "request_identity_conflict"
            );
            Ok(previous.job)
        })
        .transpose()
}

pub fn commit_edit(q: &mut Queue, library: &Path, item: &str, edit: &str, principal: &str,
    ceiling: Sensitivity) -> Result<reader::chapter_edit::Receipt> {
    let library = library.canonicalize()?;
    // Admission and commit share the short queue writer transaction. Uploads do
    // not reserve an item, and a completed replay never competes with a new job.
    let tx = q.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(receipt) = reader::chapter_edit::committed(&library, item, edit, principal, ceiling)? {
        return Ok(receipt);
    }
    let active = reader_item_busy(&tx, &library, item)?;
    anyhow::ensure!(!active, "reader_job_in_progress");
    let receipt = reader::chapter_edit::commit(&library, item, edit, principal, ceiling)?;
    tx.commit()?;
    Ok(receipt)
}

pub fn request(
    q: &mut Queue,
    library: &Path,
    request: Request,
    ceiling: Sensitivity,
    apply: bool,
) -> Result<Job> {
    anyhow::ensure!(
        (1..=128).contains(&request.request_id.len())
            && request
                .request_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
        "invalid_reader_alignment_request"
    );
    let library = library.canonicalize()?;
    permitted(&library, &request.item_id, ceiling)?;
    if let Some(job) = replay(&q.conn, &library, &request, apply)? {
        return Ok(job);
    }
    // Store snapshot/partition work never holds the queue's SQLite writer lock.
    let plan = reader::repair::plan(
        &library,
        &request.item_id,
        &request.revision_id,
        &request.chapter,
        ceiling,
    )?;
    anyhow::ensure!(
        plan.expected_alignment_version == request.expected_alignment_version,
        "stale_chapter_alignment"
    );
    let tx = q
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(job) = replay(&tx, &library, &request, apply)? {
        return Ok(job);
    }
    let active = reader_item_busy(&tx, &library, &request.item_id)?;
    anyhow::ensure!(!active, "reader_job_in_progress");
    let settings = memex_models::read(&tx)?;
    let profile = memex_models::low_dial::read(&tx)?.context("no_low_dial_provider")?;
    let mut routed = settings.clone();
    routed
        .configuration
        .selections
        .retain(|s| s.purpose != Purpose::CaptureQuality);
    routed.configuration.selections.push(Selection {
        purpose: Purpose::CaptureQuality,
        model: profile.model.clone(),
        reasoning_effort: profile.reasoning_effort,
    });
    routed
        .configuration
        .validate()
        .map_err(|_| anyhow::anyhow!("no_low_dial_provider"))?;
    anyhow::ensure!(
        routed
            .configuration
            .target(Purpose::CaptureQuality)
            .is_some(),
        "no_low_dial_provider"
    );
    let record = Record {
        batch_id: None,
        dispatch_in_flight: false,
        job: Job {
            job_id: format!("job_{}", uuid::Uuid::now_v7().simple()),
            operation: repair_operation(),
            failure_stage: None,
            failure_reason: None,
            request,
            state: "queued".into(),
            stage: "queued".into(),
            result_revision_id: None,
            alignment_version: None,
            alignment_status: None,
            error_code: None,
            budget: Budget {
                max_requests: 2,
                max_input_bytes_per_request: profile.max_input_bytes,
                max_completion_tokens_per_request: profile.max_completion_tokens,
            },
            usage: Usage { dispatches: Some(Vec::new()), input_tokens: Some(0), output_tokens: Some(0), ..Usage::default() },
            elapsed_ms: 0,
        },
        library,
        ceiling,
        settings,
        profile,
        plan,
        apply,
        started_at_ms: None,
        failure_stage: None,
        failure_reason: None,
        rejected_groups: Vec::new(),
        parser_policy: matching::PARSER_POLICY,
        auto_retries: 0,
        retry_at: 0,
    };
    tx.execute("INSERT INTO reader_repair_jobs(job_id,request_id,library,item_id,state,payload) VALUES(?1,?2,?3,?4,?5,?6)",
        params![record.job.job_id, record.job.request.request_id, record.library.to_string_lossy(), record.job.request.item_id,
            record.job.state, serde_json::to_string(&record)?])?;
    tx.commit()?;
    Ok(record.job)
}

pub fn status(
    q: &Queue,
    library: &Path,
    item: &str,
    job: &str,
    ceiling: Sensitivity,
) -> Result<Job> {
    permitted(library, item, ceiling)?;
    let r = load(q, job)?;
    anyhow::ensure!(
        r.library == library.canonicalize()? && r.job.request.item_id == item,
        "reader_job_not_found"
    );
    Ok(r.job)
}

/// CLI planning uses the same bounded execution, but never commits to the Library.
pub fn planned(
    q: &Queue,
    library: &Path,
    item: &str,
    job: &str,
    ceiling: Sensitivity,
) -> Result<Plan> {
    let job = status(q, library, item, job, ceiling)?;
    let r = load(q, &job.job_id)?;
    anyhow::ensure!(
        !r.apply && r.job.state == "succeeded",
        "reader_repair_failed"
    );
    Ok(r.plan)
}

/// Only after the daemon has acquired its exclusive owner socket. Unknown model
/// work is terminal; a persisted commit plan can replay the Store transaction.
pub fn recover(q: &mut Queue, library: &Path) -> Result<()> {
    let Some(_execution) = execution_lock(q)? else {
        return Ok(());
    };
    recover_locked(q, library)
}

fn recover_locked(q: &Queue, library: &Path) -> Result<()> {
    let library = library.canonicalize()?;
    let mut statement = q
        .conn
        .prepare("SELECT payload FROM reader_repair_jobs WHERE library=?1 AND state='running'")?;
    let rows = statement
        .query_map([library.to_string_lossy()], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    for row in rows {
        let mut r: Record = serde_json::from_str(&row)?;
        if let Some(started) = r.started_at_ms.take() {
            r.job.elapsed_ms =
                r.job.elapsed_ms.saturating_add(
                    Utc::now().timestamp_millis().saturating_sub(started).max(0) as u64,
                );
        }
        if r.job.stage == "committing" || r.job.usage.requests_dispatched == 0 {
            r.job.state = "queued".into();
        } else {
            r.job.state = "outcome_unknown".into();
            r.job.failure_stage = Some(r.job.stage.clone());
            r.job.failure_reason = Some("provider_outcome_unknown".into());
            r.job.stage = "done".into();
            r.job.error_code = Some("provider_outcome_unknown".into());
        }
        put(q, &r)?;
    }
    Ok(())
}

/// One claimed job; the optional ID lets CLI operate only its explicit request.
pub fn step(q: &mut Queue, library: &Path, job_id: Option<&str>) -> Result<bool> {
    step_with_usage(
        q,
        library,
        job_id,
        |database, snapshot, profile, input, dispatch| {
            use memex_provider::{
                credentials::Credentials,
                generation::{complete_with_optional_effort, TextRequest},
            };
            let (provider, _) = snapshot
                .configuration
                .target(Purpose::CaptureQuality)
                .context("no_low_dial_provider")?;
            let vault = Credentials::for_database(database, &provider.id);
            let state = vault.status(provider, snapshot.revision)?;
            let credential = vault.resolve(provider, state.generation)?;
            dispatch()?;
            let response = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(complete_with_optional_effort(
                    snapshot,
                    &credential,
                    TextRequest {
                        purpose: Purpose::CaptureQuality,
                        system: matching::PROMPT,
                        input,
                        max_completion_tokens: profile.max_completion_tokens,
                    },
                ))?;
            Ok((response.text, response.usage.map(|u| DispatchUsage {
                input_tokens: Some(u.prompt_tokens), output_tokens: Some(u.completion_tokens), total_tokens: Some(u.total_tokens),
            }).unwrap_or_default()))
        },
    )
}

/// The same durable worker with an explicit generator, for synthetic hosts.
/// The generator must call dispatch immediately before external work.
pub fn step_with(
    q: &mut Queue,
    library: &Path,
    job_id: Option<&str>,
    generate: impl FnOnce(
        &Path,
        &Snapshot,
        &Profile,
        &str,
        &mut dyn FnMut() -> Result<()>,
    ) -> Result<(String, Option<u64>)>,
) -> Result<bool> {
    step_with_usage(q, library, job_id, |db, snapshot, profile, input, dispatch| {
        let (text, total_tokens) = generate(db, snapshot, profile, input, dispatch)?;
        Ok((text, DispatchUsage { total_tokens, ..DispatchUsage::default() }))
    })
}

pub fn step_with_usage(
    q: &mut Queue,
    library: &Path,
    job_id: Option<&str>,
    generate: impl FnOnce(&Path, &Snapshot, &Profile, &str, &mut dyn FnMut() -> Result<()>) -> Result<(String, DispatchUsage)>,
) -> Result<bool> {
    let Some(_execution) = execution_lock(q)? else {
        return Ok(false);
    };
    recover_locked(q, library)?;
    let library = library.canonicalize()?;
    let tx = q
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let json: Option<String> = tx.query_row("SELECT payload FROM reader_repair_jobs WHERE library=?1 AND state='queued' AND (?2 IS NULL OR job_id=?2) AND COALESCE(json_extract(payload,'$.retry_at'),0)<=?3 ORDER BY rowid LIMIT 1",
        params![library.to_string_lossy(), job_id, Utc::now().timestamp_millis()], |r| r.get(0)).optional()?;
    let Some(json) = json else { return Ok(false) };
    let mut r: Record = serde_json::from_str(&json)?;
    r.job.state = "running".into();
    r.started_at_ms = Some(Utc::now().timestamp_millis());
    tx.execute(
        "UPDATE reader_repair_jobs SET state='running',payload=?1 WHERE job_id=?2",
        params![serde_json::to_string(&r)?, r.job.job_id],
    )?;
    tx.commit()?;
    execute_record(q, &library, r, |_, _| Ok(()), generate)?;
    Ok(true)
}

// The caller holds the execution lock and owns admission. Batch children live
// outside reader_repair_jobs, but use this exact chapter engine.
fn execute_record(
    q: &Queue,
    library: &Path,
    mut r: Record,
    guard: impl Fn(&Queue, &Record) -> Result<()>,
    generate: impl FnOnce(&Path, &Snapshot, &Profile, &str, &mut dyn FnMut() -> Result<()>) -> Result<(String, DispatchUsage)>,
) -> Result<()> {
    let start = Instant::now();
    let result = (|| -> Result<()> {
        guard(q, &r)?;
        permitted(&library, &r.job.request.item_id, r.ceiling)?;
        if r.job.stage != "committing" {
            r.job.stage = "formatting".into();
            put(q, &r)?;
            let fresh = reader::repair::plan(
                &library,
                &r.job.request.item_id,
                &r.job.request.revision_id,
                &r.job.request.chapter,
                r.ceiling,
            )?;
            anyhow::ensure!(fresh == r.plan, "stale_chapter_alignment");
            // Existing successful bound work is reused. No sparse anchors are consulted.
            // An offline all-unresolved partition is not successful matching work.
            if r.batch_id.is_some()
                || r.plan.expected_alignment_version == "missing"
                || r.plan
                    .pairs
                    .iter()
                    .all(|p| p.relation == reader::paragraphs::Relation::Unresolved)
            {
                let pools =
                    matching::pools(&library, &r.plan, r.ceiling, r.profile.max_input_bytes)?;
                if let Some((input, pools)) = pools {
                    // Before any dispatch: an older job never meets this parser.
                    matching::ensure_policy(r.parser_policy)?;
                    anyhow::ensure!(
                        memex_models::read(&q.conn)? == r.settings
                            && memex_models::low_dial::read(&q.conn)?.as_ref() == Some(&r.profile),
                        "no_low_dial_provider"
                    );
                    let mut snapshot = r.settings.clone();
                    snapshot
                        .configuration
                        .selections
                        .retain(|s| s.purpose != Purpose::CaptureQuality);
                    snapshot.configuration.selections.push(Selection {
                        purpose: Purpose::CaptureQuality,
                        model: r.profile.model.clone(),
                        reasoning_effort: r.profile.reasoning_effort,
                    });
                    r.job.stage = "matching".into();
                    put(q, &r)?;
                    let profile = r.profile.clone();
                    let (answer, tokens) =
                        generate(q.database_path(), &snapshot, &profile, &input, &mut || {
                            guard(q, &r)?;
                            permitted(&library, &r.job.request.item_id, r.ceiling)?;
                            let fresh = reader::repair::plan(
                                &library,
                                &r.job.request.item_id,
                                &r.job.request.revision_id,
                                &r.job.request.chapter,
                                r.ceiling,
                            )?;
                            anyhow::ensure!(fresh == r.plan, "stale_chapter_alignment");
                            anyhow::ensure!(
                                memex_models::read(&q.conn)? == r.settings
                                    && memex_models::low_dial::read(&q.conn)?.as_ref()
                                        == Some(&r.profile),
                                "no_low_dial_provider"
                            );
                            anyhow::ensure!(
                                r.job.usage.requests_dispatched < r.job.budget.max_requests,
                                "reader_repair_budget_exceeded"
                            );
                            r.job.usage.dispatched();
                            r.dispatch_in_flight = true;
                            put(q, &r) // durable intent after credentials, before external request
                        })?;
                    r.dispatch_in_flight = false;
                    r.job.usage.observed(tokens);
                    put(q, &r)?; // even an invalid task response can have incurred cost
                    let (pairs, rejected) = matching::accept(&answer, &pools)?;
                    r.rejected_groups = rejected;
                    anyhow::ensure!(
                        r.rejected_groups.is_empty() || matching::status(&pairs) != "none",
                        "reader_alignment_no_usable_matches"
                    );
                    r.plan.pairs = pairs;
                    r.plan.input_profile_sha256 =
                        reader::reader_sha256(&serde_json::to_vec(&(&r.settings, &r.profile))?);
                }
            }
            r.job.stage = "committing".into();
            put(q, &r)?;
        }
        if r.apply {
            guard(q, &r)?;
            let saved = reader::repair::apply(&library, r.plan.clone(), r.ceiling)?;
            r.job.alignment_status = Some(matching::status(&r.plan.pairs).into());
            r.job.result_revision_id = Some(saved.revision_id);
            r.job.alignment_version = Some(saved.alignment_version);
        }
        Ok(())
    })();
    r.job.elapsed_ms = r
        .job
        .elapsed_ms
        .saturating_add(start.elapsed().as_millis() as u64);
    r.started_at_ms = None;
    match result {
        Ok(()) => {
            r.job.state = "succeeded".into();
            r.job.error_code = None;
        }
        Err(error) => {
            let raw = error.to_string();
            r.failure_stage = Some(r.job.stage.clone());
            r.failure_reason = Some(match raw.as_str() {
                "reader_alignment_invalid_response"
                | "alignment_output_json"
                | "alignment_output_schema"
                | "alignment_output_size"
                | "reader_alignment_policy_stale"
                | "reader_alignment_too_many_groups"
                | "reader_alignment_no_usable_matches"
                | "stale_item_revision"
                | "stale_chapter_alignment"
                | "library_resource_not_found"
                | "no_low_dial_provider" => raw.clone(),
                _ => "operation_failed".into(),
            });
            r.job.failure_stage = r.failure_stage.clone();
            r.job.failure_reason = r.failure_reason.clone();
            let commit_unknown = r.job.stage == "committing"
                && r.apply
                && !matches!(
                    raw.as_str(),
                    "stale_item_revision"
                        | "stale_chapter_alignment"
                        | "library_resource_not_found"
                        | "invalid_reader_alignment_request"
                        | "permission_denied"
                        | "reader_batch_quote_stale"
                );
            let provider_unknown = error
                .downcast_ref::<memex_provider::generation::GenerationError>()
                == Some(&memex_provider::generation::GenerationError::OutcomeUnknown);
            let unknown = provider_unknown || commit_unknown;
            let model_step = error
                .downcast_ref::<memex_provider::generation::GenerationError>()
                .is_some()
                || matches!(
                    raw.as_str(),
                    "reader_alignment_invalid_response"
                        | "alignment_output_json"
                        | "alignment_output_schema"
                        | "alignment_output_size"
                        | "reader_alignment_no_usable_matches"
                );
            r.job.state = if unknown { "outcome_unknown" } else { "failed" }.into();
            // Bounded automatic retry of a single-chapter model step (one more request each).
            if r.batch_id.is_none()
                && model_step
                && crate::queue::auto_retryable(&error)
                && !commit_unknown
                && r.job.stage != "committing"
                && r.auto_retries < crate::queue::AUTO_RETRIES
            {
                r.auto_retries += 1;
                r.dispatch_in_flight = false;
                r.job.budget.max_requests = r.job.budget.max_requests.saturating_add(1);
                r.job.state = "queued".into();
                r.job.error_code = None;
                r.retry_at = Utc::now().timestamp_millis()
                    + crate::queue::auto_retry_delay_ms(r.auto_retries);
                put(q, &r)?;
                return Ok(());
            }
            r.job.error_code = Some(
                if provider_unknown {
                    "provider_outcome_unknown"
                } else {
                    match raw.as_str() {
                        "stale_item_revision"
                        | "stale_chapter_alignment"
                        | "library_resource_not_found"
                        | "no_low_dial_provider" => &raw,
                        _ => "reader_repair_failed",
                    }
                }
                .into(),
            );
        }
    }
    if !(r.batch_id.is_some() && r.job.state == "outcome_unknown" && r.job.stage == "committing") {
        r.job.stage = "done".into();
    }
    put(q, &r)?;
    Ok(())
}
