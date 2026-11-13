//! Owner Markdown revisions and revision-bound manual underlines. Original
//! Evidence is immutable; annotations never change the text or its byte offsets.
use super::*;

pub(crate) const ACTIVITY: &str = "owner_markdown_edit_v1";
const MAX_BODY: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarkdownEdit {
    pub item_id: String,
    pub expected_revision_id: String,
    pub request_id: String,
    pub markdown: String,
}

pub fn edit_markdown(library: &Path, edit: MarkdownEdit, ceiling: Sensitivity) -> Result<ItemMetadataUpdateReceipt> {
    if !valid_item_id(&edit.item_id) || !valid_revision_id(&edit.expected_revision_id)
        || !valid_request_id(&edit.request_id) || edit.markdown.len() > MAX_BODY
        || edit.markdown.trim().is_empty() || edit.markdown.contains('\0') {
        anyhow::bail!("invalid_markdown_edit")
    }
    let hash = digest(&serde_json::to_vec(&(ACTIVITY, &edit))?);
    let root = existing_canonical_root(library)?;
    let lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    let (item, base) = read_item_at_sensitivity(&root, &edit.item_id, ceiling)?.context("library_resource_not_found")?;
    // Authorization precedes replay, including after trash or sensitivity changes.
    if let Some(record) = metadata_v2::load_metadata_request(&root, &edit.request_id)? {
        if record.request_sha256 != hash || record.receipt.item_id != edit.item_id {
            anyhow::bail!("request_id_conflict")
        }
        return Ok(record.receipt);
    }
    if base.revision_id != edit.expected_revision_id { anyhow::bail!("stale_item_revision") }
    let (_, legacy) = read_legacy_item(&root, &edit.item_id)?;
    if base.revision_number.checked_sub(legacy.revision_number).context("invalid revision chain")? >= MAX_ITEM_REVISION_CHAIN {
        anyhow::bail!("item_revision_limit_exceeded")
    }
    let mut next = owner_edit_revision(&base, ACTIVITY, base.title.clone(), base.metadata.clone())?;
    next.content_markdown = edit.markdown;
    validate_owner_edit_transition(&base, &next)?;
    let receipt = commit_owner_revision_locked(&root, item, &base, &next, Some((edit.request_id, hash)))?;
    lock.unlock()?;
    Ok(receipt)
}

pub(crate) fn validate_transition(base: &ItemRevision, next: &ItemRevision) -> Result<()> {
    if next.title != base.title || next.metadata != base.metadata
        || next.content_markdown.trim().is_empty() || next.content_markdown.len() > MAX_BODY
        || next.content_markdown.contains('\0')
        || next.provenance.last().is_none_or(|a| a.config_digest != owner_edit_config_digest(ACTIVITY)) {
        anyhow::bail!("invalid owner markdown revision")
    }
    let mut ordinary = next.clone();
    ordinary.content_markdown = base.content_markdown.clone();
    let activity = ordinary.provenance.last_mut().context("missing owner activity")?;
    activity.activity_type = LEGACY_EDIT_ACTIVITY.into();
    activity.config_digest = owner_edit_config_digest(LEGACY_EDIT_ACTIVITY);
    validate_owner_edit_transition(base, &ordinary)
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Underline {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadingAnnotations {
    pub item_id: String,
    pub revision_id: String,
    pub markdown_sha256: String,
    pub underlines: Vec<Underline>,
}

fn digest(bytes: &[u8]) -> String { format!("sha256:{:x}", Sha256::digest(bytes)) }

impl ReadingAnnotations {
    pub fn version(&self) -> Result<String> { Ok(digest(&serde_json::to_vec(self)?)) }
}

fn annotation_path(root: &Path, item: &str, revision: &str) -> PathBuf {
    root.join("catalog/reading-annotations").join(item).join(format!("{revision}.json"))
}

fn read_locked(root: &Path, item: &str, revision: &str, ceiling: Sensitivity) -> Result<(ItemRevision, ReadingAnnotations)> {
    if !valid_item_id(item) || !valid_revision_id(revision) { anyhow::bail!("invalid_reading_request") }
    let (_, current) = read_item_at_sensitivity(root, item, ceiling)?.context("library_resource_not_found")?;
    if current.revision_id != revision { anyhow::bail!("stale_item_revision") }
    let empty = ReadingAnnotations { item_id: item.into(), revision_id: revision.into(),
        markdown_sha256: digest(current.content_markdown.as_bytes()), underlines: vec![] };
    let path = annotation_path(root, item, revision);
    let annotations = match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => inherited_underlines(root, item, revision)?
            .unwrap_or_else(|| empty.clone()),
        Err(e) => return Err(e.into()),
        Ok(_) => load::<ReadingAnnotations>(root, path)?,
    };
    if annotations.item_id != empty.item_id || annotations.revision_id != empty.revision_id
        || annotations.markdown_sha256 != empty.markdown_sha256 { anyhow::bail!("invalid_reading_annotations") }
    validate_ranges(&current.content_markdown, &annotations.underlines)?;
    Ok((current, annotations))
}

/// Underlines of the newest revision whose body is this revision's body (only metadata such
/// as the title edited since), rebound to this revision; the next save writes them here.
fn inherited_underlines(root: &Path, item: &str, revision: &str) -> Result<Option<ReadingAnnotations>> {
    for source in crate::body::same_body_revisions(root, item, revision)? {
        let path = annotation_path(root, item, &source.revision_id);
        match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
            Ok(_) => {}
        }
        let mut found = load::<ReadingAnnotations>(root, path)?;
        if found.item_id != item || found.revision_id != source.revision_id {
            anyhow::bail!("invalid_reading_annotations")
        }
        found.revision_id = revision.into();
        return Ok(Some(found));
    }
    Ok(None)
}

