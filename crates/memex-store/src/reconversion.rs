//! Owner-requested local-file reconversion. Immutable evidence precedes the
//! existing owner-edit journal; a crash may leave unreferenced evidence, never
//! a current revision whose evidence has not been durably installed.
use super::*;

pub(crate) const ACTIVITY: &str = "local_file_reconversion_v1";
const TOOL: &str = "memex.file-convert";
const VERSION: &str = "markitdown-0.1.9/memex-1";

pub struct FileReconversion {
    pub expected_revision_id: String,
    pub original_pdf_sha256: String,
    pub markdown: String,
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

pub(crate) fn validate_transition(base: &ItemRevision, next: &ItemRevision) -> Result<()> {
    let a = next
        .provenance
        .last()
        .context("missing reconversion activity")?;
    if a.activity_type != ACTIVITY
        || a.tool_id != TOOL
        || a.tool_version != VERSION
        || a.config_digest != digest(format!("{TOOL}:{VERSION}:{ACTIVITY}").as_bytes())
        || next.title != base.title
        || next.metadata != base.metadata
        || next.content_markdown.trim().is_empty()
        || next.content_markdown.len() > 16 * 1024 * 1024
        || next.content_markdown.contains('\0')
        || next.evidence_ids.len() != base.evidence_ids.len() + 1
        || next.evidence_ids[..base.evidence_ids.len()] != base.evidence_ids
        || base
            .evidence_ids
            .contains(next.evidence_ids.last().unwrap())
        || !safe_component(next.evidence_ids.last().unwrap(), "evidence_")
        || a.input_ids.len() != 3
        || a.input_checksums.len() != 3
        || !base.evidence_ids.contains(&a.input_ids[1])
        || a.input_ids.last() != next.evidence_ids.last()
        || !valid_sha256(&a.input_checksums[1])
        || a.input_checksums[2] != digest(next.content_markdown.as_bytes())
    {
        anyhow::bail!("invalid file reconversion")
    }
    // Reuse all ordinary owner-edit identity, revision, provenance-chain and
    // sensitivity checks. Only the explicitly permitted fields are normalized.
    let mut ordinary = next.clone();
    ordinary.content_markdown = base.content_markdown.clone();
    ordinary.evidence_ids = base.evidence_ids.clone();
    let a = ordinary.provenance.last_mut().unwrap();
    a.activity_type = LEGACY_EDIT_ACTIVITY.into();
    a.tool_id = "memex.desktop".into();
    a.tool_version = "1".into();
    a.config_digest = owner_edit_config_digest(LEGACY_EDIT_ACTIVITY);
    a.input_ids.truncate(1);
    a.input_checksums.truncate(1);
    validate_owner_edit_transition(base, &ordinary)
}

fn evidence(root: &Path, id: &str) -> Result<Evidence> {
    if !safe_component(id, "evidence_") {
        anyhow::bail!("invalid evidence id")
    }
    load(root, root.join("evidence").join(id).join("evidence.json"))
}

pub(crate) fn validate_evidence(root: &Path, next: &ItemRevision) -> Result<()> {
    let a = next
        .provenance
        .last()
        .context("missing reconversion activity")?;
    let pdf = evidence(root, &a.input_ids[1])?;
    let converted = evidence(root, &a.input_ids[2])?;
    let pdf_descriptor = pdf.descriptor.as_ref().context("PDF descriptor required")?;
    let descriptor = converted
        .descriptor
        .as_ref()
        .context("converted descriptor required")?;
    let rel = format!("evidence/{}/content.md", converted.evidence_id);
    if pdf_descriptor.media_type != "application/pdf"
        || format!("sha256:{}", pdf_descriptor.sha256) != a.input_checksums[1]
        || pdf.evidence_id != a.input_ids[1]
        || converted.evidence_id != a.input_ids[2]
        || pdf.sensitivity != next.sensitivity
        || converted.sensitivity != next.sensitivity
        || converted.schema_version != SCHEMA_VERSION
        || converted.source_id != pdf.source_id
        || !safe_component(&converted.capture_id, "capture_")
        || converted.blob_id != a.input_checksums[2]
        || converted.backend_ref != format!("memex-store:{rel}")
        || converted.logical_locator != rel
        || converted.supersedes.is_some()
        || descriptor.schema_version != SCHEMA_VERSION
        || descriptor.locator != rel
        || descriptor.media_type != "text/markdown"
        || descriptor.source_url.as_ref() != Some(&next.source_url)
        || descriptor.byte_size != next.content_markdown.len() as u64
        || format!("sha256:{}", descriptor.sha256) != a.input_checksums[2]
    {
        anyhow::bail!("invalid reconversion evidence")
    }
    verify_binary(
        &contained_existing(root, &root.join(&rel))?,
        next.content_markdown.as_bytes(),
    )?;
    let capture: CaptureRecord = load(
        root,
        root.join("captures")
            .join(&converted.capture_id)
            .join("capture.json"),
    )?;
    if capture.schema_version != SCHEMA_VERSION
        || capture.external_capture_id != next.revision_id
        || capture.evidence_descriptor_version != Some(1)
        || capture.capture_method != ACTIVITY
        || capture.adapter_id != TOOL
        || capture.adapter_version != VERSION
        || capture.source_id != pdf.source_id
        || capture.source_url != next.source_url
        || capture.canonical_url != next.canonical_url
        || capture.sensitivity != next.sensitivity
        || capture.blob_ids != [converted.blob_id.clone()]
        || capture.capture_id != converted.capture_id
        || capture.captured_at != a.occurred_at
    {
        anyhow::bail!("invalid reconversion capture")
    }
    Ok(())
}

/// Local owner-only API; callers must explicitly select the item and CAS revision.
/// A compatible reader must be deployed before using this on a served library.
pub fn reconvert_file_item(
    library: impl AsRef<Path>,
    item_id: &str,
    update: FileReconversion,
    max_sensitivity: Sensitivity,
) -> Result<ItemMetadataUpdateReceipt> {
    if !valid_item_id(item_id)
        || !valid_revision_id(&update.expected_revision_id)
        || !valid_lower_hex_64(&update.original_pdf_sha256)
        || update.markdown.trim().is_empty()
        || update.markdown.len() > 16 * 1024 * 1024
        || update.markdown.contains('\0')
    {
        anyhow::bail!("invalid file reconversion request")
    }
    let root = existing_canonical_root(library.as_ref())?;
    let lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    let (mut item, base) = read_item_at_sensitivity(&root, item_id, max_sensitivity)?
        .context("library_resource_not_found")?;
    if base.revision_id != update.expected_revision_id {
        anyhow::bail!("stale_item_revision")
    }
    let (_, legacy) = read_legacy_item(&root, item_id)?;
    if base.revision_number - legacy.revision_number >= MAX_ITEM_REVISION_CHAIN {
        anyhow::bail!("item_revision_limit_exceeded")
    }
    let pdf = base
        .evidence_ids
        .iter()
        .map(|id| evidence(&root, id))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .find(|e| {
            e.descriptor.as_ref().is_some_and(|d| {
                d.media_type == "application/pdf" && d.sha256 == update.original_pdf_sha256
            })
        })
        .context("original_pdf_evidence_mismatch")?;
    let mut revision =
        owner_edit_revision(&base, ACTIVITY, base.title.clone(), base.metadata.clone())?;
    revision.content_markdown = update.markdown;
    let id = new_id("evidence");
    let capture_id = new_id("capture");
    let rel = format!("evidence/{id}/content.md");
    let sha = digest(revision.content_markdown.as_bytes());
    revision.evidence_ids.push(id.clone());
    let a = revision.provenance.last_mut().unwrap();
    a.tool_id = TOOL.into();
    a.tool_version = VERSION.into();
    a.config_digest = digest(format!("{TOOL}:{VERSION}:{ACTIVITY}").as_bytes());
    a.input_ids.extend([pdf.evidence_id.clone(), id.clone()]);
    a.input_checksums.extend([
        format!("sha256:{}", update.original_pdf_sha256),
        sha.clone(),
    ]);
    let capture = CaptureRecord {
        schema_version: SCHEMA_VERSION,
        capture_id: capture_id.clone(),
        adapter_id: TOOL.into(),
        adapter_version: VERSION.into(),
        external_capture_id: revision.revision_id.clone(),
        source_id: pdf.source_id.clone(),
        captured_at: a.occurred_at.clone(),
        capture_method: ACTIVITY.into(),
        route_summary: Some("offline text-layer conversion; no model".into()),
        source_url: base.source_url.clone(),
        canonical_url: base.canonical_url.clone(),
        sensitivity: base.sensitivity,
        blob_ids: vec![sha.clone()],
        ingest_input_sha256: None,
        evidence_descriptor_version: Some(1),
    };
    let converted = Evidence {
        schema_version: SCHEMA_VERSION,
        evidence_id: id.clone(),
        source_id: pdf.source_id,
        capture_id: capture_id.clone(),
        blob_id: sha.clone(),
        logical_locator: rel.clone(),
        backend_ref: format!("memex-store:{rel}"),
        sensitivity: base.sensitivity,
        supersedes: None,
        descriptor: Some(memex_core::BlobDescriptor {
            schema_version: SCHEMA_VERSION,
            locator: rel.clone(),
            media_type: "text/markdown".into(),
            byte_size: revision.content_markdown.len() as u64,
            sha256: sha[7..].into(),
            source_url: Some(base.source_url.clone()),
        }),
    };
    validate_transition(&base, &revision)?;
    for path in [format!("evidence/{id}"), format!("captures/{capture_id}")] {
        ensure_contained_dir(&root, Path::new(&path))?;
    }
    install_binary(
        &root,
        &root.join(&rel),
        revision.content_markdown.as_bytes(),
    )?;
    install_binary(
        &root,
        &root.join(format!("evidence/{id}/evidence.json")),
        &serde_json::to_vec_pretty(&converted)?,
    )?;
    install_binary(
        &root,
        &root.join(format!("captures/{capture_id}/capture.json")),
        &serde_json::to_vec_pretty(&capture)?,
    )?;
    validate_evidence(&root, &revision)?;
    item.item_type = "document".into();
    let receipt = commit_owner_revision_locked(&root, item, &base, &revision, None)?;
    lock.unlock()?;
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, String, Item, ItemRevision) {
        let dir = tempfile::tempdir().unwrap();
        let mut c = crate::tests::capture();
        let pdf = b"%PDF-synthetic".to_vec();
        c.blobs[0].descriptor.media_type = "application/pdf".into();
        c.blobs[0].descriptor.byte_size = pdf.len() as u64;
        c.blobs[0].descriptor.sha256 = digest(&pdf)[7..].into();
        c.blobs[0].blob_id = digest(&pdf);
        c.blobs[0].bytes = pdf;
        let receipt = ingest(dir.path(), &c, crate::tests::draft()).unwrap();
        let (item, revision) = read_item(dir.path(), &receipt.item_ids[0]).unwrap();
        (dir, c.blobs[0].descriptor.sha256.clone(), item, revision)
    }
    fn request(base: &ItemRevision, hash: &str) -> FileReconversion {
        FileReconversion {
            expected_revision_id: base.revision_id.clone(),
            original_pdf_sha256: hash.into(),
            markdown: "# 合成文档\n\n第一段，保留标点。\n\nSecond paragraph: 17.5%.\n".into(),
        }
    }
    #[test]
    fn reconversion_preserves_identity_evidence_and_recovery_at_each_journal_boundary() {
        for interruption in [None, Some(0), Some(1), Some(2), Some(3), Some(4)] {
            let (dir, hash, old_item, base) = fixture();
            let generation = catalog_generation(dir.path()).unwrap();
            FAIL_ITEM_UPDATE_STEP.with(|s| s.set(interruption));
            let result = reconvert_file_item(
                dir.path(),
                &old_item.item_id,
                request(&base, &hash),
                Sensitivity::Public,
            );
            FAIL_ITEM_UPDATE_STEP.with(|s| s.set(None));
            assert_eq!(result.is_err(), interruption.is_some());
            recover_pending_ingests(dir.path()).unwrap();
            let (item, next) = read_item(dir.path(), &old_item.item_id).unwrap();
            if interruption == Some(0) {
                assert_eq!((item, next), (old_item, base));
                continue;
            }
            assert_eq!(item.item_type, "document");
            assert_eq!(item.item_id, old_item.item_id);
            assert_eq!(item.added_at, old_item.added_at);
            assert_eq!(next.source_url, base.source_url);
            assert_eq!(next.content_markdown, request(&base, &hash).markdown);
            assert_eq!(
                &next.evidence_ids[..base.evidence_ids.len()],
                base.evidence_ids
            );
            assert_eq!(
                read_legacy_item(&dir.path().canonicalize().unwrap(), &item.item_id)
                    .unwrap()
                    .1,
                base
            );
            assert_ne!(catalog_generation(dir.path()).unwrap(), generation);
            assert!(reconvert_file_item(
                dir.path(),
                &item.item_id,
                request(&base, &hash),
                Sensitivity::Public
            )
            .is_err());
            let update = ItemMetadataUpdate {
                expected_revision_id: next.revision_id.clone(),
                title: "Edited title".into(),
                manual_tags: vec![],
            };
            revise_item_metadata_at_sensitivity(
                dir.path(),
                &item.item_id,
                update,
                Sensitivity::Public,
            )
            .unwrap();
            recover_pending_ingests(dir.path()).unwrap();
            assert_eq!(
                read_item(dir.path(), &item.item_id)
                    .unwrap()
                    .1
                    .content_markdown,
                next.content_markdown
            );
            let changed_text = dir.path().join(format!(
                "evidence/{}/content.md",
                next.evidence_ids.last().unwrap()
            ));
            fs::write(changed_text, b"tampered evidence").unwrap();
            assert!(read_item(dir.path(), &item.item_id).is_err());
        }
    }
    #[test]
    fn ordinary_edits_cannot_change_body_evidence_or_type_and_bad_pdf_is_rejected() {
        let (dir, hash, item, base) = fixture();
        let ordinary = owner_edit_revision(
            &base,
            LEGACY_EDIT_ACTIVITY,
            "title".into(),
            base.metadata.clone(),
        )
        .unwrap();
        let mut tampered = ordinary.clone();
        tampered.content_markdown = "replacement".into();
        assert!(validate_owner_edit_transition(&base, &tampered).is_err());
        let mut tampered = ordinary.clone();
        tampered.evidence_ids.push(new_id("evidence"));
        assert!(validate_owner_edit_transition(&base, &tampered).is_err());
        assert!(
            validate_owner_edit_chain(&dir.path().canonicalize().unwrap(), &item, &ordinary)
                .is_ok()
        );
        let mut wrong_type = item.clone();
        wrong_type.item_type = "document".into();
        assert!(validate_owner_edit_chain(
            &dir.path().canonicalize().unwrap(),
            &wrong_type,
            &ordinary
        )
        .is_err());
        let mut bad = request(&base, &hash);
        bad.original_pdf_sha256 = "0".repeat(64);
        assert!(reconvert_file_item(dir.path(), &item.item_id, bad, Sensitivity::Public).is_err());
        assert_eq!(read_item(dir.path(), &item.item_id).unwrap(), (item, base));
    }
}
