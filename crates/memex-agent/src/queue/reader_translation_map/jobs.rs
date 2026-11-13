use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub item_id: String,
    pub revision_id: String,
    pub chapter: String,
    pub translation_version: String,
    pub quote_id: String,
    pub request_id: String,
    pub accept_cost: bool,
    pub accept_model_processing: bool,
    pub expected_map_version: String,
}
impl Request {
    fn selection(&self) -> Selection {
        Selection {
            item_id: self.item_id.clone(),
            revision_id: self.revision_id.clone(),
            chapter: self.chapter.clone(),
            translation_version: self.translation_version.clone(),
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
    pub map_version: Option<String>,
    pub result_revision_id: Option<String>,
    pub error_code: Option<String>,
    pub failure_stage: Option<String>,
    pub failure_reason: Option<String>,
    pub budget: Budget,
    pub usage: Usage,
    pub elapsed_ms: u64,
}

#[derive(Serialize, Deserialize)]
struct Record {
    schema_version: u32,
    job: Job,
    frozen: Frozen,
    in_flight: bool,
    stop_requested: bool,
    pairs: Option<Vec<Pair>>,
    /// matching::PARSER_POLICY at admission; absent (0) for older jobs.
    #[serde(default)]
    parser_policy: u32,
}

fn load(conn: &Connection, id: &str) -> Result<Record> {
    let json: String = conn
        .query_row(
            "SELECT payload FROM reader_translation_map_jobs WHERE job_id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?
        .context("reader_job_not_found")?;
    let r: Record = serde_json::from_str(&json)?;
    anyhow::ensure!(r.schema_version == 1, "reader_translation_map_unavailable");
    Ok(r)
}

fn put(conn: &Connection, r: &Record) -> Result<()> {
    conn.execute(
        "UPDATE reader_translation_map_jobs SET state=?1,payload=?2 WHERE job_id=?3",
        params![r.job.state, serde_json::to_string(r)?, r.job.job_id],
    )?;
    Ok(())
}

fn replay(
    conn: &Connection,
    library: &Path,
    authority: &Authority,
    request: &Request,
) -> Result<Option<Job>> {
    let id: Option<String> = conn
        .query_row(
            "SELECT job_id FROM reader_translation_map_jobs WHERE request_id=?1",
            [&request.request_id],
            |r| r.get(0),
        )
        .optional()?;
    id.map(|id| {
        let r = load(conn, &id)?;
        anyhow::ensure!(
            r.frozen.library == library
                && &r.frozen.authority == authority
                && r.job.request == *request,
            "request_identity_conflict"
        );
        Ok(r.job)
    })
    .transpose()
}

fn access(
    r: &Record,
    authority: &Authority,
    ceiling: Sensitivity,
    phase: Phase,
    authorize: &impl Fn(Check<'_>) -> Result<Sensitivity>,
) -> Result<Sensitivity> {
    anyhow::ensure!(&r.frozen.authority == authority, "reader_job_not_found");
    let ceiling = authority.check(
        &r.frozen.library,
        &r.job.request.item_id,
        phase,
        ceiling.min(r.frozen.ceiling),
        authorize,
    )?;
    // Job reads remain authorized through the historical chain after current
    // revision drift. Dispatch/commit separately require the exact current CAS.
    reader::revision_text::chunk(
        &r.frozen.library,
        &r.job.request.item_id,
        &r.job.request.revision_id,
        reader::revision_text::Target::Chapter {
            side: "original".into(),
            chapter: r.job.request.chapter.clone(),
        },
        0,
        4,
        &r.frozen.identity.source.source_sha256,
        ceiling,
    )?;
    Ok(ceiling)
}

// Queue configuration writers are excluded by the caller's immediate transaction.
// Credential generation is separately checked; it is never a persisted grant.
fn frozen_config(conn: &Connection, f: &Frozen) -> Result<()> {
    anyhow::ensure!(
        memex_models::read(conn)? == f.settings
            && memex_models::low_dial::read(conn)?.as_ref() == Some(&f.profile),
        "reader_translation_map_quote_stale"
    );
    Ok(())
}

pub fn request(
    q: &mut Queue,
    library: &Path,
    authority: &Authority,
    request: Request,
    ceiling: Sensitivity,
) -> Result<Job> {
    request_with(
        q,
        library,
        authority,
        request,
        ceiling,
        Utc::now().timestamp_millis(),
        credential_generation,
        local_only,
    )
}

pub fn request_with(
    q: &mut Queue,
    library: &Path,
    authority: &Authority,
    request: Request,
    ceiling: Sensitivity,
    now: i64,
    generation: impl Fn(&Path, &Snapshot) -> Result<u64>,
    authorize: impl Fn(Check<'_>) -> Result<Sensitivity>,
) -> Result<Job> {
    init(&q.conn)?;
    anyhow::ensure!(
        request.accept_cost
            && request.accept_model_processing
            && request.expected_map_version == "missing"
            && (1..=128).contains(&request.request_id.len())
            && request
                .request_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')),
        "invalid_reader_translation_map_request"
    );
    let library = library.canonicalize()?;
    let ceiling = authority.check(
        &library,
        &request.item_id,
        Phase::Admission,
        ceiling,
        &authorize,
    )?;
    // Exact committed request replay precedes expiry and credential checks.
    if let Some(job) = replay(&q.conn, &library, authority, &request)? {
        access(
            &load(&q.conn, &job.job_id)?,
            authority,
            ceiling,
            Phase::Admission,
            &authorize,
        )?;
        return Ok(job);
    }
    source(&library, &request.selection(), ceiling)?;
    let json: String = q
        .conn
        .query_row(
            "SELECT payload FROM reader_translation_map_quotes WHERE quote_id=?1",
            [&request.quote_id],
            |r| r.get(0),
        )
        .optional()?
        .context("reader_translation_map_quote_stale")?;
    let frozen: Frozen = serde_json::from_str(&json)?;
    anyhow::ensure!(
        frozen.library == library
            && &frozen.authority == authority
            && frozen.quote.selection == request.selection()
            && now < frozen.quote.expires_at,
        "reader_translation_map_quote_stale"
    );
    guard(q, &frozen, ceiling, &generation)?;
    let live = authority.check(
        &library,
        &request.item_id,
        Phase::Admission,
        ceiling.min(frozen.ceiling),
        &authorize,
    )?;
    let tx = q
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(job) = replay(&tx, &library, authority, &request)? {
        return Ok(job);
    }
    anyhow::ensure!(
        source(&library, &request.selection(), live)?.identity == frozen.identity,
        "reader_translation_map_quote_stale"
    );
    frozen_config(&tx, &frozen)?;
    anyhow::ensure!(
        !reader_item_busy(&tx, &library, &request.item_id)?,
        "reader_job_in_progress"
    );
    // Also fence our own unknown work before the shared busy seam is upgraded.
    let busy: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM reader_translation_map_jobs WHERE library=?1 AND item_id=?2 AND state IN ('queued','running','stopping','outcome_unknown'))",
        params![library.to_string_lossy(), request.item_id], |r| r.get(0))?;
    anyhow::ensure!(!busy, "reader_job_in_progress");
    let job_id = format!("map_job_{}", uuid::Uuid::now_v7().simple());
    anyhow::ensure!(tx.execute("UPDATE reader_translation_map_quotes SET consumed_by=?1 WHERE quote_id=?2 AND consumed_by IS NULL",
        params![job_id, request.quote_id])? == 1, "reader_translation_map_quote_consumed");
    let r = Record {
        schema_version: 1,
        job: Job {
            job_id,
            operation: "chapter_translation_map".into(),
            request,
            state: "queued".into(),
            stage: "queued".into(),
            map_version: None,
            result_revision_id: None,
            error_code: None,
            failure_stage: None,
            failure_reason: None,
            budget: frozen.quote.budget.clone(),
            usage: Usage {
                dispatches: Some(Vec::new()),
                ..Usage::default()
            },
            elapsed_ms: 0,
        },
        frozen,
        in_flight: false,
        stop_requested: false,
        pairs: None,
        parser_policy: matching::PARSER_POLICY,
    };
    tx.execute("INSERT INTO reader_translation_map_jobs(job_id,request_id,library,item_id,state,payload) VALUES(?1,?2,?3,?4,?5,?6)",
        params![r.job.job_id, r.job.request.request_id, library.to_string_lossy(), r.job.request.item_id,
            r.job.state, serde_json::to_string(&r)?])?;
    tx.commit()?;
    Ok(r.job)
}

fn bound(
    q: &Queue,
    library: &Path,
    authority: &Authority,
    item: &str,
    id: &str,
    ceiling: Sensitivity,
    phase: Phase,
    authorize: &impl Fn(Check<'_>) -> Result<Sensitivity>,
) -> Result<(Record, Sensitivity)> {
    init(&q.conn)?;
    let r = load(&q.conn, id)?;
    anyhow::ensure!(
        r.frozen.library == library.canonicalize()? && r.job.request.item_id == item,
        "reader_job_not_found"
    );
    let ceiling = access(&r, authority, ceiling, phase, authorize)?;
    Ok((r, ceiling))
}

pub fn status(
    q: &Queue,
    library: &Path,
    authority: &Authority,
    item: &str,
    id: &str,
    ceiling: Sensitivity,
) -> Result<Job> {
    status_with(q, library, authority, item, id, ceiling, local_only)
}

pub fn status_with(
    q: &Queue,
    library: &Path,
    authority: &Authority,
    item: &str,
    id: &str,
    ceiling: Sensitivity,
    authorize: impl Fn(Check<'_>) -> Result<Sensitivity>,
) -> Result<Job> {
    Ok(bound(
        q,
        library,
        authority,
        item,
        id,
        ceiling,
        Phase::Read,
        &authorize,
    )?
    .0
    .job)
}

fn reconcile(r: &mut Record, ceiling: Sensitivity) -> Result<bool> {
    if let Some(receipt) = store::receipt(
        &r.frozen.library,
        &r.frozen.identity,
        &r.job.job_id,
        ceiling,
    )? {
        anyhow::ensure!(
            receipt.identity == r.frozen.identity && receipt.job_id == r.job.job_id,
            "reader_translation_map_unavailable"
        );
        r.job.map_version = Some(receipt.map_version);
        r.job.result_revision_id = Some(r.job.request.revision_id.clone());
        r.job.state = "succeeded".into();
        r.job.stage = "done".into();
        r.job.error_code = None;
        r.job.failure_stage = None;
        r.job.failure_reason = None;
        r.in_flight = false;
        return Ok(true);
    }
    Ok(false)
}

pub fn stop(
    q: &mut Queue,
    library: &Path,
    authority: &Authority,
    item: &str,
    id: &str,
    ceiling: Sensitivity,
) -> Result<Job> {
    stop_with(q, library, authority, item, id, ceiling, local_only)
}

pub fn stop_with(
    q: &mut Queue,
    library: &Path,
    authority: &Authority,
    item: &str,
    id: &str,
    ceiling: Sensitivity,
    authorize: impl Fn(Check<'_>) -> Result<Sensitivity>,
) -> Result<Job> {
    let (_, ceiling) = bound(
        q,
        library,
        authority,
        item,
        id,
        ceiling,
        Phase::Stop,
        &authorize,
    )?;
    let tx = q
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut r = load(&tx, id)?;
    if !reconcile(&mut r, ceiling)?
        && matches!(
            r.job.state.as_str(),
            "queued" | "running" | "stopping" | "outcome_unknown"
        )
    {
        r.stop_requested = true;
        // Unknown remains a durable exclusion, even after the stop intent.
        if r.job.state != "outcome_unknown" {
            r.job.state = if r.in_flight { "stopping" } else { "stopped" }.into();
        }
    }
    put(&tx, &r)?;
    tx.commit()?;
    Ok(r.job)
}

fn fail(r: &mut Record, code: &str) {
    r.job.failure_stage = Some(r.job.stage.clone());
    r.job.error_code = Some(code.into());
    r.job.failure_reason = Some(code.into());
    r.job.state = "failed".into();
}

pub fn recover(q: &mut Queue, library: &Path) -> Result<usize> {
    recover_with(q, library, local_only)
}

/// Recovery is receipt-only: no provider or credential access and no new dispatch.
pub fn recover_with(
    q: &mut Queue,
    library: &Path,
    _authorize: impl Fn(Check<'_>) -> Result<Sensitivity>,
) -> Result<usize> {
    init(&q.conn)?;
    let Some(_lock) = reader_repair::function_lock(q, reader_repair::TRANSLATION_LOCK)? else {
        return Ok(0);
    };
    let library = library.canonicalize()?;
    let ids = q.conn.prepare("SELECT job_id FROM reader_translation_map_jobs WHERE library=?1 AND state IN ('queued','running','stopping','outcome_unknown')")?
        .query_map([library.to_string_lossy()], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut count = 0;
    for id in ids {
        let tx = q
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut r = load(&tx, &id)?;
        // Internal accounting is not disclosure to the expired session. Store
        // still verifies the frozen identity/proof; status/read require live access.
        let ceiling = r.frozen.ceiling;
        match reconcile(&mut r, ceiling) {
            Ok(true) => {}
            Ok(false) => {
                r.job.state = if r.in_flight {
                    "outcome_unknown"
                } else {
                    "stopped"
                }
                .into();
                r.job.error_code = Some(
                    if r.in_flight {
                        "provider_outcome_unknown"
                    } else {
                        "interrupted"
                    }
                    .into(),
                );
            }
            Err(_) => {
                r.job.state = "outcome_unknown".into();
                r.job.error_code = Some("reader_commit_outcome_unknown".into());
            }
        }
        // A worker tick is not fresh consent: no recovered job remains runnable.
        put(&tx, &r)?;
        tx.commit()?;
        count += 1;
    }
    Ok(count)
}

pub fn step(q: &mut Queue, library: &Path) -> Result<bool> {
    let database = q.database_path().to_owned();
    step_with(
        q,
        library,
        credential_generation,
        local_only,
        |settings, profile, generation, input, dispatch| {
            use memex_provider::{
                credentials::Credentials,
                generation::{complete_with_optional_effort, TextRequest},
            };
            let (provider, _) = settings
                .configuration
                .target(Purpose::CaptureQuality)
                .context("no_low_dial_provider")?;
            anyhow::ensure!(
                credential_generation(&database, settings)? == generation,
                "reader_translation_map_quote_stale"
            );
            let credential =
                Credentials::for_database(&database, &provider.id).resolve(provider, generation)?;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            dispatch()?;
            let response = runtime.block_on(complete_with_optional_effort(
                settings,
                &credential,
                TextRequest {
                    purpose: Purpose::CaptureQuality,
                    system: matching::PROMPT,
                    input,
                    max_completion_tokens: profile.max_completion_tokens,
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

/// Generator MUST call dispatch exactly once immediately before sending. A failed
/// callback forbids sending. Errors after callback are conservatively unknown.
pub fn step_with(
    q: &mut Queue,
    library: &Path,
    generation: impl Fn(&Path, &Snapshot) -> Result<u64>,
    authorize: impl Fn(Check<'_>) -> Result<Sensitivity>,
    generate: impl FnOnce(
        &Snapshot,
        &Profile,
        u64,
        &str,
        &mut dyn FnMut() -> Result<()>,
    ) -> Result<(String, DispatchUsage)>,
) -> Result<bool> {
    init(&q.conn)?;
    let Some(_lock) = reader_repair::function_lock(q, reader_repair::TRANSLATION_LOCK)? else {
        return Ok(false);
    };
    let library = library.canonicalize()?;
    let id: Option<String> = q.conn.query_row("SELECT job_id FROM reader_translation_map_jobs WHERE library=?1 AND state IN ('queued','running') ORDER BY rowid LIMIT 1",
        [library.to_string_lossy()], |r| r.get(0)).optional()?;
    let Some(id) = id else { return Ok(false) };
    let mut r = load(&q.conn, &id)?;
    let frozen_ceiling = r.frozen.ceiling;
    match reconcile(&mut r, frozen_ceiling) {
        Ok(true) => {
            put(&q.conn, &r)?;
            return Ok(true);
        }
        Ok(false) => {}
        Err(_) => {
            r.job.state = "outcome_unknown".into();
            r.job.error_code = Some("reader_commit_outcome_unknown".into());
            put(&q.conn, &r)?;
            return Ok(true);
        }
    }
    let ceiling = match access(
        &r,
        &r.frozen.authority,
        r.frozen.ceiling,
        Phase::Reconcile,
        &authorize,
    ) {
        Ok(c) => c,
        Err(_) => {
            if r.in_flight {
                r.job.state = "outcome_unknown".into();
            } else {
                fail(&mut r, "reader_translation_map_authorization_required");
            }
            put(&q.conn, &r)?;
            return Ok(true);
        }
    };
    if r.in_flight {
        r.job.state = "outcome_unknown".into();
        put(&q.conn, &r)?;
        return Ok(true);
    }
    if r.stop_requested {
        r.job.state = "stopped".into();
        put(&q.conn, &r)?;
        return Ok(true);
    }
    // Before any dispatch; an already accepted plan still commits by its frozen terms.
    if r.pairs.is_none() && matching::ensure_policy(r.parser_policy).is_err() {
        fail(&mut r, "reader_alignment_policy_stale");
        put(&q.conn, &r)?;
        return Ok(true);
    }
    let (input, pools) = match guard(q, &r.frozen, ceiling, &generation) {
        Ok(input) => input,
        Err(_) => {
            fail(&mut r, "reader_translation_map_quote_stale");
            put(&q.conn, &r)?;
            return Ok(true);
        }
    };
    if let Some(pairs) = r.pairs.as_ref() {
        let provenance = store::Provenance {
            route: r.frozen.quote.route.clone(),
            input_profile_sha256: reader::reader_sha256(&serde_json::to_vec(&(
                &r.frozen.profile,
                &r.frozen.input_sha256,
            ))?),
            prompt_version: POLICY.into(),
            prompt_sha256: reader::reader_sha256(matching::PROMPT.as_bytes()),
            algorithm: POLICY.into(),
        };
        // Serialize stop versus commit; a stop arriving after this boundary may
        // observe the receipt, never erase a completed publication or its cost.
        let live = match access(&r, &r.frozen.authority, ceiling, Phase::Commit, &authorize) {
            Ok(live) => live,
            Err(_) => {
                fail(&mut r, "reader_translation_map_authorization_required");
                put(&q.conn, &r)?;
                return Ok(true);
            }
        };
        let tx = q
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = load(&tx, &id)?;
        if current.stop_requested {
            return Ok(true);
        }
        frozen_config(&tx, &r.frozen)?;
        match store::publish(
            &library,
            r.frozen.identity.clone(),
            provenance,
            pairs.clone(),
            &id,
            live,
        ) {
            Ok(_) => {
                reconcile(&mut r, live)?;
            }
            Err(_) => {
                // A publication may have reached disk before a return failure.
                if !reconcile(&mut r, live)? {
                    fail(&mut r, "reader_translation_map_commit_failed");
                }
            }
        }
        put(&tx, &r)?;
        tx.commit()?;
        return Ok(true);
    }
    let routed = route(&r.frozen.settings, &r.frozen.profile)?;
    let started = std::time::Instant::now();
    let mut dispatched = false;
    let result = generate(
        &routed,
        &r.frozen.profile,
        r.frozen.credential_generation,
        &input,
        &mut || {
            anyhow::ensure!(!dispatched, "reader_translation_map_budget_exceeded");
            let live = access(
                &r,
                &r.frozen.authority,
                ceiling,
                Phase::Dispatch,
                &authorize,
            )?;
            guard(q, &r.frozen, live, &generation)?;
            let tx = q
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let mut current = load(&tx, &id)?;
            anyhow::ensure!(
                !current.stop_requested
                    && current.job.state == "queued"
                    && !current.in_flight
                    && current.job.usage.requests_dispatched == 0,
                "reader_translation_map_dispatch_blocked"
            );
            frozen_config(&tx, &r.frozen)?;
            current.in_flight = true;
            current.job.state = "running".into();
            current.job.stage = "matching".into();
            current.job.usage.dispatched();
            put(&tx, &current)?;
            tx.commit()?;
            dispatched = true;
            Ok(())
        },
    );
    // Reload preserves concurrent stop intent and durable dispatch accounting.
    let tx = q
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    r = load(&tx, &id)?;
    r.job.elapsed_ms = r
        .job
        .elapsed_ms
        .saturating_add(started.elapsed().as_millis() as u64);
    match result {
        Ok((answer, usage)) if dispatched => {
            r.job.usage.observed(usage);
            r.in_flight = false;
            if r.stop_requested {
                r.job.state = "stopped".into();
            } else {
                match matching::accept(&answer, &pools) {
                    Ok((pairs, _)) => {
                        r.pairs = Some(pairs);
                        r.job.stage = "committing".into();
                    }
                    Err(_) => fail(&mut r, "reader_alignment_invalid_response"),
                }
            }
        }
        Err(_) if dispatched => {
            r.job.state = "outcome_unknown".into();
        }
        _ => {
            if r.stop_requested {
                r.job.state = "stopped".into();
            } else {
                fail(&mut r, "reader_translation_map_dispatch_blocked");
            }
        }
    }
    put(&tx, &r)?;
    tx.commit()?;
    Ok(true)
}
