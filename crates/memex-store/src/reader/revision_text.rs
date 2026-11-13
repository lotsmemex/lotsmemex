//! Exact historical text, reachable only through the current verified owner chain.
use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Target {
    Body {},
    Chapter { side: String, chapter: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chunk {
    pub item_id: String,
    pub revision_id: String,
    pub target: Target,
    pub sha256: String,
    pub total_bytes: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub next_offset: Option<usize>,
    pub text: String,
}

/// The named revision on the item's verified chain, authorized at `ceiling`.
/// read_item validates the FULL current chain, even if the requested node is
/// current or an early ancestor. Never opens a client-selected filename.
pub(crate) fn historical_revision(root: &Path, item: &str, revision: &str, ceiling: Sensitivity) -> Result<ItemRevision> {
    let (_, mut selected) =
        read_item_at_sensitivity(root, item, ceiling)?.context("library_resource_not_found")?;
    let (_, legacy) = read_legacy_item(root, item)?;
    while selected.revision_id != revision {
        anyhow::ensure!(selected.revision_id != legacy.revision_id, "revision_text_unavailable");
        let parent = selected
            .provenance
            .last()
            .and_then(|a| a.input_ids.first())
            .filter(|id| valid_revision_id(id))
            .context("revision_text_unavailable")?;
        selected = load(root, root.join("items").join(item).join("revisions").join(format!("{parent}.json")))?;
    }
    anyhow::ensure!(selected.item_id == item && selected.sensitivity <= ceiling, "library_resource_not_found");
    Ok(selected)
}

pub fn chunk(
    library: &Path,
    item: &str,
    revision: &str,
    target: Target,
    start_byte: usize,
    limit_bytes: usize,
    expected_sha256: &str,
    ceiling: Sensitivity,
) -> Result<Chunk> {
    anyhow::ensure!(
        valid_item_id(item) && valid_revision_id(revision),
        "invalid_reader_request"
    );
    anyhow::ensure!((1..=8192).contains(&limit_bytes), "invalid_reader_limit");
    anyhow::ensure!(
        valid_lower_hex_64(expected_sha256) || (start_byte == 0 && expected_sha256 == "any"),
        "invalid_reader_request"
    );
    if let Target::Chapter { side, chapter } = &target {
        anyhow::ensure!(
            matches!(side.as_str(), "original" | "translation") && valid_text(chapter, 128),
            "invalid_reader_request"
        );
    }
    with_consistent_snapshot(library, |root| {
        let selected = historical_revision(root, item, revision, ceiling)?;
        // Includes historical Evidence sensitivity and exact source/manifest
        // validation; current labels never grant access to protected old text.
        let (doc, bodies) = document_revision_locked(root, &selected, ceiling)?;
        let text = match &target {
            Target::Body {} => {
                anyhow::ensure!(
                    doc.alignment == "none" && doc.default_side == "source",
                    "revision_text_unavailable"
                );
                selected.content_markdown.as_str()
            }
            Target::Chapter { side, chapter } => {
                let edition = doc
                    .editions
                    .iter()
                    .find(|e| &e.side == side)
                    .context("reader_side_not_found")?;
                let ch = edition
                    .chapters
                    .iter()
                    .find(|c| &c.key == chapter)
                    .context("reader_chapter_not_found")?;
                bodies
                    .get(side)
                    .context("reader_side_not_found")?
                    .0
                    .get(ch.start..ch.end)
                    .context("revision_text_unavailable")?
            }
        };
        let sha256 = reader_sha256(text.as_bytes());
        anyhow::ensure!(
            expected_sha256 == "any" || expected_sha256 == sha256,
            "stale_item_revision"
        );
        anyhow::ensure!(
            start_byte <= text.len() && text.is_char_boundary(start_byte),
            "invalid_reader_offset"
        );
        let mut end_byte = start_byte.saturating_add(limit_bytes).min(text.len());
        while !text.is_char_boundary(end_byte) {
            end_byte -= 1;
        }
        anyhow::ensure!(
            end_byte > start_byte || start_byte == text.len(),
            "invalid_reader_limit"
        );
        Ok(Chunk {
            item_id: item.into(),
            revision_id: revision.into(),
            target,
            sha256,
            total_bytes: text.len(),
            start_byte,
            end_byte,
            next_offset: (end_byte < text.len()).then_some(end_byte),
            text: text[start_byte..end_byte].into(),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(book: bool) -> (tempfile::TempDir, String, String) {
        let dir = tempfile::tempdir().unwrap();
        let mut capture = crate::tests::capture();
        let mut draft = crate::tests::draft();
        capture.capture.sensitivity = Sensitivity::Private;
        draft.sensitivity = Sensitivity::Private;
        draft.content_markdown = "甲🌹乙\r\n\nEnd\n".into();
        let receipt = if book {
            let original = "English\r\n";
            let mut editions = vec![
                heading_edition(original),
                heading_edition(&draft.content_markdown),
            ];
            editions[0].side = "original".into();
            editions[1].side = "translation".into();
            let manifest = BookManifest {
                schema_version: 1,
                alignment: "declared_chapter_order_not_paragraph_alignment".into(),
                editions,
            };
            capture.blobs = [
                ("original.md", "text/markdown", original.as_bytes().to_vec()),
                (
                    "translation.md",
                    "text/markdown",
                    draft.content_markdown.as_bytes().to_vec(),
                ),
                (
                    "manifest.json",
                    BOOK_MANIFEST,
                    serde_json::to_vec(&manifest).unwrap(),
                ),
            ]
            .into_iter()
            .map(|(name, media, bytes)| {
                let sha = reader_sha256(&bytes);
                memex_core::VerifiedBlob {
                    descriptor: memex_core::BlobDescriptor {
                        schema_version: 1,
                        locator: name.into(),
                        media_type: media.into(),
                        byte_size: bytes.len() as u64,
                        sha256: sha.clone(),
                        source_url: None,
                    },
                    blob_id: format!("sha256:{sha}"),
                    source_backend_ref: None,
                    bytes,
                }
            })
            .collect();
            capture.capture.artifacts =
                capture.blobs.iter().map(|b| b.descriptor.clone()).collect();
            ingest_reader_book(dir.path(), capture, draft).unwrap()
        } else {
            ingest(dir.path(), &capture, draft).unwrap()
        };
        (
            dir,
            receipt.item_ids[0].clone(),
            receipt.revision_ids[0].clone(),
        )
    }

    #[test]
    fn exact_body_utf8_bounds_hash_and_authorization() {
        let (dir, item, rev) = fixture(false);
        let read = |start, limit, hash: &str, ceiling| {
            chunk(
                dir.path(),
                &item,
                &rev,
                Target::Body {},
                start,
                limit,
                hash,
                ceiling,
            )
        };
        let full = read(0, 8192, "any", Sensitivity::Private).unwrap();
        assert_eq!(full.text, "甲🌹乙\r\n\nEnd\n");
        assert_eq!(full.sha256, reader_sha256(full.text.as_bytes()));
        assert!(read(0, 8192, "any", Sensitivity::Public).is_err());
        for limit in [0, 1, 2, 8193] {
            assert!(read(0, limit, "any", Sensitivity::Private).is_err());
        }
        assert!(read(1, 4, &full.sha256, Sensitivity::Private).is_err());
        assert!(read(3, 3, &full.sha256, Sensitivity::Private).is_err());
        assert!(read(3, 4, "any", Sensitivity::Private).is_err());
        assert!(read(0, 4, &"0".repeat(64), Sensitivity::Private).is_err());
        assert!(read(usize::MAX, 4, &full.sha256, Sensitivity::Private).is_err());
        let mut offset = 0;
        let mut restored = String::new();
        loop {
            let part = read(offset, 4, &full.sha256, Sensitivity::Private).unwrap();
            assert!(part.text.len() <= 4);
            restored.push_str(&part.text);
            let Some(next) = part.next_offset else { break };
            assert!(next > offset);
            offset = next;
        }
        assert_eq!(restored, full.text);
        assert_eq!(
            read(full.total_bytes, 1, &full.sha256, Sensitivity::Private)
                .unwrap()
                .next_offset,
            None
        );
        assert!(serde_json::from_str::<Target>(r#"{"kind":"body","side":"original"}"#).is_err());
    }

    #[test]
    fn large_body_cross_item_and_corrupt_chain_fail_closed() {
        let (dir, first_item, first_revision) = fixture(false);
        let mut capture = crate::tests::capture();
        capture.capture.external_capture_id = "second-item".into();
        let mut draft = crate::tests::draft();
        let body = "Exact 🌹 text\r\n".repeat(2000);
        draft.content_markdown = body.clone();
        let receipt = ingest(dir.path(), &capture, draft).unwrap();
        let item = &receipt.item_ids[0];
        let revision = &receipt.revision_ids[0];
        assert_ne!(item, &first_item);
        for (id, rev) in [(item, &first_revision), (&first_item, revision)] {
            assert!(chunk(
                dir.path(),
                id,
                rev,
                Target::Body {},
                0,
                8192,
                "any",
                Sensitivity::Private
            )
            .is_err());
        }
        let mut restored = String::new();
        let hash = reader_sha256(body.as_bytes());
        loop {
            let part = chunk(
                dir.path(),
                item,
                revision,
                Target::Body {},
                restored.len(),
                8192,
                &hash,
                Sensitivity::Private,
            )
            .unwrap();
            assert!(part.text.len() <= 8192);
            assert_eq!(part.total_bytes, body.len());
            restored.push_str(&part.text);
            if part.next_offset.is_none() {
                break;
            }
        }
        assert_eq!(restored, body);
        let next = revise_item_metadata(
            dir.path(),
            item,
            ItemMetadataUpdate {
                expected_revision_id: revision.clone(),
                title: "Owner title".into(),
                manual_tags: vec![],
            },
        )
        .unwrap();
        assert_eq!(
            chunk(
                dir.path(),
                item,
                revision,
                Target::Body {},
                0,
                8192,
                "any",
                Sensitivity::Private
            )
            .unwrap()
            .total_bytes,
            body.len()
        );
        let path = dir
            .path()
            .join("items")
            .join(item)
            .join("revisions")
            .join(format!("{}.json", next.revision_id));
        let mut current: ItemRevision = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        current.content_markdown = "tampered".into();
        fs::write(path, serde_json::to_vec(&current).unwrap()).unwrap();
        // Even reading the intact root must validate the damaged current chain.
        assert!(chunk(
            dir.path(),
            item,
            revision,
            Target::Body {},
            0,
            8192,
            "any",
            Sensitivity::Private
        )
        .is_err());
    }

    #[test]
    fn historical_chapter_chain_side_isolation_and_current_guard() {
        let (dir, item, rev) = fixture(true);
        let target = Target::Chapter {
            side: "original".into(),
            chapter: "section-0".into(),
        };
        let old = chunk(
            dir.path(),
            &item,
            &rev,
            target.clone(),
            0,
            8192,
            "any",
            Sensitivity::Private,
        )
        .unwrap();
        assert_eq!(old.text, "English\r\n");
        let text = "New 🌹 source\r\n";
        let upload = chapter_edit::begin(
            dir.path(),
            chapter_edit::Begin {
                item_id: item.clone(),
                revision_id: rev.clone(),
                side: "original".into(),
                chapter: "section-0".into(),
                expected_chapter_sha256: old.sha256,
                request_id: "history-test".into(),
                bytes: text.len(),
                sha256: reader_sha256(text.as_bytes()),
            },
            "local_owner",
            Sensitivity::Private,
        )
        .unwrap();
        chapter_edit::chunk(
            dir.path(),
            &item,
            &upload.edit_id,
            0,
            text,
            "local_owner",
            Sensitivity::Private,
        )
        .unwrap();
        let next = chapter_edit::commit(
            dir.path(),
            &item,
            &upload.edit_id,
            "local_owner",
            Sensitivity::Private,
        )
        .unwrap();
        assert_eq!(
            chunk(
                dir.path(),
                &item,
                &rev,
                target.clone(),
                0,
                8192,
                "any",
                Sensitivity::Private
            )
            .unwrap()
            .text,
            old.text
        );
        assert_eq!(
            chunk(
                dir.path(),
                &item,
                &next.revision_id,
                target.clone(),
                0,
                8192,
                "any",
                Sensitivity::Private
            )
            .unwrap()
            .text,
            text
        );
        assert!(reader_chunk(
            dir.path(),
            &item,
            &rev,
            "original",
            "section-0",
            0,
            8192,
            Sensitivity::Private
        )
        .is_err());
        for revision in [&rev, &next.revision_id] {
            assert!(chunk(
                dir.path(),
                &item,
                revision,
                Target::Body {},
                0,
                8192,
                "any",
                Sensitivity::Private
            )
            .is_err());
            assert_eq!(
                chunk(
                    dir.path(),
                    &item,
                    revision,
                    Target::Chapter {
                        side: "translation".into(),
                        chapter: "section-0".into()
                    },
                    0,
                    8192,
                    "any",
                    Sensitivity::Private
                )
                .unwrap()
                .text,
                "甲🌹乙\r\n\nEnd\n"
            );
        }
        // An existing, valid but unrelated revision cannot be selected, even if
        // a loose copy is planted beside this item's reachable history.
        let (_, other) = read_item(dir.path(), &item).unwrap();
        let mut orphan = other.clone();
        orphan.revision_id = new_id("revision");
        fs::write(
            dir.path()
                .join("items")
                .join(&item)
                .join("revisions")
                .join(format!("{}.json", orphan.revision_id)),
            serde_json::to_vec(&orphan).unwrap(),
        )
        .unwrap();
        assert!(chunk(
            dir.path(),
            &item,
            &orphan.revision_id,
            target.clone(),
            0,
            8192,
            "any",
            Sensitivity::Private
        )
        .is_err());
        assert!(chunk(
            dir.path(),
            "../items",
            &rev,
            target.clone(),
            0,
            8192,
            "any",
            Sensitivity::Private
        )
        .is_err());
        // Historical evidence must remain protected even when revision/item
        // visibility itself permits reading.
        let path = dir
            .path()
            .join("evidence")
            .join(&other.evidence_ids[0])
            .join("evidence.json");
        let mut e: Evidence = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        e.sensitivity = Sensitivity::Restricted;
        fs::write(path, serde_json::to_vec(&e).unwrap()).unwrap();
        assert!(chunk(
            dir.path(),
            &item,
            &rev,
            target,
            0,
            8192,
            "any",
            Sensitivity::Private
        )
        .is_err());
    }
}
