//! Principal-bound durable batches. A failed or uncertain model window is retried
//! automatically at most `AUTO_RETRIES` times with backoff (validated windows are kept).
use super::*;
use crate::queue::reader_repair::Usage;
use crate::queue::reader_repair::{function_lock, DispatchUsage, FORMAT_LOCK};

#[derive(Clone, Serialize, Deserialize)]
pub struct Job {
    pub job_id: String,
    pub request_id: String,
    pub operation: String,
    pub item_id: String,
    pub revision_id: String,
    pub target: store::Target,
    pub state: String,
    pub stage: String,
    pub result_revision_id: Option<String>,
    pub error_code: Option<String>,
    pub failure_stage: Option<String>,
    pub failure_reason: Option<String>,
    pub budget: planner::Budget,
    pub usage: Usage,
    pub elapsed_ms: u64,
}

#[derive(Clone, Serialize, Deserialize)]
struct Child {
    state: String,
    reason: Option<String>,
    job: Option<Job>,
    formatting: Option<serde_json::Value>,
    outputs: Vec<String>,
    in_flight: bool,
    retry_at: i64,
    #[serde(default)]
    coverage: Option<(usize, usize)>,
    #[serde(default)]
    auto_retries: u32,
}

#[derive(Clone, Serialize, Deserialize)]
struct Record {
    batch_id: String,
    request_id: String,
    state: String,
    error_code: Option<String>,
    frozen: Frozen,
    children: Vec<Child>,
}

fn view(r: &Record) -> serde_json::Value {
    let mut counts = serde_json::json!({"total":r.children.len(),"queued":0,"running":0,"succeeded":0,"failed":0,"outcome_unknown":0,"skipped":0});
    let mut requests = 0u64;
    let mut total = 0u64;
    let mut unknown = 0u64;
    let mut input = Some(0u64);
    let mut output = Some(0u64);
    for child in &r.children {
        if let Some(count) = counts.get_mut(&child.state) {
            *count = serde_json::json!(count.as_u64().unwrap_or(0) + 1);
        }
        if let Some(job) = &child.job {
            requests += u64::from(job.usage.requests_dispatched);
            total = total.saturating_add(job.usage.reported_total_tokens);
            unknown += u64::from(job.usage.unknown_usage_requests);
            input = input
                .and_then(|n| job.usage.input_tokens.and_then(|v| n.checked_add(v)))
                .filter(|n| *n <= SAFE as u64);
            output = output
                .and_then(|n| job.usage.output_tokens.and_then(|v| n.checked_add(v)))
                .filter(|n| *n <= SAFE as u64);
        }
    }
    serde_json::json!({"batch_id":r.batch_id,"request_id":r.request_id,"state":r.state,"counts":counts,"error_code":r.error_code,
        "usage":{"requests_dispatched":requests,"reported_total_tokens":total,"unknown_usage_requests":unknown,"input_tokens":input,"output_tokens":output}})
}

fn load(
    q: &Queue,
    library: &Path,
    principal: &str,
    id: &str,
    ceiling: Sensitivity,
) -> Result<Record> {
    let r = load_bound(q, library, principal, id)?;
    authorize(&r.frozen, ceiling)?;
    Ok(r)
}

fn load_bound(q: &Queue, library: &Path, principal: &str, id: &str) -> Result<Record> {
    init(q)?;
    let raw: String = q.conn.query_row("SELECT payload FROM content_format_batches WHERE batch_id=?1 AND library=?2 AND principal=?3",
        params![id,library.to_string_lossy(),principal], |r| r.get(0)).optional()?.context("content_format_batch_not_found")?;
    let r: Record = serde_json::from_str(&raw)?;
    Ok(r)
}

fn persist(q: &Queue, r: &Record) -> Result<()> {
    q.conn.execute(
        "UPDATE content_format_batches SET state=?1,payload=?2 WHERE batch_id=?3",
        params![r.state, serde_json::to_string(r)?, r.batch_id],
    )?;
    Ok(())
}

