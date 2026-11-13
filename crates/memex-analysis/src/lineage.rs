//! Content-identity binding of generated text (see `memex_store::body_identity`).
//!
//! Jobs and Analysis records stay bound to the exact revision they were produced from and are
//! validated against that revision (its full digest). A later revision whose body identity is the
//! same (an owner edit of the title or other metadata) inherits them: reads fall back to the
//! newest same-body revision with a succeeded row, new requests reuse it instead of queueing a
//! duplicate, and an in-flight job keeps running when only metadata changed under it.
use super::revision_ref;
use anyhow::Result;
use memex_core::ItemRevision;
use rusqlite::{params, Connection};
use std::{collections::BTreeSet, path::Path};

/// Same-body revisions of `revision_id` (newest first) that have a succeeded row in `table`
/// (plus the optional extra SQL condition, which may use `?3` = `extra_param`). Cheap when no
/// other revision of the item has such a row: the revision chain is only read then.
pub(crate) fn inherited_revisions(
    connection: &Connection,
    root: &Path,
    table: &str,
    extra: &str,
    extra_param: &str,
    item_id: &str,
    revision_id: &str,
) -> Result<Vec<ItemRevision>> {
    let mut statement = connection.prepare(&format!(
        "SELECT DISTINCT revision_id FROM {table}
          WHERE item_id=?1 AND revision_id<>?2 AND state='succeeded' {extra}"
    ))?;
    let candidates = if extra.contains("?3") {
        statement
            .query_map(params![item_id, revision_id, extra_param], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<BTreeSet<_>>>()?
    } else {
        statement
            .query_map(params![item_id, revision_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<BTreeSet<_>>>()?
    };
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    Ok(memex_store::same_body_revisions(root, item_id, revision_id)?
        .into_iter()
        .filter(|revision| candidates.contains(&revision.revision_id))
        .collect())
}

/// The revision a job bound to (`revision_id`, `revision_sha256`) still validly reads: the
/// current revision when it is that revision, else that revision when only metadata changed
/// since (same body identity). `None`: the body changed (or the revision is unknown).
pub(crate) fn job_source(
    root: &Path,
    item_id: &str,
    revision_id: &str,
    revision_sha256: &str,
) -> Result<Option<ItemRevision>> {
    let (_, current) = memex_store::read_item(root, item_id)?;
    let source = if current.revision_id == revision_id {
        Some(current)
    } else {
        memex_store::same_body_revision(root, item_id, &current.revision_id, revision_id)?
    };
    Ok(match source {
        Some(source) if revision_ref(item_id, &source)?.revision_sha256 == revision_sha256 => {
            Some(source)
        }
        _ => None,
    })
}

/// Whether `source` is not the item's current revision (records then bind it explicitly).
pub(crate) fn historical(root: &Path, source: &ItemRevision) -> Result<bool> {
    Ok(memex_store::read_item(root, &source.item_id)?.1.revision_id != source.revision_id)
}

/// `revision_id` when it is the current revision or a same-body revision of it (an edit of
/// metadata only replaced it); `None` when the body changed since or it is unknown.
pub(crate) fn job_source_revision(
    root: &Path,
    item_id: &str,
    revision_id: &str,
) -> Result<Option<ItemRevision>> {
    let (_, current) = memex_store::read_item(root, item_id)?;
    if current.revision_id == revision_id {
        return Ok(Some(current));
    }
    memex_store::same_body_revision(root, item_id, &current.revision_id, revision_id)
}
