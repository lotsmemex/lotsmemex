//! Frozen whole-book consent. Children are isolated from the legacy executor.
use super::*;
use crate::queue::reader_authority::{Authority, Check, Phase};
use reader::paragraphs::ArtifactBinding;
use reader_translation::planner::Budget as TotalBudget;
use serde_json::{json, Value};

const POLICY: &str = "reader-repair-batch-v1";

#[cfg(test)]
mod tests;

#[derive(Clone, Serialize, Deserialize)]
struct Chapter {
    plan: Plan,
    artifact: Option<ArtifactBinding>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Batch {
    id: String,
    library: PathBuf,
    principal: String,
    authority: Authority,
    item: String,
    revision: String,
    current_revision: String,
    ceiling: Sensitivity,
    state: String,
    request_id: Option<String>,
    expires_at: Option<i64>,
    settings: Option<Snapshot>,
    profile: Option<Profile>,
    credential_generation: Option<u64>,
    policy: String,
    prompt_hash: String,
    chapters: Vec<Chapter>,
    budget: Option<TotalBudget>,
    stop_reason: Option<String>,
}

pub(crate) fn init(conn: &Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS reader_repair_batches(
      batch_id TEXT PRIMARY KEY,library TEXT NOT NULL,principal TEXT NOT NULL,item_id TEXT NOT NULL,
      request_id TEXT,state TEXT NOT NULL,payload TEXT NOT NULL,UNIQUE(library,principal,request_id));
      CREATE TABLE IF NOT EXISTS reader_repair_batch_children(
      job_id TEXT PRIMARY KEY,batch_id TEXT NOT NULL,ordinal INTEGER NOT NULL,state TEXT NOT NULL,
      payload TEXT NOT NULL,UNIQUE(batch_id,ordinal));")?;
    Ok(())
}

fn save(conn: &Connection, b: &Batch) -> Result<()> {
    conn.execute(
        "UPDATE reader_repair_batches SET state=?1,request_id=?2,payload=?3 WHERE batch_id=?4",
        params![b.state, b.request_id, serde_json::to_string(b)?, b.id],
    )?;
    Ok(())
}

fn load(conn: &Connection, id: &str) -> Result<Batch> {
    let raw: String = conn
        .query_row(
            "SELECT payload FROM reader_repair_batches WHERE batch_id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?
        .context("reader_batch_not_found")?;
    Ok(serde_json::from_str(&raw)?)
}

fn children(conn: &Connection, id: &str) -> Result<Vec<Record>> {
    let raw = conn
        .prepare(
            "SELECT payload FROM reader_repair_batch_children WHERE batch_id=?1 ORDER BY ordinal",
        )?
        .query_map([id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    raw.into_iter()
        .map(|r| Ok(serde_json::from_str(&r)?))
        .collect()
}

fn access(b: &Batch, library: &Path, principal: &str, ceiling: Sensitivity) -> Result<()> {
    anyhow::ensure!(
        b.library == library.canonicalize()? && b.principal == principal,
        "reader_batch_not_found"
    );
    let (_, current) =
        memex_store::read_item_at_sensitivity(library, &b.item, ceiling.min(b.ceiling))?
            .context("library_resource_not_found")?;
    reader::reader_document(
        library,
        &b.item,
        &current.revision_id,
        ceiling.min(b.ceiling),
    )?;
    Ok(())
}

fn routed(settings: &Snapshot, profile: &Profile) -> Result<Snapshot> {
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
    Ok(routed)
}

fn unchanged(
    q: &Queue,
    b: &Batch,
    generation: &impl Fn(&Path, &Snapshot) -> Result<u64>,
) -> Result<()> {
    let settings = b.settings.as_ref().context("reader_batch_quote_stale")?;
    let profile = b.profile.as_ref().context("reader_batch_quote_stale")?;
    anyhow::ensure!(
        b.policy == POLICY
            && b.prompt_hash == reader::reader_sha256(matching::PROMPT.as_bytes())
            && memex_models::read(&q.conn)? == *settings
            && memex_models::low_dial::read(&q.conn)?.as_ref() == Some(profile)
            && Some(generation(q.database_path(), &routed(settings, profile)?)?)
                == b.credential_generation,
        "reader_batch_quote_stale"
    );
    Ok(())
}

fn preview_view(b: &Batch) -> Result<Value> {
    let quote = if b.state == "ready" && !b.chapters.is_empty() {
        let snapshot = routed(b.settings.as_ref().unwrap(), b.profile.as_ref().unwrap())?;
        let (provider, model) = snapshot
            .configuration
            .target(Purpose::CaptureQuality)
            .unwrap();
        let rows: Vec<_> = b
            .chapters
            .iter()
            .map(|c| {
                let o = &c.plan.identity.original;
                let t = c.plan.identity.translation.as_ref().unwrap();
                json!({"chapter":o.chapter,"counterpart":t.chapter,"original_sha256":o.sha256,
                "original_bytes":o.bytes,"translation_sha256":t.sha256,"translation_bytes":t.bytes,
                "expected_alignment_version":c.plan.expected_alignment_version})
            })
            .collect();
        Some(
            json!({"batch_id":b.id,"expires_at":b.expires_at,"item_id":b.item,"revision_id":b.revision,
            "chapters":rows,"route":{"provider_id":provider.id,"model_id":model.model_id,
            "reasoning_effort":b.profile.as_ref().unwrap().reasoning_effort},"budget":b.budget,"estimated_cost":null}),
        )
    } else {
        None
    };
    Ok(json!({"batch_id":b.id,"state":b.state,"quote":quote,"error_code":b.stop_reason}))
}

/// Local preparation is queued; no chapter scan or credential read on the RPC path.
pub fn preview(
    q: &Queue,
    library: &Path,
    origin: Authority,
    item: &str,
    revision: &str,
    ceiling: Sensitivity,
    authority: impl Fn(Check<'_>) -> Result<Sensitivity>,
) -> Result<Value> {
    let library = library.canonicalize()?;
    let ceiling = origin.check(&library, item, Phase::Preview, ceiling, &authority)?;
    init(&q.conn)?;
    let (_, current) = memex_store::read_item_at_sensitivity(&library, item, ceiling)?
        .context("library_resource_not_found")?;
    anyhow::ensure!(current.revision_id == revision, "stale_item_revision");
    anyhow::ensure!(
        !reader_item_busy(&q.conn, &library, item)?,
        "reader_job_in_progress"
    );
    let b = Batch {
        id: memex_core::new_id("batch"),
        library,
        principal: origin.principal().into(),
        authority: origin,
        item: item.into(),
        revision: revision.into(),
        current_revision: revision.into(),
        ceiling,
        state: "preparing".into(),
        request_id: None,
        expires_at: None,
        settings: None,
        profile: None,
        credential_generation: None,
        policy: POLICY.into(),
        prompt_hash: reader::reader_sha256(matching::PROMPT.as_bytes()),
        chapters: vec![],
        budget: None,
        stop_reason: None,
    };
    q.conn.execute("INSERT INTO reader_repair_batches(batch_id,library,principal,item_id,state,payload) VALUES(?1,?2,?3,?4,?5,?6)",
        params![b.id,b.library.to_string_lossy(),b.principal,b.item,b.state,serde_json::to_string(&b)?])?;
    preview_view(&b)
}

pub fn preview_status(
    q: &Queue,
    library: &Path,
    principal: &str,
    item: &str,
    id: &str,
    ceiling: Sensitivity,
    authority: impl Fn(Check<'_>) -> Result<Sensitivity>,
) -> Result<Value> {
    let b = load(&q.conn, id)?;
    let ceiling = b
        .authority
        .check(library, item, Phase::Preview, ceiling, &authority)?;
    access(&b, library, principal, ceiling)?;
    anyhow::ensure!(b.item == item, "reader_batch_not_found");
    preview_view(&b)
}

pub fn prepare_one(
    q: &Queue,
    library: &Path,
    generation: impl Fn(&Path, &Snapshot) -> Result<u64>,
    authority: impl Fn(Check<'_>) -> Result<Sensitivity>,
) -> Result<bool> {
    let library = library.canonicalize()?;
    let id: Option<String> = q.conn.query_row("SELECT batch_id FROM reader_repair_batches WHERE library=?1 AND state='preparing' ORDER BY rowid LIMIT 1",
        [library.to_string_lossy()], |r| r.get(0)).optional()?;
    let Some(id) = id else { return Ok(false) };
    let mut b = load(&q.conn, &id)?;
    let result = (|| -> Result<()> {
        let ceiling =
            b.authority
                .check(&library, &b.item, Phase::Preview, b.ceiling, &authority)?;
        access(&b, &library, &b.principal, ceiling)?;
        anyhow::ensure!(
            !reader_item_busy(&q.conn, &library, &b.item)?,
            "reader_job_in_progress"
        );
        b.chapters = memex_store::with_consistent_snapshot(&library, |root| {
            let doc = reader::reader_document(root, &b.item, &b.revision, ceiling)?;
            let mut chapters = vec![];
            if let Some(original) = doc.editions.iter().find(|e| e.side == "original") {
                for ch in &original.chapters {
                    if ch.counterpart.is_none() {
                        continue;
                    }
                    let plan = reader::repair::plan(root, &b.item, &b.revision, &ch.key, ceiling)?;
                    if matching::status(&plan.pairs) == "aligned" {
                        continue;
                    }
                    let artifact = reader::paragraphs::artifact_binding(
                        root,
                        &b.item,
                        &b.revision,
                        &ch.key,
                        ceiling,
                    )?;
                    chapters.push(Chapter { plan, artifact });
                }
            }
            Ok(chapters)
        })?;
        if !b.chapters.is_empty() {
            let settings = memex_models::read(&q.conn)?;
            let profile =
                memex_models::low_dial::read(&q.conn)?.context("no_low_dial_provider")?;
            b.credential_generation = Some(generation(
                q.database_path(),
                &routed(&settings, &profile)?,
            )?);
            let n = b
                .chapters
                .len()
                .checked_mul(2)
                .context("reader_repair_budget_exceeded")?;
            b.budget = Some(TotalBudget {
                max_requests: n,
                max_input_bytes_per_request: profile.max_input_bytes,
                max_completion_tokens_per_request: profile.max_completion_tokens as usize,
                max_total_input_bytes: n
                    .checked_mul(profile.max_input_bytes)
                    .context("reader_repair_budget_exceeded")?,
                max_total_completion_tokens: n
                    .checked_mul(profile.max_completion_tokens as usize)
                    .context("reader_repair_budget_exceeded")?,
            });
            b.settings = Some(settings);
            b.profile = Some(profile);
        }
        b.expires_at = Some(Utc::now().timestamp_millis() + 15 * 60 * 1000);
        Ok(())
    })();
    match result {
        Ok(()) => b.state = "ready".into(),
        Err(e) => {
            b.state = "failed".into();
            b.stop_reason = Some(code(&e));
        }
    }
    save(&q.conn, &b)?;
    Ok(true)
}

fn code(e: &anyhow::Error) -> String {
    match e.to_string().as_str() {
        "library_resource_not_found"
        | "permission_denied"
        | "reader_job_in_progress"
        | "stale_item_revision"
        | "stale_chapter_alignment"
        | "reader_batch_quote_stale"
        | "no_low_dial_provider"
        | "reader_repair_budget_exceeded" => e.to_string(),
        _ => "reader_repair_failed".into(),
    }
}

fn view(conn: &Connection, b: &Batch) -> Result<Value> {
    let jobs = children(conn, &b.id)?;
    let mut requests = 0usize;
    let mut unknown = 0usize;
    let mut total = 0u64;
    let mut input = Some(0u64);
    let mut output = Some(0u64);
    for c in &jobs {
        requests += c.job.usage.requests_dispatched as usize;
        unknown += c.job.usage.unknown_usage_requests as usize;
        total = total
            .checked_add(c.job.usage.reported_total_tokens)
            .context("reader_repair_budget_exceeded")?;
        input = input.and_then(|n| n.checked_add(c.job.usage.input_tokens?));
        output = output.and_then(|n| n.checked_add(c.job.usage.output_tokens?));
    }
    Ok(
        json!({"batch_id":b.id,"request_id":b.request_id,"state":b.state,"stop_reason":b.stop_reason,
        "done":jobs.iter().filter(|c| c.job.state == "succeeded").count(),"total":b.chapters.len(),
        "current_job_id":jobs.iter().find(|c| matches!(c.job.state.as_str(),"queued"|"running")).map(|c| &c.job.job_id),
        "usage":{"requests_dispatched":requests,"reported_total_tokens":total,"unknown_usage_requests":unknown,
            "input_tokens":input,"output_tokens":output}}),
    )
}

pub fn start_with(
    q: &mut Queue,
    library: &Path,
    principal: &str,
    item: &str,
    id: &str,
    request_id: &str,
    accept_cost: bool,
    accept_model_processing: bool,
    ceiling: Sensitivity,
    generation: impl Fn(&Path, &Snapshot) -> Result<u64>,
    authority: impl Fn(Check<'_>) -> Result<Sensitivity>,
) -> Result<Value> {
    anyhow::ensure!(
        accept_cost
            && accept_model_processing
            && (1..=128).contains(&request_id.len())
            && request_id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-'),
        "invalid_reader_alignment_request"
    );
    let mut b = load(&q.conn, id)?;
    let ceiling = b
        .authority
        .check(library, item, Phase::Admission, ceiling, &authority)?;
    access(&b, library, principal, ceiling)?;
    anyhow::ensure!(b.item == item, "reader_batch_not_found");
    if b.request_id.as_deref() == Some(request_id) {
        return Ok(json!({"batch":view(&q.conn,&b)?}));
    }
    anyhow::ensure!(b.request_id.is_none(), "reader_batch_quote_consumed");
    unchanged(q, &b, &generation)?;
    // Before consuming the quote validate every frozen artifact. The worker
    // repeats revision/artifact CAS before dispatch and publication.
    for i in 0..b.chapters.len() {
        current_plan(&b, i, ceiling)?;
    }
    let tx = q
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current = load(&tx, id)?;
    anyhow::ensure!(
        current.request_id.is_none()
            && current.state == "ready"
            && !b.chapters.is_empty()
            && current
                .expires_at
                .is_some_and(|e| e > Utc::now().timestamp_millis()),
        "reader_batch_quote_stale"
    );
    let reused: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM reader_repair_batches WHERE library=?1 AND principal=?2 AND request_id=?3)",
        params![b.library.to_string_lossy(),principal,request_id], |r| r.get(0))?;
    anyhow::ensure!(!reused, "request_identity_conflict");
    anyhow::ensure!(
        !reader_item_busy(&tx, &b.library, item)?,
        "reader_job_in_progress"
    );
    b.request_id = Some(request_id.into());
    b.state = "queued".into();
    save(&tx, &b)?;
    tx.commit()?;
    Ok(json!({"batch":view(&q.conn,&b)?}))
}

fn current_plan(b: &Batch, i: usize, ceiling: Sensitivity) -> Result<Plan> {
    memex_store::with_consistent_snapshot(&b.library, |root| {
        let frozen = &b.chapters[i];
        let fresh = reader::repair::plan(
            root,
            &b.item,
            &b.current_revision,
            &frozen.plan.identity.original.chapter,
            ceiling,
        )?;
        let same_side = |a: &reader::paragraphs::Side, z: &reader::paragraphs::Side| {
            a.chapter == z.chapter && a.sha256 == z.sha256 && a.bytes == z.bytes
        };
        anyhow::ensure!(
            same_side(&frozen.plan.identity.original, &fresh.identity.original)
                && frozen
                    .plan
                    .identity
                    .translation
                    .as_ref()
                    .zip(fresh.identity.translation.as_ref())
                    .is_some_and(|(a, z)| same_side(a, z)),
            "stale_chapter_alignment"
        );
        let binding = reader::paragraphs::artifact_binding(
            root,
            &b.item,
            &b.current_revision,
            &fresh.identity.original.chapter,
            ceiling,
        )?;
        anyhow::ensure!(binding == frozen.artifact, "stale_chapter_alignment");
        Ok(fresh)
    })
}

pub fn status(
    q: &Queue,
    library: &Path,
    principal: &str,
    item: &str,
    id: &str,
    offset: usize,
    limit: usize,
    ceiling: Sensitivity,
    authority: impl Fn(Check<'_>) -> Result<Sensitivity>,
) -> Result<Value> {
    anyhow::ensure!((1..=100).contains(&limit), "invalid_reader_limit");
    let b = load(&q.conn, id)?;
    let ceiling = b
        .authority
        .check(library, item, Phase::Read, ceiling, &authority)?;
    access(&b, library, principal, ceiling)?;
    anyhow::ensure!(b.item == item, "reader_batch_not_found");
    anyhow::ensure!(offset <= b.chapters.len(), "invalid_reader_offset");
    let jobs = children(&q.conn, id)?;
    let end = offset.saturating_add(limit).min(b.chapters.len());
    let rows: Vec<_> = (offset..end).map(|i| {
        let job = jobs.get(i).map(|c| &c.job);
        json!({"chapter":b.chapters[i].plan.identity.original.chapter,"job_id":job.map(|j| &j.job_id),
            "state":job.map(|j|j.state.as_str()).unwrap_or("pending"),"error_code":job.and_then(|j|j.error_code.as_ref()),
            "result_revision_id":job.and_then(|j|j.result_revision_id.as_ref()),
            "alignment_version":job.and_then(|j|j.alignment_version.as_ref()),
            "alignment_status":job.and_then(|j|j.alignment_status.as_ref())})
    }).collect();
    Ok(
        json!({"batch":view(&q.conn,&b)?,"offset":offset,"total":b.chapters.len(),
        "next_offset":(end < b.chapters.len()).then_some(end),"chapters":rows}),
    )
}

pub fn stop(
    q: &mut Queue,
    library: &Path,
    principal: &str,
    item: &str,
    id: &str,
    ceiling: Sensitivity,
    authority: impl Fn(Check<'_>) -> Result<Sensitivity>,
) -> Result<Value> {
    let observed = load(&q.conn, id)?;
    let ceiling = observed
        .authority
        .check(library, item, Phase::Stop, ceiling, &authority)?;
    let tx = q
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut b = load(&tx, id)?;
    access(&b, library, principal, ceiling)?;
    anyhow::ensure!(b.item == item, "reader_batch_not_found");
    if matches!(b.state.as_str(), "queued" | "running" | "stopping") {
        let active = children(&tx, id)?
            .iter()
            .any(|c| matches!(c.job.state.as_str(), "queued" | "running"));
        b.state = if active { "stopping" } else { "stopped" }.into();
        b.stop_reason = Some("requested".into());
        save(&tx, &b)?;
    }
    tx.commit()?;
    Ok(json!({"batch":view(&q.conn,&b)?}))
}

pub(super) fn put_child(q: &Queue, r: &Record) -> Result<()> {
    let id = r.batch_id.as_ref().context("reader_batch_not_found")?;
    let tx = q.conn.unchecked_transaction()?;
    let b = load(&tx, id)?;
    anyhow::ensure!(
        matches!(b.state.as_str(), "running" | "stopping"),
        "reader_batch_quote_stale"
    );
    // Aggregate reservations are the durable child intents themselves, not an
    // independently updated counter that can drift after a crash.
    let previous = children(&tx, id)?;
    let requests: usize = previous
        .iter()
        .filter(|c| c.job.job_id != r.job.job_id)
        .map(|c| c.job.usage.requests_dispatched as usize)
        .sum::<usize>()
        + r.job.usage.requests_dispatched as usize;
    anyhow::ensure!(
        requests
            <= b.budget
                .as_ref()
                .context("reader_batch_quote_stale")?
                .max_requests
            && r.job.usage.requests_dispatched <= r.job.budget.max_requests,
        "reader_repair_budget_exceeded"
    );
    anyhow::ensure!(tx.execute("UPDATE reader_repair_batch_children SET state=?1,payload=?2 WHERE job_id=?3 AND batch_id=?4",
        params![r.job.state,serde_json::to_string(r)?,r.job.job_id,id])? == 1, "reader_batch_not_found");
    tx.commit()?;
    Ok(())
}

fn new_child(b: &Batch, i: usize, plan: Plan) -> Record {
    let profile = b.profile.clone().unwrap();
    let request_id = format!(
        "batch_{}",
        reader::reader_sha256(format!("memex.repair.child.v1:{}:{i}", b.id).as_bytes())
    );
    Record {
        batch_id: Some(b.id.clone()),
        dispatch_in_flight: false,
        job: Job {
            job_id: memex_core::new_id("job"),
            operation: repair_operation(),
            failure_stage: None,
            failure_reason: None,
            request: Request {
                item_id: b.item.clone(),
                revision_id: b.current_revision.clone(),
                chapter: plan.identity.original.chapter.clone(),
                request_id,
                expected_alignment_version: plan.expected_alignment_version.clone(),
            },
            state: "running".into(),
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
            usage: Usage {
                dispatches: Some(vec![]),
                input_tokens: Some(0),
                output_tokens: Some(0),
                ..Usage::default()
            },
            elapsed_ms: 0,
        },
        library: b.library.clone(),
        ceiling: b.ceiling,
        settings: b.settings.clone().unwrap(),
        profile,
        plan,
        apply: true,
        started_at_ms: Some(Utc::now().timestamp_millis()),
        failure_stage: None,
        failure_reason: None,
        rejected_groups: vec![],
        parser_policy: matching::PARSER_POLICY,
        auto_retries: 0,
        retry_at: 0,
    }
}

/// One admitted chapter, never a legacy queued row or nested executor lock.
pub fn step_with(
    q: &mut Queue,
    library: &Path,
    generation: impl Fn(&Path, &Snapshot) -> Result<u64>,
    authority: impl Fn(Check<'_>) -> Result<Sensitivity>,
    generate: impl FnOnce(
        &Path,
        &Snapshot,
        &Profile,
        &str,
        &mut dyn FnMut() -> Result<()>,
    ) -> Result<(String, DispatchUsage)>,
) -> Result<bool> {
    let Some(_execution) = execution_lock(q)? else {
        return Ok(false);
    };
    let library = library.canonicalize()?;
    if prepare_one(q, &library, &generation, &authority)? {
        return Ok(true);
    }
    let id: Option<String> = q.conn.query_row("SELECT batch_id FROM reader_repair_batches WHERE library=?1 AND state='queued' ORDER BY rowid LIMIT 1",
        [library.to_string_lossy()],|r|r.get(0)).optional()?;
    let Some(id) = id else { return Ok(false) };
    let mut b = load(&q.conn, &id)?;
    let jobs = children(&q.conn, &id)?;
    let i = jobs.len();
    let admitted = (|| -> Result<Record> {
        let ceiling =
            b.authority
                .check(&library, &b.item, Phase::Admission, b.ceiling, &authority)?;
        access(&b, &library, &b.principal, ceiling)?;
        unchanged(q, &b, &generation)?;
        anyhow::ensure!(
            i < b.chapters.len() && jobs.iter().all(|c| c.job.state == "succeeded"),
            "reader_batch_quote_stale"
        );
        let plan = current_plan(&b, i, ceiling)?;
        let child = new_child(&b, i, plan);
        let tx = q
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        anyhow::ensure!(
            load(&tx, &id)?.state == "queued",
            "reader_batch_quote_stale"
        );
        b.state = "running".into();
        save(&tx, &b)?;
        tx.execute("INSERT INTO reader_repair_batch_children(job_id,batch_id,ordinal,state,payload) VALUES(?1,?2,?3,?4,?5)",
            params![child.job.job_id,id,i as i64,child.job.state,serde_json::to_string(&child)?])?;
        tx.commit()?;
        Ok(child)
    })();
    let child = match admitted {
        Ok(child) => child,
        Err(e) => {
            // A concurrent stop wins; never overwrite it from a stale in-memory copy.
            b = load(&q.conn, &id)?;
            if b.state == "queued" {
                b.state = "failed".into();
                b.stop_reason = Some(code(&e));
                save(&q.conn, &b)?;
            }
            return Ok(true);
        }
    };
    execute_record(
        q,
        &library,
        child,
        |q, r| {
            let b = load(&q.conn, &id)?;
            let phase = if r.job.stage == "committing" {
                Phase::Commit
            } else {
                Phase::Dispatch
            };
            let ceiling = b
                .authority
                .check(&library, &b.item, phase, b.ceiling, &authority)?;
            access(&b, &library, &b.principal, ceiling)?;
            unchanged(q, &b, &generation)?;
            anyhow::ensure!(
                r.job.request.revision_id == b.current_revision,
                "stale_item_revision"
            );
            // Authorize the exact source again at the live ceiling, including Evidence.
            reader::reader_document(&library, &b.item, &b.current_revision, ceiling)?;
            Ok(())
        },
        generate,
    )?;
    finish(q, &id)?;
    Ok(true)
}

fn finish(q: &Queue, id: &str) -> Result<()> {
    let tx = q.conn.unchecked_transaction()?;
    let mut b = load(&tx, id)?;
    let jobs = children(&tx, id)?;
    let child = jobs.last().context("reader_batch_not_found")?;
    if child.job.state == "succeeded" {
        b.current_revision = child
            .job
            .result_revision_id
            .clone()
            .context("reader_repair_failed")?;
        b.state = if b.state == "stopping" {
            "stopped"
        } else if jobs.len() == b.chapters.len() {
            "succeeded"
        } else {
            "queued"
        }
        .into();
    } else {
        b.state = if child.job.state == "outcome_unknown" {
            "outcome_unknown"
        } else {
            "failed"
        }
        .into();
        b.stop_reason = child.job.error_code.clone();
    }
    save(&tx, &b)?;
    tx.commit()?;
    Ok(())
}

/// Call once on executor startup. Reconcile existing results only: recovery
/// never adds a new commit or dispatch under expired consent/configuration.
pub fn recover(
    q: &Queue,
    library: &Path,
    _authority: impl Fn(Check<'_>) -> Result<Sensitivity>,
) -> Result<()> {
    let Some(_execution) = execution_lock(q)? else {
        return Ok(());
    };
    let library = library.canonicalize()?;
    let ids=q.conn.prepare("SELECT batch_id FROM reader_repair_batches WHERE library=?1 AND state IN ('queued','running','stopping','outcome_unknown')")?
        .query_map([library.to_string_lossy()],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    for id in ids {
        let mut b = load(&q.conn, &id)?;
        let jobs = children(&q.conn, &id)?;
        for mut c in jobs {
            if !matches!(
                c.job.state.as_str(),
                "running" | "queued" | "outcome_unknown"
            ) {
                continue;
            }
            if c.job.stage == "committing" {
                match reader::repair::committed(&library, &c.plan, c.ceiling) {
                    Ok(Some(saved)) => {
                        c.job.state = "succeeded".into();
                        c.job.result_revision_id = Some(saved.revision_id.clone());
                        c.job.alignment_version = Some(saved.alignment_version);
                        c.job.alignment_status = Some(matching::status(&c.plan.pairs).into());
                        b.current_revision = saved.revision_id;
                    }
                    Ok(None) => c.job.state = "stopped".into(),
                    Err(_) => c.job.state = "outcome_unknown".into(),
                }
            } else if c.dispatch_in_flight {
                c.job.state = "outcome_unknown".into();
            } else {
                c.job.state = "stopped".into();
            }
            c.job.error_code = (c.job.state != "succeeded").then(|| {
                if c.job.state == "outcome_unknown" {
                    if c.job.stage == "committing" {
                        "reader_commit_outcome_unknown"
                    } else {
                        "provider_outcome_unknown"
                    }
                } else {
                    "interrupted"
                }
                .into()
            });
            q.conn.execute(
                "UPDATE reader_repair_batch_children SET state=?1,payload=?2 WHERE job_id=?3",
                params![c.job.state, serde_json::to_string(&c)?, c.job.job_id],
            )?;
        }
        b.state = if children(&q.conn, &id)?
            .iter()
            .any(|c| c.job.state == "outcome_unknown")
        {
            "outcome_unknown"
        } else {
            "stopped"
        }
        .into();
        b.stop_reason = Some("interrupted".into());
        save(&q.conn, &b)?;
    }
    Ok(())
}
