//! Read-only inheritance across verified metadata-only owner transitions.
use super::*;

pub(super) fn version(
    kind: &str,
    item: &str,
    current: &str,
    source: &str,
    hash: &str,
) -> Result<String> {
    if current == source {
        return Ok(hash.into());
    }
    Ok(reader_sha256(&serde_json::to_vec(&[
        "memex.reader.metadata-view.v1",
        kind,
        item,
        current,
        source,
        hash,
    ])?))
}

/// The base of a validated owner transition that may leave single chapters
/// byte-identical: metadata-only edits (whole document unchanged) plus content
/// format/restore, chapter edit and repair. Never crosses a reconversion or a
/// whole-markdown edit. Callers must still compare the chapter they inherit for
/// (key and bytes on every side they bind) and validate the old proof itself.
pub(super) fn chapter_parent(
    root: &Path,
    current: &ItemRevision,
    ceiling: Sensitivity,
) -> Result<Option<ItemRevision>> {
    let Some(activity) = current.provenance.last() else {
        return Ok(None);
    };
    if !matches!(
        activity.activity_type.as_str(),
        content_format::ACTIVITY | content_format::RESTORE_ACTIVITY | chapter_edit::ACTIVITY | repair::ACTIVITY
    ) {
        return parent(root, current, ceiling);
    }
    let id = activity
        .input_ids
        .first()
        .filter(|id| valid_revision_id(id))
        .context("invalid owner edit chain")?;
    let base: ItemRevision = load(
        root,
        root.join("items")
            .join(&current.item_id)
            .join("revisions")
            .join(format!("{id}.json")),
    )?;
    validate_owner_edit_transition(&base, current)?;
    anyhow::ensure!(base.sensitivity <= ceiling, "library_resource_not_found");
    Ok(Some(base))
}

// Callers have already authorized and validated the complete current chain.
// Only call after a missing artifact; corruption is never a fallback signal.
pub(super) fn parent(
    root: &Path,
    current: &ItemRevision,
    ceiling: Sensitivity,
) -> Result<Option<ItemRevision>> {
    let Some(activity) = current.provenance.last() else {
        return Ok(None);
    };
    if !matches!(
        activity.activity_type.as_str(),
        LEGACY_EDIT_ACTIVITY
            | BIBLIOGRAPHIC_EDIT_ACTIVITY
            | MODEL_SUGGESTION_ACTIVITY
            | lookup::LOOKUP_APPLY_ACTIVITY
            | podcast_metadata::ACTIVITY
    ) {
        return Ok(None);
    }
    let id = activity
        .input_ids
        .first()
        .filter(|id| valid_revision_id(id))
        .context("invalid owner edit chain")?;
    let base: ItemRevision = load(
        root,
        root.join("items")
            .join(&current.item_id)
            .join("revisions")
            .join(format!("{id}.json")),
    )?;
    validate_owner_edit_transition(&base, current)?;
    anyhow::ensure!(base.sensitivity <= ceiling, "library_resource_not_found");
    let (before, before_bodies) = document_revision_locked(root, &base, ceiling)?;
    let (after, after_bodies) = document_revision_locked(root, current, ceiling)?;
    anyhow::ensure!(
        before.item_id == after.item_id
            && before.canonical_sha256 == after.canonical_sha256
            && before.default_side == after.default_side
            && before.alignment == after.alignment
            && before.editions == after.editions
            && before_bodies == after_bodies,
        "invalid owner edit revision"
    );
    Ok(Some(base))
}