fn validate_ranges(body: &str, ranges: &[Underline]) -> Result<()> {
    if ranges.len() > 4096 || ranges.iter().any(|r| r.start >= r.end || body.get(r.start..r.end).is_none())
        || ranges.windows(2).any(|r| r[0].end >= r[1].start) {
        anyhow::bail!("invalid_reading_annotations")
    }
    Ok(())
}

pub fn read_annotations(library: &Path, item: &str, revision: &str, ceiling: Sensitivity) -> Result<ReadingAnnotations> {
    with_consistent_snapshot(library, |root| Ok(read_locked(root, item, revision, ceiling)?.1))
}

/// Full-set CAS is also retry-safe: identical current bytes mean this intent
/// already succeeded. A conflicting later write is never silently overwritten.
pub fn save_annotations(library: &Path, expected_version: &str, next: ReadingAnnotations, ceiling: Sensitivity) -> Result<ReadingAnnotations> {
    let root = existing_canonical_root(library)?;
    let lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    let (revision, current) = read_locked(&root, &next.item_id, &next.revision_id, ceiling)?;
    if current.markdown_sha256 != next.markdown_sha256 { anyhow::bail!("stale_item_revision") }
    validate_ranges(&revision.content_markdown, &next.underlines)?;
    if next == current { return Ok(current); }
    if current.version()? != expected_version { anyhow::bail!("stale_reading_annotations") }
    ensure_contained_dir(&root, &Path::new("catalog/reading-annotations").join(&next.item_id))?;
    durable_replace(&root, &annotation_path(&root, &next.item_id, &next.revision_id), &serde_json::to_vec(&next)?)?;
    lock.unlock()?;
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn markdown_replay_recovers_every_commit_boundary() {
        for step in 0..=4 {
            let dir = tempfile::tempdir().unwrap();
            let receipt = ingest(dir.path(), &crate::tests::capture(), crate::tests::draft()).unwrap();
            let id = &receipt.item_ids[0];
            let (_, original) = read_item(dir.path(), id).unwrap();
            let edit = MarkdownEdit { item_id: id.clone(), expected_revision_id: original.revision_id.clone(), request_id: "recover-body-edit".into(), markdown: "新的正文🌹".into() };
            FAIL_ITEM_UPDATE_STEP.with(|v| v.set(Some(step)));
            let interrupted = edit_markdown(dir.path(), edit.clone(), Sensitivity::Restricted);
            FAIL_ITEM_UPDATE_STEP.with(|v| v.set(None));
            assert!(interrupted.is_err(), "step {step}");
            recover_pending_ingests(dir.path()).unwrap();
            let saved = edit_markdown(dir.path(), edit.clone(), Sensitivity::Restricted).unwrap();
            assert_eq!(edit_markdown(dir.path(), edit, Sensitivity::Restricted).unwrap().revision_id, saved.revision_id);
            let (_, current) = read_item(dir.path(), id).unwrap();
            assert_eq!(current.revision_number, original.revision_number + 1);
            assert_eq!(current.content_markdown, "新的正文🌹");
            assert_eq!(current.evidence_ids, original.evidence_ids);
            assert_eq!(read_legacy_item(dir.path(), id).unwrap().1, original);
        }
    }

    #[test]
    fn markdown_edits_preserve_evidence_replay_and_revision_bound_underlines() {
        let dir = tempfile::tempdir().unwrap();
        let mut draft = crate::tests::draft();
        draft.content_markdown = "# 标题\n\n重复文字，重复文字。".into();
        let receipt = ingest(dir.path(), &crate::tests::capture(), draft).unwrap();
        let id = &receipt.item_ids[0];
        let (_, original) = read_item(dir.path(), id).unwrap();
        let read = || read_annotations(dir.path(), id, &original.revision_id, Sensitivity::Restricted).unwrap();
        let initial = read();
        let mut marked = initial.clone();
        let start = original.content_markdown.rfind("重复文字").unwrap();
        marked.underlines.push(Underline { start, end: start + "重复文字".len() });
        save_annotations(dir.path(), &initial.version().unwrap(), marked.clone(), Sensitivity::Restricted).unwrap();
        assert_eq!(read(), marked);
        save_annotations(dir.path(), &initial.version().unwrap(), marked.clone(), Sensitivity::Restricted).unwrap();
        assert!(save_annotations(dir.path(), &initial.version().unwrap(), initial.clone(), Sensitivity::Restricted).is_err());
        let mut invalid = marked.clone();
        invalid.underlines[0].start += 1;
        assert!(save_annotations(dir.path(), &marked.version().unwrap(), invalid, Sensitivity::Restricted).is_err());
        // A title edit keeps the underlines (same body), rebound to the new revision.
        let renamed = revise_item_metadata(dir.path(), id, ItemMetadataUpdate {
            expected_revision_id: original.revision_id.clone(), title: "改过的标题".into(), manual_tags: vec![] }).unwrap();
        let inherited = read_annotations(dir.path(), id, &renamed.revision_id, Sensitivity::Restricted).unwrap();
        assert_eq!(inherited.underlines, marked.underlines);
        assert_eq!(inherited.revision_id, renamed.revision_id);
        let mut more = inherited.clone();
        more.underlines.insert(0, Underline { start: 0, end: 1 });
        save_annotations(dir.path(), &inherited.version().unwrap(), more.clone(), Sensitivity::Restricted).unwrap();
        assert_eq!(read_annotations(dir.path(), id, &renamed.revision_id, Sensitivity::Restricted).unwrap(), more);
        let edit = MarkdownEdit { item_id: id.clone(), expected_revision_id: renamed.revision_id.clone(), request_id: "synthetic-body-edit".into(), markdown: "# 新版\n\n修改后的正文。".into() };
        let saved = edit_markdown(dir.path(), edit.clone(), Sensitivity::Restricted).unwrap();
        assert_eq!(edit_markdown(dir.path(), edit.clone(), Sensitivity::Restricted).unwrap().revision_id, saved.revision_id);
        let (_, current) = read_item(dir.path(), id).unwrap();
        assert_eq!(current.content_markdown, edit.markdown);
        assert_eq!(current.evidence_ids, original.evidence_ids);
        assert_eq!(current.metadata, original.metadata);
        assert_eq!(current.attachments, original.attachments);
        assert_eq!(current.revision_number, original.revision_number + 2);
        assert_eq!(read_legacy_item(&existing_canonical_root(dir.path()).unwrap(), id).unwrap().1, original);
        assert!(read_annotations(dir.path(), id, &original.revision_id, Sensitivity::Restricted).is_err());
        assert!(read_annotations(dir.path(), id, &current.revision_id, Sensitivity::Restricted).unwrap().underlines.is_empty());
        assert!(edit_markdown(dir.path(), MarkdownEdit { request_id: "another-request".into(), ..edit }, Sensitivity::Restricted).is_err());
    }
}
