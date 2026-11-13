//! Selected-chapter translation planning and durable execution.
pub mod jobs;
pub mod planner;

use super::*;
use memex_core::Sensitivity;
use memex_models::{Purpose, Snapshot};
use memex_store::reader::chapter_translation as projection;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub item_id: String,
    pub revision_id: String,
    pub chapter: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Quote {
    pub quote_id: String,
    pub expires_at: i64,
    #[serde(flatten)]
    pub selection: Selection,
    pub source_sha256: String,
    pub target_language: String,
    pub settings_revision: u64,
    pub route: projection::Route,
    pub budget: planner::Budget,
    pub source_bytes: usize,
    pub planned_requests: usize,
    pub estimated_cost: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct Preview {
    pub eligible: bool,
    pub reason: Option<String>,
    pub quote: Option<Quote>,
    pub translation: Option<projection::Metadata>,
}

#[derive(Serialize, Deserialize)]
struct FrozenQuote {
    quote: Quote,
    library: PathBuf,
    identity: projection::Identity,
    settings: Snapshot,
    credential_generation: u64,
    plan_sha256: String,
}

fn limits() -> planner::AdapterLimits {
    planner::AdapterLimits {
        max_input_bytes: memex_provider::generation::MAX_INPUT_BYTES.min(32768),
        max_completion_tokens: 8192,
    }
}

// This is a local, noninteractive status read, not a provider credential probe.
// status internally reads the scoped Keychain record; only generation escapes.
pub(crate) fn credential_generation(database: &Path, settings: &Snapshot) -> Result<u64> {
    use memex_provider::credentials::{CredentialState, Credentials};
    let (provider, _) = settings
        .configuration
        .target(Purpose::Translation)
        .context("no_translation_provider")?;
    let state = Credentials::for_database(database, &provider.id)
        .status(provider, settings.revision)
        .map_err(|_| anyhow::anyhow!("reader_translation_unavailable"))?;
    anyhow::ensure!(
        state.state == CredentialState::Saved && state.generation > 0,
        "reader_translation_unavailable"
    );
    Ok(state.generation)
}

pub fn preview(
    q: &Queue,
    library: &Path,
    selection: Selection,
    ceiling: Sensitivity,
) -> Result<Preview> {
    preview_with(
        q,
        library,
        selection,
        ceiling,
        Utc::now().timestamp_millis(),
        credential_generation,
    )
}

pub(crate) fn preview_with(
    q: &Queue,
    library: &Path,
    selection: Selection,
    ceiling: Sensitivity,
    now: i64,
    generation: impl FnOnce(&Path, &Snapshot) -> Result<u64>,
) -> Result<Preview> {
    preview_inner(q, library, selection, ceiling, now, generation, false)
}

/// A quote the server issues and accepts itself for a one-step `TranslateRequest` (no client
/// quote): the same frozen settings/credential/plan snapshot, also for a published result.
pub(crate) fn issue_quote(
    q: &Queue,
    library: &Path,
    selection: Selection,
    ceiling: Sensitivity,
    now: i64,
    generation: impl FnOnce(&Path, &Snapshot) -> Result<u64>,
) -> Result<String> {
    let preview = preview_inner(q, library, selection, ceiling, now, generation, true)?;
    match preview.quote {
        Some(quote) => Ok(quote.quote_id),
        None => anyhow::bail!(preview
            .reason
            .unwrap_or_else(|| "reader_translation_unavailable".into())),
    }
}

fn preview_inner(
    q: &Queue,
    library: &Path,
    selection: Selection,
    ceiling: Sensitivity,
    now: i64,
    generation: impl FnOnce(&Path, &Snapshot) -> Result<u64>,
    always_quote: bool,
) -> Result<Preview> {
    let unavailable = |reason: &str| Preview {
        eligible: false,
        reason: Some(reason.into()),
        quote: None,
        translation: None,
    };
    let library = library.canonicalize()?;
    let source = match projection::source(
        &library,
        &selection.item_id,
        &selection.revision_id,
        &selection.chapter,
        ceiling,
    ) {
        Ok(source) => source,
        Err(e)
            if matches!(
                e.to_string().as_str(),
                "reader_translation_not_needed" | "reader_translation_language_unsupported"
            ) =>
        {
            return Ok(unavailable(&e.to_string()))
        }
        Err(e) => return Err(e),
    };
    if let Some(translation) = projection::metadata(
        &library,
        &selection.item_id,
        &selection.revision_id,
        &selection.chapter,
        ceiling,
    )?
    .filter(|_| !always_quote)
    {
        return Ok(Preview {
            eligible: true,
            reason: None,
            quote: None,
            translation: Some(translation),
        });
    }
    let plan = match planner::plan(&source.markdown, limits()) {
        Ok(plan) => plan,
        Err(_) => return Ok(unavailable("reader_translation_budget_exceeded")),
    };
    let settings = memex_models::read(&q.conn)?;
    let Some((provider, model)) = settings.configuration.target(Purpose::Translation) else {
        return Ok(unavailable("no_translation_provider"));
    };
    let route = projection::Route {
        provider_id: provider.id.clone(),
        model_id: model.model_id.clone(),
        reasoning_effort: settings
            .configuration
            .selections
            .iter()
            .find(|s| s.purpose == Purpose::Translation)
            .and_then(|s| s.reasoning_effort)
            .map(|e| {
                serde_json::to_value(e)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()
            }),
    };
    let credential_generation = match generation(q.database_path(), &settings) {
        Ok(g) if g > 0 => g,
        _ => return Ok(unavailable("reader_translation_unavailable")),
    };
    // A quote is metadata only; no chapter text or credential status is persisted.
    let quote = Quote {
        quote_id: format!("quote_{}", uuid::Uuid::now_v7().simple()),
        expires_at: now
            .checked_add(15 * 60 * 1000)
            .context("invalid_reader_translation_request")?,
        selection,
        source_sha256: source.identity.source_sha256.clone(),
        target_language: "zh-CN".into(),
        settings_revision: settings.revision,
        route,
        budget: plan.budget().clone(),
        source_bytes: source.identity.source_bytes,
        planned_requests: plan.batches().len(),
        estimated_cost: None,
    };
    let frozen = FrozenQuote {
        quote: quote.clone(),
        library: library.clone(),
        identity: source.identity,
        settings,
        credential_generation,
        plan_sha256: plan.identity().sha256(),
    };
    q.conn.execute(
        "INSERT INTO reader_translation_quotes(quote_id,payload) VALUES(?1,?2)",
        params![quote.quote_id, serde_json::to_string(&frozen)?],
    )?;
    Ok(Preview {
        eligible: true,
        reason: None,
        quote: Some(quote),
        translation: None,
    })
}

// New admission must validate this before reserving an item or dispatching.
// Committed request replay is intentionally handled before this expiry check.
fn validate_quote(
    q: &Queue,
    library: &Path,
    selection: &Selection,
    quote_id: &str,
    ceiling: Sensitivity,
    now: i64,
    generation: impl FnOnce(&Path, &Snapshot) -> Result<u64>,
) -> Result<(FrozenQuote, planner::Plan)> {
    let payload: String = q
        .conn
        .query_row(
            "SELECT payload FROM reader_translation_quotes WHERE quote_id=?1",
            [quote_id],
            |r| r.get(0),
        )
        .optional()?
        .context("reader_translation_quote_stale")?;
    let frozen: FrozenQuote = serde_json::from_str(&payload)?;
    anyhow::ensure!(
        frozen.library == library.canonicalize()?
            && frozen.quote.expires_at > now
            && frozen.quote.selection.item_id == selection.item_id
            && frozen.quote.selection.revision_id == selection.revision_id
            && frozen.quote.selection.chapter == selection.chapter,
        "reader_translation_quote_stale"
    );
    let source = projection::source(
        library,
        &selection.item_id,
        &selection.revision_id,
        &selection.chapter,
        ceiling,
    )?;
    let plan = planner::plan(&source.markdown, limits())?;
    let settings = memex_models::read(&q.conn)?;
    anyhow::ensure!(
        source.identity == frozen.identity
            && settings == frozen.settings
            && plan.identity().sha256() == frozen.plan_sha256,
        "reader_translation_quote_stale"
    );
    anyhow::ensure!(
        generation(q.database_path(), &settings).ok() == Some(frozen.credential_generation),
        "reader_translation_quote_stale"
    );
    Ok((frozen, plan))
}

#[cfg(test)]
#[path = "../../examples/fixtures/reader_book.rs"]
mod book;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_binds_scope_generation_and_expiry_without_inference() {
        let tmp = tempfile::tempdir().unwrap();
        let library = tmp.path().join("Library");
        let doc = book::seed(&library).unwrap();
        let mut q = Queue::open(&tmp.path().join("queue.db")).unwrap();
        let selection = Selection {
            item_id: doc.item_id,
            revision_id: doc.revision_id,
            chapter: "appendix".into(),
        };
        let mut paired = selection.clone();
        paired.chapter = "chapter-1".into();
        let denied = preview_with(&q, &library, paired, Sensitivity::Public, 100, |_, _| {
            panic!("ineligible credential lookup")
        })
        .unwrap();
        assert_eq!(
            denied.reason.as_deref(),
            Some("reader_translation_not_needed")
        );
        let unconfigured = preview_with(
            &q,
            &library,
            selection.clone(),
            Sensitivity::Public,
            100,
            |_, _| panic!("unconfigured credential lookup"),
        )
        .unwrap();
        assert_eq!(
            unconfigured.reason.as_deref(),
            Some("no_translation_provider")
        );
        let config = memex_models::Configuration {
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
                reasoning_effort: Some(memex_models::ReasoningEffort::Low),
            }],
        };
        memex_models::replace(&mut q.conn, 0, config.clone()).unwrap();
        for generation in [Ok(0), Err(anyhow::anyhow!("synthetic SECRET credential"))] {
            let failed = preview_with(
                &q,
                &library,
                selection.clone(),
                Sensitivity::Public,
                100,
                |_, _| generation,
            )
            .unwrap();
            assert!(failed.quote.is_none());
            assert_eq!(
                failed.reason.as_deref(),
                Some("reader_translation_unavailable")
            );
            assert!(!serde_json::to_string(&failed).unwrap().contains("SECRET"));
        }
        let preview = preview_with(
            &q,
            &library,
            selection.clone(),
            Sensitivity::Public,
            100,
            |database, settings| {
                assert_eq!(database, q.database_path());
                assert_eq!(settings.revision, 1);
                Ok(7)
            },
        )
        .unwrap();
        let quote = preview.quote.unwrap();
        assert_eq!(quote.expires_at, 900100);
        assert_eq!(quote.planned_requests, 1);
        assert_eq!(quote.route.model_id, "fixture-model");
        assert_eq!(quote.route.reasoning_effort.as_deref(), Some("low"));
        assert!(quote.estimated_cost.is_none());
        let wire = serde_json::to_string(&quote).unwrap();
        assert!(
            !wire.contains("credential")
                && !wire.contains("masked_key")
                && !wire.contains("Original only")
        );
        assert!(validate_quote(
            &q,
            &library,
            &selection,
            &quote.quote_id,
            Sensitivity::Public,
            900099,
            |_, _| Ok(7)
        )
        .is_ok());
        assert!(validate_quote(
            &q,
            &library,
            &selection,
            &quote.quote_id,
            Sensitivity::Public,
            900100,
            |_, _| panic!("expired credential lookup")
        )
        .is_err());
        assert!(validate_quote(
            &q,
            &library,
            &selection,
            &quote.quote_id,
            Sensitivity::Public,
            101,
            |_, _| Ok(8)
        )
        .is_err());
        memex_models::replace(&mut q.conn, 1, config).unwrap();
        assert!(validate_quote(
            &q,
            &library,
            &selection,
            &quote.quote_id,
            Sensitivity::Public,
            101,
            |_, _| panic!("stale settings credential lookup")
        )
        .is_err());
        let source = projection::source(
            &library,
            &selection.item_id,
            &selection.revision_id,
            &selection.chapter,
            Sensitivity::Public,
        )
        .unwrap();
        let saved = projection::publish(
            &library,
            source.identity,
            projection::Provenance {
                route: quote.route,
                prompt_version: planner::PROMPT_VERSION.into(),
                prompt_sha256: memex_store::reader::reader_sha256(planner::PROMPT.as_bytes()),
                plan_sha256: "a".repeat(64),
            },
            "# 附录\n\n仅有原文。".into(),
            Sensitivity::Public,
        )
        .unwrap();
        let cached = preview_with(&q, &library, selection.clone(), Sensitivity::Public, 200, |_, _| {
            panic!("cached credential lookup")
        })
        .unwrap();
        assert!(cached.quote.is_none());
        assert_eq!(cached.translation.unwrap(), saved);
        let renamed = memex_store::revise_item_metadata(&library, &selection.item_id, memex_store::ItemMetadataUpdate {
            expected_revision_id: selection.revision_id.clone(), title: "Renamed synthetic".into(), manual_tags: vec![],
        }).unwrap();
        let mut selection = selection;
        selection.revision_id = renamed.revision_id.clone();
        let inherited = preview_with(&q, &library, selection, Sensitivity::Public, 201, |_, _| {
            panic!("inherited projection must not probe credentials")
        }).unwrap();
        assert!(inherited.quote.is_none());
        let inherited = inherited.translation.unwrap();
        assert_eq!(inherited.revision_id, renamed.revision_id);
        assert_eq!(inherited.sha256, saved.sha256);
        assert_ne!(inherited.version, saved.version);
        assert_eq!(
            q.conn
                .query_row("SELECT count(*) FROM reader_repair_jobs", [], |r| r
                    .get::<_, usize>(0))
                .unwrap(),
            0
        );
    }
}
