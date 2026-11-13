//! Recoverable trash. Trashing writes one marker record `catalog/trash/<item_id>.json` under the
//! writer lock; item files are never deleted. Every scoped read, listing and metadata page skips
//! trashed items (so library queries, facets, group counts and surface/mobile exports exclude
//! them), while the raw `read_item` used by internal derivation keeps working. Restoring removes
//! the marker. There is no hard delete.

use super::*;

pub const TRASH_PAGE_SIZE: usize = 50;
const TRASH_DIR: &str = "catalog/trash";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrashRecord {
    pub schema_version: u32,
    pub item_id: String,
    /// The revision that was current when the item was trashed.
    pub revision_id: String,
    pub trashed_at: String,
    /// Fixed reason code, e.g. `duplicate`.
    pub reason: String,
    pub kept_item_id: Option<String>,
    pub plan_id: Option<String>,
}

/// One `trash_list` row.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrashEntry {
    pub item_id: String,
    pub revision_id: String,
    pub title: String,
    pub source_url: String,
    pub trashed_at: String,
    pub reason: String,
    pub kept_item_id: Option<String>,
    pub plan_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrashRestoreReceipt {
    pub item_id: String,
    pub revision_id: String,
    pub restored: bool,
}

fn marker_path(root: &Path, item_id: &str) -> PathBuf {
    root.join(TRASH_DIR).join(format!("{item_id}.json"))
}

fn valid_reason(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

/// Whether `item_id` carries a trash marker (`root` is the canonical library root).
pub(crate) fn is_trashed(root: &Path, item_id: &str) -> Result<bool> {
    match fs::symlink_metadata(marker_path(root, item_id)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_file() => Ok(true),
        Ok(_) => anyhow::bail!("unsafe trash marker"),
        Err(error) => Err(error.into()),
    }
}

/// Whether an item is in the trash (for raw-read paths that must hide it).
pub fn is_item_trashed(library: impl AsRef<Path>, item_id: &str) -> Result<bool> {
    if !valid_item_id(item_id) {
        return Ok(false);
    }
    is_trashed(&existing_canonical_root(library.as_ref())?, item_id)
}

/// The trash marker of one item, if it is trashed.
pub fn read_trash_record(library: impl AsRef<Path>, item_id: &str) -> Result<Option<TrashRecord>> {
    if !valid_item_id(item_id) {
        return Ok(None);
    }
    let root = existing_canonical_root(library.as_ref())?;
    if !is_trashed(&root, item_id)? {
        return Ok(None);
    }
    load_marker(&root, item_id).map(Some)
}

/// All trashed item ids (one directory read).
pub(crate) fn trashed_ids(root: &Path) -> Result<BTreeSet<String>> {
    let dir = root.join(TRASH_DIR);
    let mut ids = BTreeSet::new();
    match fs::symlink_metadata(&dir) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(ids),
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_dir() => {}
        Ok(_) => anyhow::bail!("unsafe trash directory"),
        Err(error) => return Err(error.into()),
    }
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("unsafe trash marker"))?;
        if name.starts_with('.') {
            // Temporary files of an interrupted durable replace.
            continue;
        }
        let item_id = name
            .strip_suffix(".json")
            .filter(|id| valid_item_id(id))
            .ok_or_else(|| anyhow::anyhow!("unsafe trash marker"))?;
        if entry.file_type()?.is_symlink() {
            anyhow::bail!("unsafe trash marker")
        }
        ids.insert(item_id.to_owned());
    }
    Ok(ids)
}

fn load_marker(root: &Path, item_id: &str) -> Result<TrashRecord> {
    let record: TrashRecord = load(root, marker_path(root, item_id))?;
    if record.schema_version != SCHEMA_VERSION
        || record.item_id != item_id
        || !valid_revision_id(&record.revision_id)
        || !valid_reason(&record.reason)
        || chrono::DateTime::parse_from_rfc3339(&record.trashed_at).is_err()
    {
        anyhow::bail!("invalid trash marker")
    }
    Ok(record)
}