pub(super) fn already_formatted(
    q: &Queue,
    library: &Path,
    target: &Target,
    ceiling: Sensitivity,
) -> Result<bool> {
    let rows = q
        .conn
        .prepare("SELECT payload FROM content_format_batches WHERE library=?1")?
        .query_map([library.to_string_lossy()], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for raw in rows {
        let r: Record = serde_json::from_str(&raw)?;
        if r.frozen
            .preview
            .quote
            .as_ref()
            .is_none_or(|q| q.policy_version != POLICY)
        {
            continue;
        }
        for child in r.children {
            let Some(job) = child.job else { continue };
            if child.state == "succeeded"
                && job.item_id == target.item_id
                && job.target == target.target
                && job.result_revision_id.as_deref() == Some(target.revision_id.as_str())
            {
                if let Some(receipt) = store::receipt(
                    library,
                    &job.item_id,
                    &job.job_id,
                    &r.frozen.principal,
                    ceiling,
                )? {
                    if receipt.result_revision_id == target.revision_id {
                        return Ok(true);
                    }
                }
            }
        }
    }
    Ok(false)
}

fn unchanged(
    q: &Queue,
    f: &Frozen,
    generation: &impl Fn(&Path, &Snapshot) -> Result<u64>,
) -> Result<()> {
    let settings = memex_models::read(&q.conn)?;
    let profile = memex_models::low_dial::read(&q.conn)?;
    anyhow::ensure!(
        serde_json::to_value(&settings)? == serde_json::to_value(&f.settings)?
            && serde_json::to_value(&profile)? == serde_json::to_value(&f.profile)?,
        "content_format_quote_stale"
    );
    if let Some(expected) = f.generation {
        let routed = route(
            &settings,
            profile.as_ref().context("content_format_quote_stale")?,
        )?;
        anyhow::ensure!(
            generation(q.database_path(), &routed)? == expected,
            "content_format_quote_stale"
        );
    }
    Ok(())
}

pub(crate) fn request_with(
    q: &mut Queue,
    library: &Path,
    principal: &str,
    quote_id: &str,
    request_id: &str,
    _accept_cost: bool,
    accept_model_processing: bool,
    ceiling: Sensitivity,
    generation: impl Fn(&Path, &Snapshot) -> Result<u64>,
) -> Result<serde_json::Value> {
    init(q)?;
    anyhow::ensure!(
        // `accept_cost` is no longer a gate (kept for older clients); consent to model
        // processing of the selected text still is.
        identifier(request_id) && identifier(quote_id) && accept_model_processing,
        "invalid_content_format_request"
    );
    let library = library.canonicalize()?;
    let replay = |conn: &Connection| -> Result<Option<Record>> {
        let raw: Option<String> = conn.query_row("SELECT payload FROM content_format_batches WHERE library=?1 AND principal=?2 AND request_id=?3",
            params![library.to_string_lossy(),principal,request_id], |r| r.get(0)).optional()?;
        raw.map(|raw| {
            let r: Record = serde_json::from_str(&raw)?;
            anyhow::ensure!(
                r.frozen
                    .preview
                    .quote
                    .as_ref()
                    .is_some_and(|q| q.quote_id == quote_id),
                "request_identity_conflict"
            );
            authorize(&r.frozen, ceiling)?;
            Ok(r)
        })
        .transpose()
    };
    if let Some(r) = replay(&q.conn)? {
        return Ok(serde_json::json!({"batch":view(&r)}));
    }
    let (raw, consumed): (String,Option<String>) = q.conn.query_row("SELECT payload,consumed_by FROM content_format_previews WHERE quote_id=?1 AND library=?2 AND principal=?3",
        params![quote_id,library.to_string_lossy(),principal], |r| Ok((r.get(0)?,r.get(1)?))).optional()?.context("content_format_quote_stale")?;
    let f: Frozen = serde_json::from_str(&raw)?;
    authorize(&f, ceiling)?;
    anyhow::ensure!(consumed.is_none(), "content_format_quote_consumed");
    let quote = f
        .preview
        .quote
        .as_ref()
        .context("content_format_quote_stale")?;
    anyhow::ensure!(
        f.preview.state == "ready"
            && quote.policy_version == POLICY
            && quote.expires_at > Utc::now().timestamp_millis(),
        "content_format_quote_stale"
    );
    unchanged(q, &f, &generation)?;
    let scope = members(&library, &f.selection, ceiling.min(f.ceiling))?;
    for target in &f.targets {
        if let Some(expected) = &target.identity {
            anyhow::ensure!(
                scope.contains(&expected.item_id),
                "content_format_quote_stale"
            );
            let source = store::source(
                &library,
                &expected.item_id,
                &expected.revision_id,
                expected.target.clone(),
                ceiling.min(f.ceiling),
            )?;
            anyhow::ensure!(source.identity == *expected, "content_format_quote_stale");
        }
    }
    let children = f
        .targets
        .iter()
        .map(|t| {
            let job = t.row.budget.clone().map(|budget| Job {
                job_id: memex_core::new_id("job"),
                request_id: memex_core::new_id("request"),
                operation: "content_format".into(),
                item_id: t.row.item_id.clone(),
                revision_id: t.row.revision_id.clone(),
                target: t.row.target.clone(),
                state: "queued".into(),
                stage: "queued".into(),
                result_revision_id: None,
                error_code: None,
                failure_stage: None,
                failure_reason: None,
                budget,
                usage: Usage {
                    dispatches: Some(Vec::new()),
                    input_tokens: Some(0),
                    output_tokens: Some(0),
                    ..Usage::default()
                },
                elapsed_ms: 0,
            });
            Child {
                state: if job.is_some() { "queued" } else { "skipped" }.into(),
                reason: t.row.reason.clone(),
                job,
                formatting: None,
                outputs: vec![],
                in_flight: false,
                retry_at: 0,
                coverage: None,
                auto_retries: 0,
            }
        })
        .collect::<Vec<_>>();
    let r = Record {
        batch_id: memex_core::new_id("batch"),
        request_id: request_id.into(),
        state: if children.iter().any(|c| c.state == "queued") {
            "queued"
        } else {
            "done"
        }
        .into(),
        error_code: None,
        frozen: f,
        children,
    };
    let tx = q
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(replayed) = replay(&tx)? {
        return Ok(serde_json::json!({"batch":view(&replayed)}));
    }
    let used = tx.execute("UPDATE content_format_previews SET consumed_by=?1 WHERE quote_id=?2 AND consumed_by IS NULL",params![r.batch_id,quote_id])?;
    anyhow::ensure!(used == 1, "content_format_quote_consumed");
    tx.execute("INSERT INTO content_format_batches(batch_id,library,principal,request_id,quote_id,state,payload) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![r.batch_id,library.to_string_lossy(),principal,request_id,quote_id,r.state,serde_json::to_string(&r)?])?;
    tx.commit()?;
    Ok(serde_json::json!({"batch":view(&r)}))
}

pub fn status(
    q: &Queue,
    library: &Path,
    principal: &str,
    id: &str,
    ceiling: Sensitivity,
) -> Result<serde_json::Value> {
    Ok(serde_json::json!({"batch":view(&load(q,&library.canonicalize()?,principal,id,ceiling)?)}))
}

pub fn results(
    q: &Queue,
    library: &Path,
    principal: &str,
    id: &str,
    offset: usize,
    limit: usize,
    ceiling: Sensitivity,
) -> Result<serde_json::Value> {
    anyhow::ensure!((1..=100).contains(&limit), "invalid_content_format_request");
    let r = load(q, &library.canonicalize()?, principal, id, ceiling)?;
    anyhow::ensure!(offset <= r.children.len(), "invalid_content_format_request");
    let end = offset.saturating_add(limit).min(r.children.len());
    let rows = (offset..end).map(|i| { let child = &r.children[i]; let target = &r.frozen.targets[i].row;
        serde_json::json!({"target_id":target.target_id,"item_id":target.item_id,"revision_id":target.revision_id,"target":target.target,
            "state":child.state,"reason":child.reason,"job":child.job,"formatting":child.formatting}) }).collect::<Vec<_>>();
    Ok(
        serde_json::json!({"batch_id":id,"total":r.children.len(),"offset":offset,"next_offset":(end<r.children.len()).then_some(end),"results":rows}),
    )
}

pub(crate) fn resume_with(
    q: &Queue,
    library: &Path,
    principal: &str,
    id: &str,
    ceiling: Sensitivity,
    generation: impl Fn(&Path, &Snapshot) -> Result<u64>,
) -> Result<serde_json::Value> {
    let mut r = load(q, &library.canonicalize()?, principal, id, ceiling)?;
    if r.state == "paused" {
        reconcile_commits(&mut r, ceiling);
        done(&mut r);
        persist(q, &r)?;
    }
    if r.state == "paused" {
        unchanged(q, &r.frozen, &generation)?;
        r.state = "queued".into();
        r.error_code = None;
        persist(q, &r)?;
    }
    Ok(serde_json::json!({"batch":view(&r)}))
}

pub fn step(q: &mut Queue, library: &Path) -> Result<bool> {
    let database = q.database_path().to_owned();
    step_with(
        q,
        library,
        credential_generation,
        |settings, profile, generation, window, dispatch| {
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
                "content_format_quote_stale"
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
                    system: planner::PROMPT,
                    input: window.input(),
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
        store::receipt,
        store::publish,
    )
}

fn completed_target(
    q: &Queue,
    library: &Path,
    principal: &str,
    batch: &str,
    target: &str,
    ceiling: Sensitivity,
) -> Result<(String, String, String, Sensitivity)> {
    let r = load_bound(q, &library.canonicalize()?, principal, batch)?;
    let ceiling = ceiling.min(r.frozen.ceiling);
    let i = r
        .frozen
        .targets
        .iter()
        .position(|t| t.row.target_id == target)
        .context("content_format_changes_stale")?;
    let child = &r.children[i];
    anyhow::ensure!(child.state == "succeeded", "content_format_changes_stale");
    let job = child.job.as_ref().context("content_format_changes_stale")?;
    // Proof authorization is target-local: an unrelated trashed batch member
    // must not block review or restoration of this still-readable result.
    store::receipt(
        library,
        &job.item_id,
        &job.job_id,
        principal,
        ceiling.min(r.frozen.ceiling),
    )?
    .context("content_format_changes_stale")?;
    let version = child
        .formatting
        .as_ref()
        .and_then(|v| v["changes_version"].as_str())
        .context("content_format_changes_stale")?;
    Ok((
        job.job_id.clone(),
        version.into(),
        job.item_id.clone(),
        ceiling,
    ))
}

pub fn changes(
    q: &Queue,
    library: &Path,
    principal: &str,
    batch: &str,
    target: &str,
    expected: &str,
    offset: usize,
    limit: usize,
    ceiling: Sensitivity,
) -> Result<store::Changes> {
    let (operation, version, _, ceiling) =
        completed_target(q, library, principal, batch, target, ceiling)?;
    anyhow::ensure!(version == expected, "content_format_changes_stale");
    store::changes_list(
        library, &operation, principal, expected, offset, limit, ceiling,
    )
}

pub fn change_text(
    q: &Queue,
    library: &Path,
    principal: &str,
    batch: &str,
    target: &str,
    expected: &str,
    change: &str,
    part: &str,
    start: usize,
    limit: usize,
    ceiling: Sensitivity,
) -> Result<store::ChangeText> {
    let (operation, version, _, ceiling) =
        completed_target(q, library, principal, batch, target, ceiling)?;
    anyhow::ensure!(version == expected, "content_format_changes_stale");
    store::change_text(
        library, &operation, principal, expected, change, part, start, limit, ceiling,
    )
}

pub fn restore(
    q: &mut Queue,
    library: &Path,
    principal: &str,
    batch: &str,
    target: &str,
    revision: &str,
    request: &str,
    ceiling: Sensitivity,
) -> Result<store::RestoreReceipt> {
    let library = library.canonicalize()?;
    let (operation, version, item, ceiling) =
        completed_target(q, &library, principal, batch, target, ceiling)?;
    if let Some(receipt) = store::restore_receipt(
        &library, &operation, principal, &version, revision, request, ceiling,
    )? {
        return Ok(receipt);
    }
    let tx = q
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    anyhow::ensure!(
        !super::super::reader_item_busy(&tx, &library, &item)?,
        "reader_job_in_progress"
    );
    let result = store::restore(
        &library, &operation, principal, &version, revision, request, ceiling,
    )?;
    tx.commit()?;
    Ok(result)
}

fn done(r: &mut Record) {
    if r.children
        .iter()
        .all(|c| !matches!(c.state.as_str(), "queued" | "running"))
    {
        r.state = "done".into();
    } else if r.state != "paused" {
        r.state = "queued".into();
    }
}

fn retry_later(child: &mut Child, now: i64) -> bool {
    if child.auto_retries >= crate::queue::AUTO_RETRIES {
        return false;
    }
    child.auto_retries += 1;
    child.in_flight = false;
    child.state = "queued".into();
    child.retry_at = now + crate::queue::auto_retry_delay_ms(child.auto_retries);
    if let Some(job) = &mut child.job {
        job.state = "queued".into();
    }
    true
}

fn fail(child: &mut Child, state: &str, reason: &str) {
    child.state = state.into();
    child.reason = Some(reason.into());
    child.in_flight = false;
    if let Some(job) = &mut child.job {
        job.state = state.into();
        job.failure_stage = Some(job.stage.clone());
        job.stage = "done".into();
        job.error_code = Some(reason.into());
        job.failure_reason = Some(reason.into());
    }
}

fn reconcile_commits(r: &mut Record, ceiling: Sensitivity) {
    for i in 0..r.children.len() {
        let child = &r.children[i];
        let Some(job) = &child.job else { continue };
        if !child.in_flight
            && matches!(child.state.as_str(), "queued" | "running")
            && job.stage == "committing"
        {
            if let Ok(Some(saved)) = store::receipt(
                &r.frozen.library,
                &job.item_id,
                &job.job_id,
                &r.frozen.principal,
                ceiling.min(r.frozen.ceiling),
            ) {
                finish(r, i, saved);
            }
        }
    }
}

// Must hold the shared execution lock: a second process cannot steal a live call.
fn recover_locked(q: &Queue, library: &Path) -> Result<()> {
    let rows = q
        .conn
        .prepare("SELECT payload FROM content_format_batches WHERE library=?1 AND state='running'")?
        .query_map([library.to_string_lossy()], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for raw in rows {
        let mut r: Record = serde_json::from_str(&raw)?;
        let ceiling = r.frozen.ceiling;
        reconcile_commits(&mut r, ceiling);
        for child in &mut r.children {
            if child.in_flight {
                fail(child, "outcome_unknown", "provider_outcome_unknown");
            } else if child.state == "running" {
                child.state = "queued".into();
                if let Some(job) = &mut child.job {
                    job.state = "queued".into();
                }
            }
        }
        r.state = "paused".into();
        done(&mut r);
        persist(q, &r)?;
    }
    q.conn.execute(
        "DELETE FROM content_format_active WHERE library=?1",
        [library.to_string_lossy()],
    )?;
    Ok(())
}

fn guard(
    q: &Queue,
    r: &Record,
    i: usize,
    generation: &impl Fn(&Path, &Snapshot) -> Result<u64>,
) -> Result<(store::Source, planner::Plan)> {
    let f = &r.frozen;
    let target = &f.targets[i];
    let identity = target
        .identity
        .as_ref()
        .context("content_format_unavailable")?;
    anyhow::ensure!(
        members(&f.library, &f.selection, f.ceiling)?.contains(&identity.item_id),
        "scope_changed"
    );
    let source = store::source(
        &f.library,
        &identity.item_id,
        &identity.revision_id,
        identity.target.clone(),
        f.ceiling,
    )?;
    anyhow::ensure!(
        source.identity == *identity && source.identity.sensitivity != Sensitivity::Restricted,
        "stale_item_revision"
    );
    unchanged(q, f, generation)?;
    let profile = f.profile.as_ref().context("content_format_quote_stale")?;
    let plan = planner::plan(
        &source.markdown,
        planner::SavedLowLimits {
            max_input_bytes: profile.max_input_bytes,
            max_completion_tokens: profile.max_completion_tokens as usize,
        },
        &[],
    )?;
    anyhow::ensure!(
        target.plan_sha256.as_ref() == Some(&plan.identity().sha256()),
        "content_format_quote_stale"
    );
    Ok((source, plan))
}

/// One window or publication per step, with durable intent and no SQLite lock
/// held over provider I/O. Store hooks are injected for crash-boundary testing.
pub fn step_with(
    q: &mut Queue,
    library: &Path,
    generation: impl Fn(&Path, &Snapshot) -> Result<u64>,
    mut generate: impl FnMut(
        &Snapshot,
        &Profile,
        u64,
        &planner::Window,
        &mut dyn FnMut() -> Result<()>,
    ) -> Result<(String, DispatchUsage)>,
    mut receipt: impl FnMut(&Path, &str, &str, &str, Sensitivity) -> Result<Option<store::Receipt>>,
    mut publish: impl FnMut(
        &Path,
        store::Identity,
        &str,
        &str,
        &store::edits::Applied,
        serde_json::Value,
        Sensitivity,
    ) -> Result<store::Receipt>,
) -> Result<bool> {
    init(q)?;
    let library = library.canonicalize()?;
    let Some(_lock) = function_lock(q, FORMAT_LOCK)? else {
        return Ok(false);
    };
    recover_locked(q, &library)?;
    if prepare_one(q, &library, &generation)? {
        return Ok(true);
    }
    if super::admit_one_step(q, &library, &generation)? {
        return Ok(true);
    }
    let rows = q.conn.prepare("SELECT payload FROM content_format_batches WHERE library=?1 AND state='queued' ORDER BY rowid")?
        .query_map([library.to_string_lossy()],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let now = Utc::now().timestamp_millis();
    let mut selected = None;
    for raw in rows {
        let r: Record = serde_json::from_str(&raw)?;
        if let Some(i) = r
            .children
            .iter()
            .position(|c| c.state == "queued" && c.retry_at <= now)
        {
            selected = Some((r, i));
            break;
        }
    }
    let Some((mut r, i)) = selected else {
        return Ok(false);
    };
    let item = r.frozen.targets[i].row.item_id.clone();
    let job_id = r.children[i]
        .job
        .as_ref()
        .context("content_format_unavailable")?
        .job_id
        .clone();
    match receipt(
        &library,
        &item,
        &job_id,
        &r.frozen.principal,
        r.frozen.ceiling,
    ) {
        Ok(Some(saved)) => {
            finish(&mut r, i, saved);
            done(&mut r);
            persist(q, &r)?;
            return Ok(true);
        }
        Err(_) => {
            // A temporarily unreadable commit receipt is not a definite
            // publication failure; do not redispatch or starve other targets.
            if r.children[i].job.as_ref().unwrap().stage == "committing" {
                r.children[i].retry_at = now + 30_000;
            } else {
                fail(&mut r.children[i], "failed", "library_resource_not_found");
            }
            done(&mut r);
            persist(q, &r)?;
            return Ok(true);
        }
        Ok(None) => {}
    }
    let tx = q
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    if super::super::reader_item_busy(&tx, &library, &item)? {
        fail(&mut r.children[i], "skipped", "busy");
        done(&mut r);
        drop(tx);
        persist(q, &r)?;
        return Ok(true);
    }
    tx.execute(
        "INSERT INTO content_format_active(library,item_id,batch_id) VALUES(?1,?2,?3)",
        params![library.to_string_lossy(), item, r.batch_id],
    )?;
    tx.commit()?;
    r.state = "running".into();
    r.children[i].state = "running".into();
    r.children[i].job.as_mut().unwrap().state = "running".into();
    persist(q, &r)?;
    let start = std::time::Instant::now();
    let mut result = (|| -> Result<()> {
        let (source, plan) = guard(q, &r, i, &generation)?;
        let count = r.children[i].outputs.len();
        anyhow::ensure!(
            count <= plan.windows().len(),
            "content_format_invalid_result"
        );
        if count < plan.windows().len() {
            let settings = route(
                r.frozen.settings.as_ref().unwrap(),
                r.frozen.profile.as_ref().unwrap(),
            )?;
            let profile = r.frozen.profile.clone().unwrap();
            let expected_generation = r.frozen.generation.context("content_format_quote_stale")?;
            r.children[i].job.as_mut().unwrap().stage = "formatting".into();
            let mut dispatched = false;
            let (reply, usage) = generate(
                &settings,
                &profile,
                expected_generation,
                &plan.windows()[count],
                &mut || {
                    anyhow::ensure!(!dispatched, "invalid_content_format_request");
                    let _ = guard(q, &r, i, &generation)?;
                    let child = &mut r.children[i];
                    let job = child.job.as_mut().unwrap();
                    // Each automatic retry re-sends at most one window, so the request
                    // runaway guard grows by exactly the retries already granted.
                    anyhow::ensure!(
                        (job.usage.requests_dispatched as usize)
                            < job.budget.max_requests + child.auto_retries as usize,
                        "content_format_quote_stale"
                    );
                    child.in_flight = true;
                    job.usage.dispatched();
                    persist(q, &r)?;
                    dispatched = true;
                    Ok(())
                },
            )?;
            anyhow::ensure!(dispatched, "content_format_unavailable");
            r.children[i].in_flight = false;
            r.children[i].job.as_mut().unwrap().usage.observed(usage);
            plan.windows()[count].validate_output(&reply)?;
            r.children[i].outputs.push(reply);
            persist(q, &r)?;
        }
        if r.children[i].outputs.len() == plan.windows().len() {
            let applied = plan.apply(&source.markdown, &r.children[i].outputs)?;
            r.children[i].coverage = Some(plan.coverage(&r.children[i].outputs)?);
            r.children[i].job.as_mut().unwrap().stage = "committing".into();
            persist(q, &r)?;
            let provenance = serde_json::json!({"policy_version":POLICY,"route":r.frozen.preview.quote.as_ref().unwrap().route,
                "prompt_version":planner::PROMPT_VERSION,"prompt_sha256":plan.identity().prompt_sha256,"plan_sha256":plan.identity().sha256()});
            let saved = publish(
                &library,
                source.identity,
                &job_id,
                &r.frozen.principal,
                &applied,
                provenance,
                r.frozen.ceiling,
            )?;
            finish(&mut r, i, saved);
        } else {
            r.children[i].state = "queued".into();
            r.children[i].job.as_mut().unwrap().state = "queued".into();
        }
        Ok(())
    })();
    // Publication may be visible even when its acknowledgement failed. Resolve
    // that journal/receipt before classifying the operation as a failure.
    if result.is_err() && r.children[i].job.as_ref().unwrap().stage == "committing" {
        if let Ok(Some(saved)) = receipt(
            &library,
            &item,
            &job_id,
            &r.frozen.principal,
            r.frozen.ceiling,
        ) {
            finish(&mut r, i, saved);
            result = Ok(());
        }
    }
    if let Err(error) = result {
        let child = &mut r.children[i];
        let known = error
            .downcast_ref::<memex_provider::generation::GenerationError>()
            .is_some_and(|e| {
                !matches!(
                    e,
                    memex_provider::generation::GenerationError::OutcomeUnknown
                )
            });
        let reason = match error.to_string().as_str() {
            "library_resource_not_found" => "library_resource_not_found",
            "scope_changed" => "scope_changed",
            "stale_item_revision" => "stale_revision",
            "content_format_quote_stale" => "content_format_quote_stale",
            _ => "content_format_invalid_result",
        };
        let model_step = (child.in_flight && !known) || reason == "content_format_invalid_result";
        let committing = child.job.as_ref().unwrap().stage == "committing";
        if model_step && !committing && crate::queue::auto_retryable(&error) && retry_later(child, now) {
            // Bounded automatic retry of the model window.
        } else if child.in_flight && !known {
            fail(child, "outcome_unknown", "provider_outcome_unknown");
        } else if error.to_string() == "content_format_quote_stale" {
            child.in_flight = false;
            child.state = "queued".into();
            child.job.as_mut().unwrap().state = "queued".into();
            r.state = "paused".into();
            r.error_code = Some("content_format_quote_stale".into());
        } else if child.job.as_ref().unwrap().stage == "committing"
            && error.chain().any(|e| e.is::<std::io::Error>())
        {
            child.state = "queued".into();
            child.retry_at = now + 30_000;
            child.job.as_mut().unwrap().state = "queued".into();
        } else {
            let reason = match error.to_string().as_str() {
                "library_resource_not_found" => "library_resource_not_found",
                "scope_changed" => "scope_changed",
                "stale_item_revision" => "stale_revision",
                _ => "content_format_invalid_result",
            };
            fail(child, "failed", reason);
            // Bounded, content-free site of an invalid model reply; error_code
            // and the target reason stay content_format_invalid_result.
            if let Some(site) = error.downcast_ref::<planner::Error>().and_then(planner::Error::failure_reason) {
                child.job.as_mut().unwrap().failure_reason = Some(site.into());
            }
        }
    }
    r.children[i].job.as_mut().unwrap().elapsed_ms +=
        start.elapsed().as_millis().min(u64::MAX as u128) as u64;
    done(&mut r);
    persist(q, &r)?;
    q.conn.execute(
        "DELETE FROM content_format_active WHERE library=?1 AND item_id=?2 AND batch_id=?3",
        params![library.to_string_lossy(), item, r.batch_id],
    )?;
    Ok(true)
}

fn finish(r: &mut Record, i: usize, receipt: store::Receipt) {
    let child = &mut r.children[i];
    child.state = "succeeded".into();
    child.in_flight = false;
    let job = child.job.as_mut().unwrap();
    job.state = "succeeded".into();
    job.stage = "done".into();
    job.result_revision_id = Some(receipt.result_revision_id);
    let bytes = r.frozen.targets[i].row.source_bytes.unwrap_or(0);
    let (checked, unresolved) = child.coverage.unwrap_or((0, 0));
    child.formatting = Some(
        serde_json::json!({"status":if checked==bytes && unresolved==0 {"complete"}else{"partial"},"changed":receipt.changed,"applied_count":receipt.applied_count,
        "correction_count":receipt.correction_count,"deleted_blocks":receipt.deleted_blocks,"unresolved_count":unresolved,
        "checked_bytes":checked,"total_bytes":bytes,"changes_version":receipt.changes_version,"quality":"not_certified"}),
    );
}
