//! Selected-content formatting planning. Durable admission and execution are not
//! exposed until consent, per-window guards, proof publication and restore exist.
pub mod jobs;
pub mod planner;
#[cfg(test)]
mod preview_tests;
#[cfg(test)]
mod tests;

use super::*;
use memex_core::Sensitivity;
use memex_models::{low_dial::Profile, Purpose, Snapshot};
use memex_store::reader::content_format as store;
use std::collections::{BTreeMap, BTreeSet};

const POLICY: &str = "content-format-v1";
const SAFE: usize = 9_007_199_254_740_991;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Selection {
    Item {
        item_id: String,
        revision_id: String,
        target: store::Target,
    },
    Group {
        group_id: String,
        include_descendants: bool,
    },
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Target {
    pub target_id: String,
    pub item_id: String,
    pub revision_id: String,
    pub target: store::Target,
    pub source_sha256: Option<String>,
    pub source_bytes: Option<usize>,
    pub state: String,
    pub reason: Option<String>,
    pub budget: Option<planner::Budget>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Quote {
    pub quote_id: String,
    pub expires_at: i64,
    pub selection: Selection,
    pub policy_version: String,
    pub route: Option<serde_json::Value>,
    pub counts: serde_json::Value,
    pub budget: serde_json::Value,
    pub estimated_cost: Option<serde_json::Value>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Preview {
    pub preview_id: String,
    pub state: String,
    pub quote: Option<Quote>,
    pub error_code: Option<String>,
    /// Additive: the batch a one-step preview (`auto_request_id`) was admitted as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct FrozenTarget {
    row: Target,
    identity: Option<store::Identity>,
    plan_sha256: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Frozen {
    preview: Preview,
    library: PathBuf,
    principal: String,
    ceiling: Sensitivity,
    selection: Selection,
    targets: Vec<FrozenTarget>,
    // Old payloads already contain their complete expansion. New directory
    // intents acquire their Store snapshot on the durable worker, not the RPC.
    #[serde(default)]
    expansion_pending: bool,
    settings: Option<Snapshot>,
    profile: Option<Profile>,
    generation: Option<u64>,
    /// One-step request: once ready, the worker admits the quote under this request id with
    /// cost and model processing accepted (the owner's single click). Absent: two-step flow.
    #[serde(default)]
    auto_request_id: Option<String>,
}

// Queue-owned private payloads, never Debug or returned as public job metadata.
fn init(q: &Queue) -> Result<()> {
    q.conn.execute_batch("CREATE TABLE IF NOT EXISTS content_format_previews (
        preview_id TEXT PRIMARY KEY, quote_id TEXT UNIQUE, library TEXT NOT NULL,
        principal TEXT NOT NULL, state TEXT NOT NULL, consumed_by TEXT UNIQUE, payload TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS content_format_batches (
        batch_id TEXT PRIMARY KEY, library TEXT NOT NULL, principal TEXT NOT NULL,
        request_id TEXT NOT NULL, quote_id TEXT NOT NULL UNIQUE, state TEXT NOT NULL,
        payload TEXT NOT NULL, UNIQUE(library,principal,request_id));")?;
    Ok(())
}

fn identifier(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

fn members(
    library: &Path,
    selection: &Selection,
    ceiling: Sensitivity,
) -> Result<BTreeSet<String>> {
    memex_store::with_consistent_snapshot(library, |root| match selection {
        Selection::Item { item_id, .. } => {
            anyhow::ensure!(
                memex_store::read_item_at_sensitivity(root, item_id, ceiling)?.is_some(),
                "library_resource_not_found"
            );
            Ok(BTreeSet::from([item_id.clone()]))
        }
        Selection::Group {
            group_id,
            include_descendants,
        } => {
            let groups = memex_store::read_visible_groups(root, ceiling)?;
            anyhow::ensure!(
                groups.nodes.iter().any(|g| g.group_id == *group_id),
                "library_resource_not_found"
            );
            let mut selected = BTreeSet::from([group_id.clone()]);
            if *include_descendants {
                loop {
                    let count = selected.len();
                    for node in &groups.nodes {
                        if node
                            .parent_id
                            .as_ref()
                            .is_some_and(|p| selected.contains(p))
                        {
                            selected.insert(node.group_id.clone());
                        }
                    }
                    if count == selected.len() {
                        break;
                    }
                }
            }
            let mut items = BTreeSet::new();
            for membership in &groups.memberships {
                if selected.contains(&membership.group_id)
                    && memex_store::read_item_at_sensitivity(root, &membership.item_id, ceiling)?
                        .is_some()
                {
                    items.insert(membership.item_id.clone());
                }
            }
            Ok(items)
        }
    })
}

fn authorize(f: &Frozen, ceiling: Sensitivity) -> Result<()> {
    let ceiling = ceiling.min(f.ceiling);
    authorize_selection(&f.library, &f.selection, ceiling)?;
    for target in &f.targets {
        memex_store::with_consistent_snapshot(&f.library, |root| {
            let (_, current) =
                memex_store::read_item_at_sensitivity(root, &target.row.item_id, ceiling)?
                    .context("library_resource_not_found")?;
            // Current revisions retain historical Evidence; validate its
            // protection too before disclosing any complete-batch metadata.
            memex_store::reader::reader_document(
                root,
                &target.row.item_id,
                &current.revision_id,
                ceiling,
            )?;
            Ok(())
        })?;
        if let Some(identity) = &target.identity {
            anyhow::ensure!(
                identity.sensitivity <= ceiling,
                "library_resource_not_found"
            );
        }
    }
    Ok(())
}

fn authorize_selection(library: &Path, selection: &Selection, ceiling: Sensitivity) -> Result<()> {
    memex_store::with_consistent_snapshot(library, |root| {
        let visible = match selection {
            Selection::Item { item_id, .. } => {
                memex_store::read_item_at_sensitivity(root, item_id, ceiling)?.is_some()
            }
            Selection::Group { group_id, .. } => memex_store::read_visible_groups(root, ceiling)?
                .nodes
                .iter()
                .any(|g| g.group_id == *group_id),
        };
        anyhow::ensure!(visible, "library_resource_not_found");
        Ok(())
    })
}

// Freeze membership and revisions together under one Store read lock. Persist
// this checkpoint before planning so a restart never adds subsequent arrivals.
fn expand(q: &Queue, f: &mut Frozen) -> Result<()> {
    let targets = memex_store::with_consistent_snapshot(&f.library, |root| {
        let ids = members(root, &f.selection, f.ceiling)?;
        ids.into_iter()
            .map(|item_id| {
                let (_, revision) =
                    memex_store::read_item_at_sensitivity(root, &item_id, f.ceiling)?
                        .context("library_resource_not_found")?;
                Ok(FrozenTarget {
                    row: Target {
                        target_id: memex_core::new_id("target"),
                        item_id,
                        revision_id: revision.revision_id,
                        target: store::Target::Body {},
                        source_sha256: None,
                        source_bytes: None,
                        state: "skipped".into(),
                        reason: None,
                        budget: None,
                    },
                    identity: None,
                    plan_sha256: None,
                })
            })
            .collect::<Result<Vec<_>>>()
    })?;
    f.targets = targets;
    f.expansion_pending = false;
    q.conn.execute(
        "UPDATE content_format_previews SET payload=?1 WHERE preview_id=?2 AND state='preparing'",
        params![serde_json::to_string(f)?, f.preview.preview_id],
    )?;
    Ok(())
}

fn load_preview(
    q: &Queue,
    library: &Path,
    principal: &str,
    id: &str,
    ceiling: Sensitivity,
) -> Result<Frozen> {
    init(q)?;
    let raw: String = q.conn.query_row("SELECT payload FROM content_format_previews WHERE preview_id=?1 AND library=?2 AND principal=?3",
        params![id,library.to_string_lossy(),principal], |r| r.get(0)).optional()?.context("content_format_preview_not_found")?;
    let f: Frozen = serde_json::from_str(&raw)?;
    authorize(&f, ceiling)?;
    Ok(f)
}

pub fn preview(
    q: &Queue,
    library: &Path,
    principal: &str,
    selection: Selection,
    ceiling: Sensitivity,
) -> Result<Preview> {
    preview_auto(q, library, principal, selection, None, ceiling)
}

/// `preview` with an optional one-step request id (see `Frozen::auto_request_id`).
pub fn preview_auto(
    q: &Queue,
    library: &Path,
    principal: &str,
    selection: Selection,
    auto_request_id: Option<String>,
    ceiling: Sensitivity,
) -> Result<Preview> {
    init(q)?;
    anyhow::ensure!(
        auto_request_id.as_deref().is_none_or(identifier),
        "invalid_content_format_request"
    );
    anyhow::ensure!(
        principal == "local_owner"
            || (principal.len() == 64
                && principal
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))),
        "invalid_content_format_request"
    );
    let library = library.canonicalize()?;
    authorize_selection(&library, &selection, ceiling)?;
    let expansion_pending = matches!(selection, Selection::Group { .. });
    let ids = match &selection {
        Selection::Item { item_id, .. } => BTreeSet::from([item_id.clone()]),
        Selection::Group { .. } => BTreeSet::new(),
    };
    let targets = ids
        .into_iter()
        .map(|item_id| {
            let (_, revision) =
                memex_store::read_item_at_sensitivity(&library, &item_id, ceiling)?
                    .context("library_resource_not_found")?;
            let target = match &selection {
                Selection::Item {
                    revision_id,
                    target,
                    ..
                } => {
                    anyhow::ensure!(revision.revision_id == *revision_id, "stale_item_revision");
                    target.clone()
                }
                Selection::Group { .. } => store::Target::Body {},
            };
            Ok(FrozenTarget {
                row: Target {
                    target_id: memex_core::new_id("target"),
                    item_id,
                    revision_id: revision.revision_id,
                    target,
                    source_sha256: None,
                    source_bytes: None,
                    state: "skipped".into(),
                    reason: None,
                    budget: None,
                },
                identity: None,
                plan_sha256: None,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let f = Frozen {
        preview: Preview {
            preview_id: memex_core::new_id("preview"),
            state: "preparing".into(),
            quote: None,
            error_code: None,
            batch_id: None,
        },
        library,
        principal: principal.into(),
        ceiling,
        selection,
        targets,
        expansion_pending,
        settings: None,
        profile: None,
        generation: None,
        auto_request_id,
    };
    q.conn.execute("INSERT INTO content_format_previews(preview_id,library,principal,state,payload) VALUES(?1,?2,?3,'preparing',?4)",
        params![f.preview.preview_id,f.library.to_string_lossy(),principal,serde_json::to_string(&f)?])?;
    Ok(f.preview)
}

pub fn preview_status(
    q: &Queue,
    library: &Path,
    principal: &str,
    id: &str,
    ceiling: Sensitivity,
) -> Result<Preview> {
    let mut preview = load_preview(q, &library.canonicalize()?, principal, id, ceiling)?.preview;
    preview.batch_id = q
        .conn
        .query_row(
            "SELECT consumed_by FROM content_format_previews WHERE preview_id=?1",
            [id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    Ok(preview)
}

/// Admits one ready one-step preview (see `Frozen::auto_request_id`) through the ordinary
/// request path, with the same quote freshness and source checks. A preview that cannot be
/// admitted ends `failed` with the request's error code, so it is never retried in a loop.
pub(crate) fn admit_one_step(
    q: &mut Queue,
    library: &Path,
    generation: impl Fn(&Path, &Snapshot) -> Result<u64>,
) -> Result<bool> {
    init(q)?;
    let library = library.canonicalize()?;
    let rows = q.conn.prepare("SELECT preview_id,quote_id,payload FROM content_format_previews WHERE library=?1 AND state='ready' AND consumed_by IS NULL AND quote_id IS NOT NULL ORDER BY rowid")?
        .query_map([library.to_string_lossy()], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (preview_id, quote_id, raw) in rows {
        let mut f: Frozen = serde_json::from_str(&raw)?;
        let Some(request_id) = f.auto_request_id.clone() else { continue };
        let admitted = jobs::request_with(q, &library, &f.principal.clone(), &quote_id, &request_id,
            true, true, f.ceiling, &generation);
        if let Err(error) = admitted {
            f.preview.state = "failed".into();
            f.preview.error_code = Some(match error.to_string().as_str() {
                "content_format_quote_stale" => "content_format_quote_stale",
                "content_format_quote_consumed" => "content_format_quote_consumed",
                _ => "content_format_unavailable",
            }.into());
            q.conn.execute("UPDATE content_format_previews SET state='failed',payload=?1 WHERE preview_id=?2 AND consumed_by IS NULL",
                params![serde_json::to_string(&f)?, preview_id])?;
        }
        return Ok(true);
    }
    Ok(false)
}

pub fn targets(
    q: &Queue,
    library: &Path,
    principal: &str,
    id: &str,
    offset: usize,
    limit: usize,
    ceiling: Sensitivity,
) -> Result<serde_json::Value> {
    anyhow::ensure!((1..=100).contains(&limit), "invalid_content_format_request");
    let f = load_preview(q, &library.canonicalize()?, principal, id, ceiling)?;
    anyhow::ensure!(offset <= f.targets.len(), "invalid_content_format_request");
    let end = offset.saturating_add(limit).min(f.targets.len());
    Ok(
        serde_json::json!({"preview_id":id,"total":f.targets.len(),"offset":offset,"next_offset":(end < f.targets.len()).then_some(end),
        "targets":f.targets[offset..end].iter().map(|t| &t.row).collect::<Vec<_>>() }),
    )
}

fn route(settings: &Snapshot, profile: &Profile) -> Result<Snapshot> {
    let mut routed = settings.clone();
    routed
        .configuration
        .selections
        .retain(|s| s.purpose != Purpose::CaptureQuality);
    routed
        .configuration
        .selections
        .push(memex_models::Selection {
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

pub(crate) fn credential_generation(database: &Path, settings: &Snapshot) -> Result<u64> {
    use memex_provider::credentials::{CredentialState, Credentials};
    let (provider, _) = settings
        .configuration
        .target(Purpose::CaptureQuality)
        .context("no_low_dial_provider")?;
    let state = Credentials::for_database(database, &provider.id)
        .status(provider, settings.revision)
        .map_err(|_| anyhow::anyhow!("content_format_unavailable"))?;
    anyhow::ensure!(
        state.state == CredentialState::Saved && state.generation > 0,
        "content_format_unavailable"
    );
    Ok(state.generation)
}

pub(crate) fn prepare_one(
    q: &Queue,
    library: &Path,
    generation: impl Fn(&Path, &Snapshot) -> Result<u64>,
) -> Result<bool> {
    init(q)?;
    let library = library.canonicalize()?;
    let raw: Option<String> = q.conn.query_row("SELECT payload FROM content_format_previews WHERE library=?1 AND state='preparing' ORDER BY rowid LIMIT 1",
        [library.to_string_lossy()], |r| r.get(0)).optional()?;
    let Some(raw) = raw else { return Ok(false) };
    let mut f: Frozen = serde_json::from_str(&raw)?;
    let result = (|| -> Result<()> {
        authorize(&f, f.ceiling)?;
        if f.expansion_pending {
            expand(q, &mut f)?;
        }
        let settings = memex_models::read(&q.conn)?;
        let profile = memex_models::low_dial::read(&q.conn)?;
        let current_members = members(&library, &f.selection, f.ceiling)?;
        let mut budget = [0usize; 3];
        let mut skipped = BTreeMap::<String, usize>::new();
        let mut private = 0;
        for target in &mut f.targets {
            if current_members.contains(&target.row.item_id)
                && jobs::already_formatted(q, &library, &target.row, f.ceiling)?
            {
                target.row.reason = Some("already_formatted".into());
                *skipped.entry("already_formatted".into()).or_default() += 1;
                continue;
            }
            let source = if current_members.contains(&target.row.item_id) {
                store::source(
                    &library,
                    &target.row.item_id,
                    &target.row.revision_id,
                    target.row.target.clone(),
                    f.ceiling,
                )
            } else {
                Err(anyhow::anyhow!("scope_changed"))
            };
            let planned = source.and_then(|source| {
                anyhow::ensure!(
                    source.identity.sensitivity != Sensitivity::Restricted,
                    "restricted"
                );
                let profile = profile.as_ref().context("no_low_dial_provider")?;
                route(&settings, profile)?;
                let limits = planner::SavedLowLimits {
                    max_input_bytes: profile.max_input_bytes,
                    max_completion_tokens: profile.max_completion_tokens as usize,
                };
                let plan =
                    planner::plan(&source.markdown, limits, &[]).map_err(|e| anyhow::anyhow!(e))?;
                Ok((source, plan))
            });
            match planned {
                Ok((source, plan)) => {
                    let b = plan.budget();
                    for (sum, n) in budget.iter_mut().zip([
                        b.max_requests,
                        b.max_total_input_bytes,
                        b.max_total_completion_tokens,
                    ]) {
                        *sum = sum
                            .checked_add(n)
                            .filter(|v| *v <= SAFE)
                            .context("content_format_unavailable")?;
                    }
                    private += usize::from(source.identity.sensitivity == Sensitivity::Private);
                    target.row.state = "eligible".into();
                    target.row.source_sha256 = Some(source.identity.source_sha256.clone());
                    target.row.source_bytes = Some(source.identity.source_bytes);
                    target.row.budget = Some(b.clone());
                    target.identity = Some(source.identity);
                    target.plan_sha256 = Some(plan.identity().sha256());
                }
                Err(e) => {
                    // Missing saved routing is a preview failure, never a claim
                    // that an otherwise supported body is unsupported.
                    if e.to_string() == "no_low_dial_provider" {
                        return Err(e);
                    }
                    let reason = match e.to_string().as_str() {
                        "chapter_required" => "chapter_required",
                        "restricted" => "restricted",
                        "stale_item_revision" => "stale_revision",
                        "scope_changed" => "scope_changed",
                        "content_format_budget_exceeded" => "budget_exceeded",
                        _ => "unsupported_body",
                    };
                    target.row.reason = Some(reason.into());
                    *skipped.entry(reason.into()).or_default() += 1;
                }
            }
        }
        let public_route = if budget[0] > 0 {
            let profile = profile.as_ref().context("no_low_dial_provider")?;
            let routed = route(&settings, profile)?;
            let g = generation(q.database_path(), &routed)?;
            anyhow::ensure!(g > 0, "content_format_unavailable");
            f.generation = Some(g);
            let (provider, model) = routed
                .configuration
                .target(Purpose::CaptureQuality)
                .unwrap();
            Some(
                serde_json::json!({"provider_id":provider.id,"model_id":model.model_id,"reasoning_effort":profile.reasoning_effort}),
            )
        } else {
            None
        };
        let count: usize = skipped.values().sum();
        f.preview.quote = Some(Quote {
            quote_id: memex_core::new_id("quote"),
            expires_at: Utc::now().timestamp_millis() + 900_000,
            selection: f.selection.clone(),
            policy_version: POLICY.into(),
            route: public_route,
            counts: serde_json::json!({"items":f.targets.len(),"targets":f.targets.len(),"eligible":f.targets.len()-count,"skipped":count,"private_targets":private,"skipped_by_reason":skipped}),
            budget: serde_json::json!({"max_requests":budget[0],"max_input_bytes":budget[1],"max_completion_tokens":budget[2]}),
            estimated_cost: None,
        });
        f.settings = Some(settings);
        f.profile = profile;
        f.preview.state = "ready".into();
        Ok(())
    })();
    if let Err(error) = result {
        f.preview.state = "failed".into();
        f.preview.error_code = Some(
            if error.to_string() == "no_low_dial_provider" {
                "no_low_dial_provider"
            } else {
                "content_format_unavailable"
            }
            .into(),
        );
    }
    q.conn.execute("UPDATE content_format_previews SET state=?1,quote_id=?2,payload=?3 WHERE preview_id=?4 AND state='preparing'",
        params![f.preview.state,f.preview.quote.as_ref().map(|q|&q.quote_id),serde_json::to_string(&f)?,f.preview.preview_id])?;
    Ok(true)
}