/// Trashes one item if its current revision is still `expected_revision_id` (CAS under the
/// writer lock). Re-trashing with the same plan is an idempotent success.
pub fn trash_item(
    library: impl AsRef<Path>,
    item_id: &str,
    expected_revision_id: &str,
    reason: &str,
    kept_item_id: Option<&str>,
    plan_id: Option<&str>,
) -> Result<TrashRecord> {
    if !valid_item_id(item_id)
        || !valid_revision_id(expected_revision_id)
        || !valid_reason(reason)
        || kept_item_id.is_some_and(|id| !valid_item_id(id) || id == item_id)
        || plan_id.is_some_and(|id| !valid_text(id, 128))
    {
        anyhow::bail!("invalid_trash_request")
    }
    let root = existing_canonical_root(library.as_ref())?;
    let lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    if is_trashed(&root, item_id)? {
        let record = load_marker(&root, item_id)?;
        lock.unlock()?;
        if record.revision_id == expected_revision_id && record.plan_id.as_deref() == plan_id {
            return Ok(record);
        }
        anyhow::bail!("item_already_trashed")
    }
    let (_, current) = read_item(&root, item_id)?;
    if current.revision_id != expected_revision_id {
        anyhow::bail!("stale_item_revision")
    }
    let record = TrashRecord {
        schema_version: SCHEMA_VERSION,
        item_id: item_id.to_owned(),
        revision_id: current.revision_id,
        trashed_at: chrono::Utc::now().to_rfc3339(),
        reason: reason.to_owned(),
        kept_item_id: kept_item_id.map(Into::into),
        plan_id: plan_id.map(Into::into),
    };
    let dir = ensure_contained_dir(&root, Path::new(TRASH_DIR))?;
    #[cfg(unix)]
    fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    durable_replace(
        &root,
        &marker_path(&root, item_id),
        serde_json::to_string_pretty(&record)?.as_bytes(),
    )?;
    lock.unlock()?;
    Ok(record)
}

/// Removes the trash marker; the item reappears with its unchanged current revision.
pub fn restore_item(library: impl AsRef<Path>, item_id: &str) -> Result<TrashRestoreReceipt> {
    if !valid_item_id(item_id) {
        anyhow::bail!("library_resource_not_found")
    }
    let root = existing_canonical_root(library.as_ref())?;
    let lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    if !root.join("items").join(item_id).is_dir() {
        anyhow::bail!("library_resource_not_found")
    }
    if !is_trashed(&root, item_id)? {
        anyhow::bail!("item_not_trashed")
    }
    let path = contained_existing(&root, &marker_path(&root, item_id))?;
    fs::remove_file(&path)?;
    sync_dir(path.parent().context("trash marker has no parent")?)?;
    let (_, revision) = read_item(&root, item_id)?;
    lock.unlock()?;
    Ok(TrashRestoreReceipt {
        item_id: item_id.to_owned(),
        revision_id: revision.revision_id,
        restored: true,
    })
}

/// One page of trashed items in ascending item-id order.
pub fn list_trash(
    library: impl AsRef<Path>,
    cursor: Option<&str>,
) -> Result<(Vec<TrashEntry>, Option<String>)> {
    if cursor.is_some_and(|value| !valid_item_id(value)) {
        anyhow::bail!("invalid_cursor")
    }
    with_consistent_snapshot(library, |root| {
        let ids: Vec<String> = trashed_ids(root)?
            .into_iter()
            .filter(|id| cursor.is_none_or(|cursor| id.as_str() > cursor))
            .take(TRASH_PAGE_SIZE + 1)
            .collect();
        let has_more = ids.len() > TRASH_PAGE_SIZE;
        let mut entries = Vec::new();
        for item_id in ids.into_iter().take(TRASH_PAGE_SIZE) {
            let record = load_marker(root, &item_id)?;
            let (_, revision) = read_item(root, &item_id)?;
            entries.push(TrashEntry {
                item_id,
                revision_id: revision.revision_id,
                title: revision.title,
                source_url: revision.source_url,
                trashed_at: record.trashed_at,
                reason: record.reason,
                kept_item_id: record.kept_item_id,
                plan_id: record.plan_id,
            });
        }
        let next = has_more
            .then(|| entries.last().map(|e| e.item_id.clone()))
            .flatten();
        Ok((entries, next))
    })
}
