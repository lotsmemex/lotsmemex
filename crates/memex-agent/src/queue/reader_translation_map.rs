//! Retrospective AI projection alignment. Never enters the human repair queue.
//! Frozen records contain identities and validated ranges, never provider prose.
use super::reader_authority::{local_only, Authority, Check, Phase};
use super::reader_repair::{matching, Budget, DispatchUsage, Usage};
use super::*;
use memex_core::Sensitivity;
use memex_models::{low_dial::Profile, Purpose, Snapshot};
use memex_store::reader::{self, chapter_translation_map as store, paragraphs::Pair};

pub mod jobs;
pub use jobs::{
    recover, recover_with, request, request_with, status, status_with, step, step_with, stop,
    stop_with, Job, Request,
};

pub const POLICY: &str = "retrospective-model-alignment-v1";
pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS reader_translation_map_quotes (
 quote_id TEXT PRIMARY KEY, payload TEXT NOT NULL, consumed_by TEXT UNIQUE);
CREATE TABLE IF NOT EXISTS reader_translation_map_jobs (
 job_id TEXT PRIMARY KEY, request_id TEXT NOT NULL UNIQUE,
 library TEXT NOT NULL, item_id TEXT NOT NULL, state TEXT NOT NULL,
 payload TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS reader_translation_map_active
 ON reader_translation_map_jobs(library,item_id,state);";

pub(super) fn init(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA)?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub item_id: String,
    pub revision_id: String,
    pub chapter: String,
    pub translation_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Quote {
    pub quote_id: String,
    pub expires_at: i64,
    #[serde(flatten)]
    pub selection: Selection,
    pub policy_version: String,
    pub settings_revision: u64,
    pub profile_revision: u64,
    pub route: reader::chapter_translation::Route,
    pub budget: Budget,
    pub planned_requests: usize,
    pub estimated_cost: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct Preview {
    pub eligible: bool,
    pub reason: Option<String>,
    pub quote: Option<Quote>,
    pub map_version: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Frozen {
    quote: Quote,
    authority: Authority,
    library: PathBuf,
    ceiling: Sensitivity,
    identity: store::Identity,
    settings: Snapshot,
    profile: Profile,
    credential_generation: u64,
    input_sha256: String,
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

fn credential_generation(database: &Path, settings: &Snapshot) -> Result<u64> {
    super::content_format::credential_generation(database, settings)
        .map_err(|_| anyhow::anyhow!("reader_translation_map_unavailable"))
}

fn source(library: &Path, selection: &Selection, ceiling: Sensitivity) -> Result<store::Source> {
    store::source(
        library,
        &selection.item_id,
        &selection.revision_id,
        &selection.chapter,
        &selection.translation_version,
        ceiling,
    )
}

fn input(source: &store::Source, budget: usize) -> Result<(String, matching::Pools)> {
    matching::from_texts([&source.source_markdown, &source.target_markdown], budget)?
        .context("reader_translation_map_budget_exceeded")
}

pub fn preview(
    q: &Queue,
    library: &Path,
    authority: &Authority,
    selection: Selection,
    ceiling: Sensitivity,
) -> Result<Preview> {
    preview_with(
        q,
        library,
        authority,
        selection,
        ceiling,
        Utc::now().timestamp_millis(),
        credential_generation,
        local_only,
    )
}

pub fn preview_with(
    q: &Queue,
    library: &Path,
    authority: &Authority,
    selection: Selection,
    ceiling: Sensitivity,
    now: i64,
    generation: impl Fn(&Path, &Snapshot) -> Result<u64>,
    authorize: impl Fn(Check<'_>) -> Result<Sensitivity>,
) -> Result<Preview> {
    init(&q.conn)?;
    let library = library.canonicalize()?;
    let ceiling = authority.check(
        &library,
        &selection.item_id,
        Phase::Preview,
        ceiling,
        &authorize,
    )?;
    let source = source(&library, &selection, ceiling)?;
    let page = store::read(
        &library,
        &selection.item_id,
        &selection.revision_id,
        &selection.chapter,
        &selection.translation_version,
        "any",
        0,
        1,
        ceiling,
    )?;
    if page.alignment.version != "missing" {
        return Ok(Preview {
            eligible: true,
            reason: Some("map_available".into()),
            quote: None,
            map_version: Some(page.alignment.version),
        });
    }
    if source.recorded_map {
        return Ok(Preview {
            eligible: true,
            reason: Some("recorded_map_available".into()),
            quote: None,
            map_version: None,
        });
    }
    let settings = memex_models::read(&q.conn)?;
    let profile = memex_models::low_dial::read(&q.conn)?.context("no_low_dial_provider")?;
    let routed = route(&settings, &profile)?;
    let (provider, model) = routed
        .configuration
        .target(Purpose::CaptureQuality)
        .context("no_low_dial_provider")?;
    let budget = Budget {
        max_requests: 1,
        max_input_bytes_per_request: profile
            .max_input_bytes
            .min(memex_provider::generation::MAX_INPUT_BYTES),
        max_completion_tokens_per_request: profile.max_completion_tokens,
    };
    let (input, _) = input(&source, budget.max_input_bytes_per_request)?;
    let credential_generation = generation(q.database_path(), &routed)?;
    anyhow::ensure!(
        credential_generation > 0,
        "reader_translation_map_unavailable"
    );
    let quote = Quote {
        quote_id: format!("map_quote_{}", uuid::Uuid::now_v7().simple()),
        expires_at: now
            .checked_add(900_000)
            .context("invalid_reader_translation_map_request")?,
        selection,
        policy_version: POLICY.into(),
        settings_revision: settings.revision,
        profile_revision: profile.revision,
        route: reader::chapter_translation::Route {
            provider_id: provider.id.clone(),
            model_id: model.model_id.clone(),
            reasoning_effort: profile.reasoning_effort.map(|e| {
                serde_json::to_value(e)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()
            }),
        },
        budget,
        planned_requests: 1,
        estimated_cost: None,
    };
    let frozen = Frozen {
        quote: quote.clone(),
        authority: authority.clone(),
        library,
        ceiling,
        identity: source.identity,
        settings,
        profile,
        credential_generation,
        input_sha256: reader::reader_sha256(input.as_bytes()),
    };
    q.conn.execute(
        "INSERT INTO reader_translation_map_quotes(quote_id,payload) VALUES(?1,?2)",
        params![quote.quote_id, serde_json::to_string(&frozen)?],
    )?;
    Ok(Preview {
        eligible: true,
        reason: None,
        quote: Some(quote),
        map_version: None,
    })
}

fn guard(
    q: &Queue,
    f: &Frozen,
    ceiling: Sensitivity,
    generation: &impl Fn(&Path, &Snapshot) -> Result<u64>,
) -> Result<(String, matching::Pools)> {
    let source = source(&f.library, &f.quote.selection, ceiling.min(f.ceiling))?;
    let settings = memex_models::read(&q.conn)?;
    let profile = memex_models::low_dial::read(&q.conn)?;
    anyhow::ensure!(
        source.identity == f.identity
            && !source.recorded_map
            && settings == f.settings
            && profile.as_ref() == Some(&f.profile)
            && f.quote.policy_version == POLICY,
        "reader_translation_map_quote_stale"
    );
    let (input, pools) = input(&source, f.quote.budget.max_input_bytes_per_request)?;
    anyhow::ensure!(
        reader::reader_sha256(input.as_bytes()) == f.input_sha256
            && generation(q.database_path(), &route(&settings, &f.profile)?).ok()
                == Some(f.credential_generation),
        "reader_translation_map_quote_stale"
    );
    let s = &f.quote.selection;
    store::read(
        &f.library,
        &s.item_id,
        &s.revision_id,
        &s.chapter,
        &s.translation_version,
        "missing",
        0,
        1,
        ceiling.min(f.ceiling),
    )?;
    Ok((input, pools))
}

#[cfg(test)]
mod tests;
