//! Source-bound formatting, immutable private change proofs and journaled restore.
//! Model consent, budgets and durable batch scheduling remain Agent-owned.
mod durable;
#[cfg(test)]
mod durable_tests;
pub mod edits;
pub use durable::{
    change_text, changes_list, publish, receipt, restore, restore_receipt, ChangeRow, ChangeText,
    Changes, Receipt, RestoreReceipt,
};
pub(crate) use durable::{validate_evidence, validate_transition, ACTIVITY, RESTORE_ACTIVITY};

pub use super::revision_text::Target;
use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub item_id: String,
    pub revision_id: String,
    pub revision_sha256: String,
    pub target: Target,
    pub source_sha256: String,
    pub source_bytes: usize,
    pub sensitivity: Sensitivity,
    pub source_evidence_ids: Vec<String>,
}

// Neither Debug nor Serialize: selected text is not diagnostic metadata.
pub struct Source {
    pub identity: Identity,
    pub markdown: String,
}

/// Snapshot only the requested current canonical body or edition chapter. This
/// local read grants no model consent; callers must separately enforce dispatch
/// eligibility, frozen membership, route/generation and the quoted budget.
pub fn source(
    library: &Path,
    item: &str,
    revision: &str,
    target: Target,
    ceiling: Sensitivity,
) -> Result<Source> {
    anyhow::ensure!(
        valid_item_id(item) && valid_revision_id(revision),
        "invalid_content_format_request"
    );
    if let Target::Chapter { side, chapter } = &target {
        anyhow::ensure!(
            matches!(side.as_str(), "original" | "translation") && valid_text(chapter, 128),
            "invalid_content_format_request"
        );
    }
    with_consistent_snapshot(library, |root| {
        let (_, current) =
            read_item_at_sensitivity(root, item, ceiling)?.context("library_resource_not_found")?;
        anyhow::ensure!(current.revision_id == revision, "stale_item_revision");
        // Validates the manifest, edition blobs and ALL historical Evidence
        // protection before any selected text can leave this snapshot.
        let (doc, bodies) = document_revision_locked(root, &current, ceiling)?;
        let markdown = match &target {
            Target::Body {} => {
                anyhow::ensure!(
                    doc.alignment == "none" && doc.default_side == "source",
                    "chapter_required"
                );
                current.content_markdown.clone()
            }
            Target::Chapter { side, chapter } => {
                let edition = doc
                    .editions
                    .iter()
                    .find(|e| e.side == *side)
                    .context("reader_side_not_found")?;
                let selected = edition
                    .chapters
                    .iter()
                    .find(|c| c.key == *chapter)
                    .context("reader_chapter_not_found")?;
                bodies
                    .get(side)
                    .context("reader_side_not_found")?
                    .0
                    .get(selected.start..selected.end)
                    .context("content_format_unavailable")?
                    .to_owned()
            }
        };
        // Retained proof/model input must not inherit a weaker current label.
        let mut sensitivity = current.sensitivity;
        for id in &current.evidence_ids {
            let evidence: Evidence =
                load(root, root.join("evidence").join(id).join("evidence.json"))?;
            sensitivity = sensitivity.max(evidence.sensitivity);
        }
        Ok(Source {
            identity: Identity {
                item_id: item.into(),
                revision_id: revision.into(),
                revision_sha256: revision_digest(&current)?,
                target,
                source_sha256: reader_sha256(markdown.as_bytes()),
                source_bytes: markdown.len(),
                sensitivity,
                source_evidence_ids: current.evidence_ids,
            },
            markdown,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_body_identity_preserves_bytes_and_rejects_old_revision() {
        let dir = tempfile::tempdir().unwrap();
        let mut capture = crate::tests::capture();
        capture.capture.sensitivity = Sensitivity::Private;
        let mut draft = crate::tests::draft();
        draft.sensitivity = Sensitivity::Private;
        draft.content_markdown = "摘要🌹\r\n\nExact text.\n".into();
        let receipt = ingest(dir.path(), &capture, draft).unwrap();
        let item = &receipt.item_ids[0];
        let rev = &receipt.revision_ids[0];
        let read =
            |revision: &str, ceiling| source(dir.path(), item, revision, Target::Body {}, ceiling);
        let first = read(rev, Sensitivity::Private).unwrap();
        assert_eq!(first.markdown, "摘要🌹\r\n\nExact text.\n");
        assert_eq!(first.identity.source_bytes, 25);
        assert_eq!(
            first.identity.source_sha256,
            reader_sha256(first.markdown.as_bytes())
        );
        assert!(!first.identity.source_evidence_ids.is_empty());
        assert!(read(rev, Sensitivity::Public).is_err());
        let next = revise_item_metadata(
            dir.path(),
            item,
            ItemMetadataUpdate {
                expected_revision_id: rev.clone(),
                title: "New title".into(),
                manual_tags: vec![],
            },
        )
        .unwrap();
        assert_eq!(
            read(rev, Sensitivity::Private).err().unwrap().to_string(),
            "stale_item_revision"
        );
        let second = read(&next.revision_id, Sensitivity::Private).unwrap();
        assert_eq!(first.markdown, second.markdown);
        assert_ne!(
            first.identity.revision_sha256,
            second.identity.revision_sha256
        );
        assert_ne!(first.identity, second.identity);
    }
}
