//! Body identity: what generated text and reader projections of a revision depend on.
//!
//! An item keeps one immutable `item_id`; an owner edit of its title, creators, dates,
//! keywords, tags or groups writes a new revision whose body is copied unchanged. Derived
//! results (translations, minutes, insights, chapter translations, marks, passes) are bound to
//! the revision they were produced from and validated against it; a later revision with the
//! same body identity inherits them. Only a changed body (Markdown, Evidence, attachments) or a
//! changed sensitivity invalidates them.
use super::*;

/// `sha256:` digest of the item id, body Markdown, Evidence ids, attachments and sensitivity.
/// Title and every bibliographic/organizing field are excluded on purpose.
pub fn body_identity(revision: &ItemRevision) -> Result<String> {
    let mut digest = Sha256::new();
    for part in [
        revision.item_id.as_bytes().to_vec(),
        revision.content_markdown.as_bytes().to_vec(),
        serde_json::to_vec(&revision.evidence_ids)?,
        serde_json::to_vec(&revision.attachments)?,
        serde_json::to_vec(&revision.sensitivity)?,
    ] {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(&part);
    }
    Ok(format!("sha256:{:x}", digest.finalize()))
}

/// The item's verified owner chain, current revision first (at most the edit-chain bound).
fn verified_chain(library: &Path, item_id: &str) -> Result<Vec<ItemRevision>> {
    let (_, current) = read_item(library, item_id)?;
    let root = existing_canonical_root(library)?;
    let (_, legacy) = read_legacy_revision(&root, item_id)?;
    let mut chain = vec![current];
    loop {
        let last = chain.last().expect("non-empty chain");
        if last.revision_id == legacy.revision_id || chain.len() > MAX_ITEM_REVISION_CHAIN as usize
        {
            break;
        }
        // `read_item` verified the whole chain; follow the same parent links it checked.
        let Some(parent) = last
            .provenance
            .last()
            .and_then(|activity| activity.input_ids.first())
            .filter(|id| valid_revision_id(id))
            .cloned()
        else {
            break;
        };
        let revision: ItemRevision = load(
            &root,
            root.join("items")
                .join(item_id)
                .join("revisions")
                .join(format!("{parent}.json")),
        )?;
        anyhow::ensure!(
            revision.item_id == item_id && revision.revision_id == parent,
            "invalid owner edit chain"
        );
        chain.push(revision);
    }
    Ok(chain)
}

/// Other revisions on the item's verified owner chain whose body identity equals that of
/// `revision_id`, newest first. Empty when `revision_id` is not on the chain.
pub fn same_body_revisions(
    library: impl AsRef<Path>,
    item_id: &str,
    revision_id: &str,
) -> Result<Vec<ItemRevision>> {
    if !valid_item_id(item_id) || !valid_revision_id(revision_id) {
        anyhow::bail!("invalid item id");
    }
    let chain = verified_chain(library.as_ref(), item_id)?;
    let Some(target) = chain.iter().find(|r| r.revision_id == revision_id) else {
        return Ok(Vec::new());
    };
    let identity = body_identity(target)?;
    let mut same = Vec::new();
    for revision in &chain {
        if revision.revision_id != revision_id && body_identity(revision)? == identity {
            same.push(revision.clone());
        }
    }
    Ok(same)
}

/// Whether `candidate` is a revision of the item's verified chain with the same body as
/// `revision_id`; returns that revision.
pub fn same_body_revision(
    library: impl AsRef<Path>,
    item_id: &str,
    revision_id: &str,
    candidate: &str,
) -> Result<Option<ItemRevision>> {
    Ok(same_body_revisions(library, item_id, revision_id)?
        .into_iter()
        .find(|revision| revision.revision_id == candidate))
}

/// Whether revisions `a` and `b` of the item are the same revision or share their body on
/// the verified chain. Read-only; never takes Store locks (callable under them).
pub(crate) fn same_body(root: &Path, item_id: &str, a: &str, b: &str) -> bool {
    a == b || same_body_revision(root, item_id, a, b).is_ok_and(|found| found.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_and_tag_edits_keep_body_identity_and_body_edits_change_it() {
        let dir = tempfile::tempdir().unwrap();
        let receipt = ingest(dir.path(), &crate::tests::capture(), crate::tests::draft()).unwrap();
        let (item, first) = (&receipt.item_ids[0], &receipt.revision_ids[0]);
        let renamed = revise_item_metadata(
            dir.path(),
            item,
            ItemMetadataUpdate {
                expected_revision_id: first.clone(),
                title: "Renamed title".into(),
                manual_tags: vec!["tag".into()],
            },
        )
        .unwrap();
        let renamed_again = revise_item_metadata(
            dir.path(),
            item,
            ItemMetadataUpdate {
                expected_revision_id: renamed.revision_id.clone(),
                title: "Renamed twice".into(),
                manual_tags: vec!["tag".into()],
            },
        )
        .unwrap();
        let same = same_body_revisions(dir.path(), item, &renamed_again.revision_id).unwrap();
        let ids: Vec<_> = same.iter().map(|r| r.revision_id.as_str()).collect();
        assert_eq!(ids, [renamed.revision_id.as_str(), first.as_str()]);
        assert!(same_body_revision(dir.path(), item, &renamed_again.revision_id, first)
            .unwrap()
            .is_some());
        let edited = edit_markdown(
            dir.path(),
            MarkdownEdit {
                item_id: item.clone(),
                expected_revision_id: renamed_again.revision_id.clone(),
                request_id: "request_body_edit_test".into(),
                markdown: "A different body.\n".into(),
            },
            Sensitivity::Restricted,
        )
        .unwrap();
        assert!(same_body_revisions(dir.path(), item, &edited.revision_id)
            .unwrap()
            .is_empty());
        // The older revisions still share their body with each other.
        assert_eq!(
            same_body_revisions(dir.path(), item, first).unwrap().len(),
            2
        );
    }
}
