//! Owner-local staged book import. Reservations are not published project trees.
//! All visible items use ordinary ingest journals and receipts; no new recovery engine.
use super::*;
use serde::{Deserialize, Serialize};

const STATE_PATH: &str = "catalog/book-projects/reservations.json";
const MAX_STATE_BYTES: usize = 16 * 1024 * 1024;
const MAX_PROJECTS: usize = 1024;
const MAX_REQUESTS: usize = 65536;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedItem {
    /// Normal Store ingest key, fixed with the capture timestamp before reservation.
    pub ingest_key: String,
    pub input_sha256: String,
    pub sensitivity: Sensitivity,
}

impl PlannedItem {
    pub fn from_input(capture: &VerifiedCapture, draft: &ItemDraft) -> Result<Self> {
        Ok(Self { ingest_key: ingest_key(capture), input_sha256: ingest_input_sha256(capture, draft)?,
            sensitivity: effective_sensitivity(capture).max(draft.sensitivity) })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportPlan {
    /// Owner-chosen stable source-set identity, independent of filenames and body revisions.
    pub source_identity: String,
    /// Full source-bundle input digest, not the hash of a path to Downloads.
    pub sources_sha256: String,
    pub sensitivity: Sensitivity,
    pub volume_count: u16,
    pub container: PlannedItem,
    /// Frozen source-mapping keys. IDs are generated once, not derived from these keys.
    pub articles: BTreeMap<String, PlannedItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArticleIdentity {
    pub section_id: String,
    pub item_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reservation {
    pub project_id: String,
    pub container_item_id: String,
    pub volume_ids: Vec<String>,
    pub articles: BTreeMap<String, ArticleIdentity>,
    pub plan: ImportPlan,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    schema_version: u32,
    projects: BTreeMap<String, Reservation>,
    requests: BTreeMap<String, String>,
}

fn validate_plan(plan: &ImportPlan) -> Result<()> {
    anyhow::ensure!(valid_text(&plan.source_identity, 256)
        && valid_sha256(&plan.sources_sha256)
        && (1..=64).contains(&plan.volume_count)
        && !plan.articles.is_empty() && plan.articles.len() <= 4096,
        "invalid_book_project_plan");
    let mut keys = BTreeSet::new();
    for input in std::iter::once(&plan.container).chain(plan.articles.values()) {
        anyhow::ensure!(valid_text(&input.ingest_key, 1024)
            && valid_sha256(&input.input_sha256) && keys.insert(&input.ingest_key),
            "invalid_book_project_input");
    }
    anyhow::ensure!(plan.articles.keys().all(|key| valid_text(key, 256)), "invalid_book_project_source_key");
    Ok(())
}

fn read_state(root: &Path) -> Result<State> {
    let path = root.join(STATE_PATH);
    if !path.try_exists()? {
        return Ok(State { schema_version: 1, ..Default::default() });
    }
    let path = contained_existing(root, &path)?;
    let file = open_regular_nofollow(&path)?;
    anyhow::ensure!(file.metadata()?.len() <= MAX_STATE_BYTES as u64, "book_project_limit");
    let mut bytes = Vec::new();
    file.take(MAX_STATE_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= MAX_STATE_BYTES, "book_project_limit");
    let state: State = serde_json::from_slice(&bytes)?;
    anyhow::ensure!(state.schema_version == 1 && state.projects.len() <= MAX_PROJECTS
        && state.requests.len() <= MAX_REQUESTS, "invalid_book_project_state");
    let mut ids = BTreeSet::new();
    let mut source_ids = BTreeSet::new();
    let mut source_digests = BTreeSet::new();
    for (id, reservation) in &state.projects {
        validate_plan(&reservation.plan)?;
        anyhow::ensure!(id == &reservation.project_id
            && source_ids.insert(&reservation.plan.source_identity)
            && source_digests.insert(&reservation.plan.sources_sha256)
            && valid_item_id(&reservation.container_item_id)
            && ids.insert(&reservation.container_item_id)
            && reservation.volume_ids.len() == reservation.plan.volume_count as usize
            && reservation.articles.keys().eq(reservation.plan.articles.keys()), "invalid_book_project_state");
        for volume in &reservation.volume_ids {
            anyhow::ensure!(valid_text(volume, 128) && ids.insert(volume), "invalid_book_project_state");
        }
        for article in reservation.articles.values() {
            anyhow::ensure!(valid_item_id(&article.item_id) && ids.insert(&article.item_id)
                && valid_text(&article.section_id, 128) && ids.insert(&article.section_id),
                "invalid_book_project_state");
        }
    }
    anyhow::ensure!(state.requests.iter().all(|(request, id)| valid_text(request, 256)
        && state.projects.contains_key(id)), "invalid_book_project_state");
    Ok(state)
}

fn write_state(root: &Path, state: &State) -> Result<()> {
    let bytes = serde_json::to_vec(state)?;
    anyhow::ensure!(bytes.len() <= MAX_STATE_BYTES, "book_project_limit");
    durable_replace(root, &root.join(STATE_PATH), &bytes)
}

// Reservation receipts are not an access bypass after an item becomes hidden or
// trashed. Uncreated identities remain private preparation, not missing references.
fn authorize_reservation(root: &Path, reservation: &Reservation, ceiling: Sensitivity) -> Result<()> {
    anyhow::ensure!(reservation.plan.sensitivity <= ceiling
        && std::iter::once(&reservation.plan.container).chain(reservation.plan.articles.values())
            .all(|input| input.sensitivity <= ceiling), "library_resource_not_found");
    for id in std::iter::once(&reservation.container_item_id)
        .chain(reservation.articles.values().map(|article| &article.item_id)) {
        match fs::symlink_metadata(root.join("items").join(id)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
            Ok(_) => anyhow::ensure!(read_item_at_sensitivity(root, id, ceiling)?.is_some(),
                "library_resource_not_found"),
        }
    }
    Ok(())
}

/// Persist every identity before ANY article may be ingested. Owner-local only;
/// there is deliberately no IPC/gateway handler or public capability yet.
pub fn reserve(library: &Path, request: &str, plan: ImportPlan, ceiling: Sensitivity) -> Result<Reservation> {
    anyhow::ensure!(plan.sensitivity <= ceiling
        && std::iter::once(&plan.container).chain(plan.articles.values())
            .all(|input| input.sensitivity <= ceiling), "library_resource_not_found");
    validate_plan(&plan)?;
    anyhow::ensure!(valid_text(request, 256), "invalid_book_project_request");
    let root = canonical_root(library)?;
    let lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    let mut state = read_state(&root)?;
    let existing = state.requests.get(request).and_then(|id| state.projects.get(id))
        .or_else(|| state.projects.values().find(|r| r.plan.source_identity == plan.source_identity
            || r.plan.sources_sha256 == plan.sources_sha256)).cloned();
    let reservation = if let Some(existing) = existing {
        authorize_reservation(&root, &existing, ceiling)?;
        anyhow::ensure!(existing.plan == plan, "book_project_input_conflict");
        existing
    } else {
        anyhow::ensure!(state.projects.len() < MAX_PROJECTS, "book_project_limit");
        Reservation { project_id: new_id("project"), container_item_id: new_id("item"),
            volume_ids: (0..plan.volume_count).map(|_| new_id("volume")).collect(),
            articles: plan.articles.keys().map(|key| (key.clone(), ArticleIdentity {
                section_id: new_id("section"), item_id: new_id("item"),
            })).collect(), plan }
    };
    if !state.requests.contains_key(request) {
        anyhow::ensure!(state.requests.len() < MAX_REQUESTS, "book_project_limit");
        state.requests.insert(request.into(), reservation.project_id.clone());
        state.projects.insert(reservation.project_id.clone(), reservation.clone());
        write_state(&root, &state)?;
    }
    lock.unlock()?;
    Ok(reservation)
}

/// None selects the container; Some selects a reserved article. A returned replay
/// is historical receipt reconciliation, never permission to overwrite or untrash.
pub fn ingest_part(library: &Path, project: &str, article: Option<&str>, capture: &VerifiedCapture,
    draft: ItemDraft, ceiling: Sensitivity) -> Result<IngestReceipt> {
    let root = existing_canonical_root(library)?;
    let lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    let state = read_state(&root)?;
    let reservation = state.projects.get(project).context("library_resource_not_found")?;
    authorize_reservation(&root, reservation, ceiling)?;
    anyhow::ensure!(reservation.plan.sensitivity <= ceiling && effective_sensitivity(capture) <= ceiling
        && draft.sensitivity <= ceiling, "library_resource_not_found");
    let (id, expected) = match article {
        None => (&reservation.container_item_id, &reservation.plan.container),
        Some(key) => (&reservation.articles.get(key).context("book_project_part_not_found")?.item_id,
            reservation.plan.articles.get(key).context("book_project_part_not_found")?),
    };
    anyhow::ensure!(PlannedItem::from_input(capture, &draft)? == *expected, "book_project_input_conflict");
    let keys = catalog_keys(&root)?;
    if let Some(path) = keys.get(&expected.ingest_key) {
        let prior: IngestReceipt = load(&root, safe_receipt_path(&root, path)?)?;
        anyhow::ensure!(prior.item_ids == [id.as_str()], "book_project_identity_conflict");
        let current = read_item_at_sensitivity(&root, id, ceiling)?.context("library_resource_not_found")?;
        anyhow::ensure!(prior.revision_ids == [current.1.revision_id], "book_project_item_conflict");
    }
    let receipt = match plan_ingest_with_item_locked(&root, capture, draft, &keys, Some(id))? {
        IngestPlan::Replay(receipt) => receipt,
        IngestPlan::New(journal, contents) => {
            match stage_ingest(&root, journal, contents)? {
                PreparedIngest::Journal(path, mut journal) => {
                    apply_journal(&root, &path, &mut journal)?;
                    journal.receipt
                }
                PreparedIngest::Replay(_) => unreachable!(),
            }
        }
    };
    clear_recovery_pending(&root)?;
    lock.unlock()?;
    Ok(receipt)
}

#[cfg(test)]
mod tests;
