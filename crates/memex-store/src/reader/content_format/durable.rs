//! Private immutable operation proofs, installed before the existing owner journal.
use super::*;

pub(crate) const ACTIVITY: &str = "owner_content_format_v1";
pub(crate) const RESTORE_ACTIVITY: &str = "owner_content_format_restore_v1";
const MEDIA: &str = "application/vnd.memex.content-format+json";
const PROOF_LIMIT: usize = 128 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub item_id: String,
    pub input_revision_id: String,
    pub result_revision_id: String,
    pub target: Target,
    pub changed: bool,
    pub changes_version: String,
    pub applied_count: usize,
    pub correction_count: usize,
    pub deleted_blocks: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreReceipt {
    pub item_id: String,
    pub request_id: String,
    pub revision_id: String,
    pub changed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeRow {
    pub id: String,
    pub kind: String,
    pub basis: String,
    pub reason_code: String,
    pub old_range: [usize; 2],
    pub new_range: [usize; 2],
    pub old_sha256: String,
    pub new_sha256: String,
    pub before_bytes: usize,
    pub after_bytes: usize,
    pub source_evidence_ids: Vec<String>,
}

#[derive(Serialize)]
pub struct Changes {
    pub version: String,
    pub item_id: String,
    pub input_revision_id: String,
    pub result_revision_id: String,
    pub target: Target,
    pub source_sha256: String,
    pub result_sha256: String,
    pub total: usize,
    pub offset: usize,
    pub next_offset: Option<usize>,
    pub changes: Vec<ChangeRow>,
}

#[derive(Serialize)]
pub struct ChangeText {
    pub version: String,
    pub change_id: String,
    pub part: String,
    pub total_bytes: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub next_offset: Option<usize>,
    pub text: String,
}

// Text-bearing records intentionally have no Debug implementation.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Proof {
    schema_version: u32,
    principal: String,
    request_id: String,
    identity: Identity,
    result_revision_id: String,
    before: String,
    after: String,
    changes: Vec<ChangeRow>,
    provenance: serde_json::Value,
    // Restore binds the original immutable proof, not a caller-provided body.
    restore_of: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Operation {
    proof: Proof,
    next: Option<ItemRevision>,
}

fn valid_request(request: &str, principal: &str) -> Result<()> {
    anyhow::ensure!(
        valid_request_id(request) && (principal == "local_owner" || valid_lower_hex_64(principal)),
        "invalid_content_format_request"
    );
    Ok(())
}

fn key(request: &str, principal: &str) -> Result<String> {
    valid_request(request, principal)?;
    Ok(format!(
        "format-{}",
        reader_sha256(&serde_json::to_vec(&(principal, request))?)
    ))
}

fn path(root: &Path, request: &str, principal: &str) -> Result<PathBuf> {
    Ok(root
        .join("catalog/content-format")
        .join(format!("{}.json", key(request, principal)?)))
}

fn version(proof: &Proof) -> Result<String> {
    Ok(reader_sha256(&serde_json::to_vec(proof)?))
}

fn read_operation(root: &Path, request: &str, principal: &str) -> Result<Option<Operation>> {
    let p = path(root, request, principal)?;
    match fs::symlink_metadata(&p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
        Ok(meta) => {
            anyhow::ensure!(
                meta.is_file()
                    && !meta.file_type().is_symlink()
                    && meta.len() <= (PROOF_LIMIT * 2) as u64,
                "content_format_invalid_result"
            );
            let op: Operation = load(root, p)?;
            anyhow::ensure!(
                op.proof.principal == principal && op.proof.request_id == request,
                "content_format_invalid_result"
            );
            validate_proof(&op.proof)?;
            Ok(Some(op))
        }
    }
}

fn proof_receipt(p: &Proof) -> Result<Receipt> {
    Ok(Receipt {
        item_id: p.identity.item_id.clone(),
        input_revision_id: p.identity.revision_id.clone(),
        result_revision_id: p.result_revision_id.clone(),
        target: p.identity.target.clone(),
        changed: p.before != p.after,
        changes_version: version(p)?,
        applied_count: p.changes.len(),
        correction_count: p
            .changes
            .iter()
            .filter(|c| {
                matches!(
                    c.kind.as_str(),
                    "typo" | "extraction_garble" | "punctuation"
                )
            })
            .count(),
        deleted_blocks: p
            .changes
            .iter()
            .filter(|c| c.kind == "garbage_delete")
            .count(),
    })
}

fn selected(root: &Path, revision: &ItemRevision, target: &Target) -> Result<String> {
    let (doc, bodies) = document_revision_locked(root, revision, Sensitivity::Restricted)?;
    match target {
        Target::Body {} => {
            anyhow::ensure!(
                doc.default_side == "source" && doc.alignment == "none",
                "chapter_required"
            );
            Ok(revision.content_markdown.clone())
        }
        Target::Chapter { side, chapter } => {
            anyhow::ensure!(
                matches!(side.as_str(), "original" | "translation"),
                "invalid_content_format_request"
            );
            let ch = doc
                .editions
                .iter()
                .find(|e| e.side == *side)
                .and_then(|e| e.chapters.iter().find(|c| c.key == *chapter))
                .context("reader_chapter_not_found")?;
            Ok(bodies
                .get(side)
                .context("reader_side_not_found")?
                .0
                .get(ch.start..ch.end)
                .context("content_format_invalid_result")?
                .into())
        }
    }
}

fn identity(root: &Path, base: &ItemRevision, target: Target) -> Result<Source> {
    let markdown = selected(root, base, &target)?;
    let mut sensitivity = base.sensitivity;
    for id in &base.evidence_ids {
        let e: Evidence = load(root, root.join("evidence").join(id).join("evidence.json"))?;
        sensitivity = sensitivity.max(e.sensitivity);
    }
    Ok(Source {
        identity: Identity {
            item_id: base.item_id.clone(),
            revision_id: base.revision_id.clone(),
            revision_sha256: revision_digest(base)?,
            target,
            source_sha256: reader_sha256(markdown.as_bytes()),
            source_bytes: markdown.len(),
            sensitivity,
            source_evidence_ids: base.evidence_ids.clone(),
        },
        markdown,
    })
}

fn validate_proof(p: &Proof) -> Result<()> {
    valid_request(&p.request_id, &p.principal)?;
    anyhow::ensure!(
        p.schema_version == 1
            && valid_item_id(&p.identity.item_id)
            && valid_revision_id(&p.identity.revision_id)
            && valid_revision_id(&p.result_revision_id)
            && p.before.len() == p.identity.source_bytes
            && reader_sha256(p.before.as_bytes()) == p.identity.source_sha256
            && p.before.len() <= MAX_TEXT
            && p.after.len() <= MAX_TEXT
            && !p.after.trim().is_empty()
            && !p.after.contains('\0')
            && (p.before == p.after) == (p.identity.revision_id == p.result_revision_id),
        "content_format_invalid_result"
    );
    if p.restore_of.is_some() {
        return Ok(());
    }
    anyhow::ensure!(
        p.changes.len() <= edits::MAX_EDITS,
        "content_format_invalid_result"
    );
    let (mut old, mut new) = (0, 0);
    let mut ids = BTreeSet::new();
    for c in &p.changes {
        let [a, b] = c.old_range;
        let [x, y] = c.new_range;
        anyhow::ensure!(
            a >= old
                && b >= a
                && x >= new
                && y >= x
                && p.before.get(old..a) == p.after.get(new..x)
                && p.before.get(old..a).is_some()
                && p.before.get(a..b).is_some()
                && p.after.get(x..y).is_some()
                && b - a == c.before_bytes
                && y - x == c.after_bytes
                && ids.insert(&c.id)
                && c.source_evidence_ids
                    .iter()
                    .all(|id| p.identity.source_evidence_ids.contains(id)),
            "content_format_invalid_result"
        );
        anyhow::ensure!(
            reader_sha256(p.before[a..b].as_bytes()) == c.old_sha256
                && reader_sha256(p.after[x..y].as_bytes()) == c.new_sha256,
            "content_format_invalid_result"
        );
        old = b;
        new = y;
    }
    anyhow::ensure!(
        p.before.get(old..) == p.after.get(new..),
        "content_format_invalid_result"
    );
    // Re-run the pure validator on retained spans, not on a deserialized Applied.
    // Classification support is still a trusted-caller decision, but persisted
    // evidence cannot acquire broader deletion/correction powers on recovery.
    let mut blocks: Vec<_> = edits::blocks(&p.before)?
        .into_iter()
        .map(|block| edits::ClassifiedBlock {
            block,
            role: edits::Role::Body,
            source_evidence_ids: vec![],
        })
        .collect();
    let mut proposals = Vec::with_capacity(p.changes.len());
    for c in &p.changes {
        let kind = match c.kind.as_str() {
            "whitespace" => edits::Kind::Whitespace,
            "paragraph_boundary" => edits::Kind::ParagraphBoundary,
            "garbage_delete" => edits::Kind::GarbageDelete,
            "typo" => edits::Kind::Typo,
            "extraction_garble" => edits::Kind::ExtractionGarble,
            "punctuation" => edits::Kind::Punctuation,
            _ => anyhow::bail!("content_format_invalid_result"),
        };
        let basis = match c.basis.as_str() {
            "deterministic" => edits::Basis::Deterministic,
            "source_verified" => edits::Basis::SourceVerified,
            "context_inferred" => edits::Basis::ContextInferred,
            _ => anyhow::bail!("content_format_invalid_result"),
        };
        let reason_code = match c.reason_code.as_str() {
            "navigation" => edits::Reason::Navigation,
            "advertisement" => edits::Reason::Advertisement,
            "saved_control" => edits::Reason::SavedControl,
            "account_widget" => edits::Reason::AccountWidget,
            "paragraph_layout" => edits::Reason::ParagraphLayout,
            "short_correction" => edits::Reason::ShortCorrection,
            "source_garble" => edits::Reason::SourceGarble,
            "punctuation" => edits::Reason::Punctuation,
            _ => anyhow::bail!("content_format_invalid_result"),
        };
        let block_id = if kind == edits::Kind::GarbageDelete {
            let block = blocks
                .iter_mut()
                .find(|b| b.block.range == c.old_range)
                .context("content_format_invalid_result")?;
            block.role = edits::Role::Furniture {
                reason: reason_code,
                basis,
            };
            block.source_evidence_ids = c.source_evidence_ids.clone();
            Some(block.block.id.clone())
        } else {
            None
        };
        proposals.push(edits::ProposedEdit {
            old_range: c.old_range,
            old_sha256: c.old_sha256.clone(),
            replacement: p.after[c.new_range[0]..c.new_range[1]].into(),
            kind,
            basis,
            reason_code,
            source_evidence_ids: c.source_evidence_ids.clone(),
            block_id,
        });
    }
    let applied = edits::apply(
        &p.before,
        &p.identity.source_sha256,
        &blocks,
        &p.identity.source_evidence_ids,
        &proposals,
    )?;
    anyhow::ensure!(
        applied.output() == p.after
            && applied
                .changes()
                .iter()
                .zip(&p.changes)
                .all(|(actual, stored)| actual.id() == stored.id
                    && actual.new_range() == stored.new_range),
        "content_format_invalid_result"
    );
    Ok(())
}

// Rebuild only the selected side. Original manifest identities and every other
// chapter's bytes survive; only subsequent offsets and the selected side hash move.
fn replacement(
    root: &Path,
    base: &ItemRevision,
    target: &Target,
    text: &str,
) -> Result<(String, Vec<(&'static str, Vec<u8>)>)> {
    if matches!(target, Target::Body {}) {
        return Ok((text.into(), vec![]));
    }
    let Target::Chapter { side, chapter } = target else {
        unreachable!()
    };
    let (doc, mut bodies) = document_revision_locked(root, base, Sensitivity::Restricted)?;
    let mut manifest = BookManifest {
        schema_version: 1,
        alignment: doc.alignment,
        editions: doc.editions,
    };
    let edition = manifest
        .editions
        .iter_mut()
        .find(|e| e.side == *side)
        .context("reader_side_not_found")?;
    let pos = edition
        .chapters
        .iter()
        .position(|c| c.key == *chapter)
        .context("reader_chapter_not_found")?;
    let ch = &edition.chapters[pos];
    let body = &mut bodies.get_mut(side).context("reader_side_not_found")?.0;
    anyhow::ensure!(
        body.len() - (ch.end - ch.start) + text.len() <= MAX_TEXT,
        "reader_edit_too_large"
    );
    body.replace_range(ch.start..ch.end, text);
    let mut offset = ch.start;
    for (i, ch) in edition.chapters.iter_mut().enumerate().skip(pos) {
        let len = if i == pos {
            text.len()
        } else {
            ch.end - ch.start
        };
        ch.start = offset;
        offset += len;
        ch.end = offset;
    }
    edition.bytes = body.len();
    edition.sha256 = reader_sha256(body.as_bytes());
    validate_edition(edition, body)?;
    Ok((
        bodies["translation"].0.clone(),
        vec![
            ("text/markdown", bodies["original"].0.as_bytes().to_vec()),
            ("text/markdown", bodies["translation"].0.as_bytes().to_vec()),
            (BOOK_MANIFEST, serde_json::to_vec(&manifest)?),
        ],
    ))
}

fn install_bundle(
    root: &Path,
    base: &ItemRevision,
    next: &mut ItemRevision,
    p: &Proof,
) -> Result<()> {
    let (canonical, mut payloads) = replacement(root, base, &p.identity.target, &p.after)?;
    next.content_markdown = canonical;
    payloads.push((MEDIA, serde_json::to_vec(p)?));
    let source: Evidence = load(
        root,
        root.join("evidence")
            .join(&base.evidence_ids[0])
            .join("evidence.json"),
    )?;
    let capture_id = new_id("capture");
    let mut blob_ids = Vec::new();
    ensure_contained_dir(root, Path::new("reader-blobs"))?;
    for (media, bytes) in payloads {
        anyhow::ensure!(bytes.len() <= PROOF_LIMIT, "content_format_invalid_result");
        let sha = reader_sha256(&bytes);
        let evidence_id = new_id("evidence");
        let relative = format!("reader-blobs/{sha}");
        let e = Evidence {
            schema_version: 1,
            evidence_id: evidence_id.clone(),
            source_id: source.source_id.clone(),
            capture_id: capture_id.clone(),
            blob_id: format!("sha256:{sha}"),
            logical_locator: relative.clone(),
            backend_ref: format!("memex-store:{relative}"),
            sensitivity: p.identity.sensitivity,
            supersedes: None,
            descriptor: Some(memex_core::BlobDescriptor {
                schema_version: 1,
                locator: relative.clone(),
                media_type: media.into(),
                byte_size: bytes.len() as u64,
                sha256: sha,
                source_url: Some(base.source_url.clone()),
            }),
        };
        install_binary(root, &root.join(relative), &bytes)?;
        let dir = ensure_contained_dir(root, Path::new(&format!("evidence/{evidence_id}")))?;
        install_binary(root, &dir.join("evidence.json"), &serde_json::to_vec(&e)?)?;
        next.evidence_ids.push(evidence_id.clone());
        let a = next.provenance.last_mut().unwrap();
        a.input_ids.push(evidence_id);
        a.input_checksums.push(e.blob_id.clone());
        blob_ids.push(e.blob_id);
    }
    let a = next.provenance.last().unwrap();
    let capture = CaptureRecord {
        schema_version: 1,
        capture_id: capture_id.clone(),
        adapter_id: "memex.desktop".into(),
        adapter_version: "1".into(),
        external_capture_id: next.revision_id.clone(),
        source_id: source.source_id,
        captured_at: a.occurred_at.clone(),
        capture_method: a.activity_type.clone(),
        route_summary: Some("owner selected content formatting".into()),
        source_url: base.source_url.clone(),
        canonical_url: base.canonical_url.clone(),
        sensitivity: p.identity.sensitivity,
        blob_ids,
        ingest_input_sha256: None,
        evidence_descriptor_version: Some(1),
    };
    let dir = ensure_contained_dir(root, Path::new(&format!("captures/{capture_id}")))?;
    install_binary(
        root,
        &dir.join("capture.json"),
        &serde_json::to_vec(&capture)?,
    )?;
    Ok(())
}

pub(crate) fn validate_transition(base: &ItemRevision, next: &ItemRevision) -> Result<()> {
    let a = next
        .provenance
        .last()
        .context("content_format_invalid_result")?;
    let count = next
        .evidence_ids
        .len()
        .checked_sub(base.evidence_ids.len())
        .context("content_format_invalid_result")?;
    anyhow::ensure!(
        matches!(a.activity_type.as_str(), ACTIVITY | RESTORE_ACTIVITY)
            && matches!(count, 1 | 4)
            && next.title == base.title
            && next.metadata == base.metadata
            && next.evidence_ids[..base.evidence_ids.len()] == base.evidence_ids
            && next.evidence_ids.iter().collect::<BTreeSet<_>>().len() == next.evidence_ids.len()
            && next
                .evidence_ids
                .iter()
                .all(|id| safe_component(id, "evidence_"))
            && a.input_ids.len() == count + 1
            && a.input_checksums.len() == count + 1
            && a.input_ids[1..] == next.evidence_ids[base.evidence_ids.len()..]
            && a.input_checksums[1..].iter().all(|s| valid_sha256(s))
            && a.config_digest == owner_edit_config_digest(&a.activity_type),
        "content_format_invalid_result"
    );
    let mut ordinary = next.clone();
    ordinary.content_markdown = base.content_markdown.clone();
    ordinary.evidence_ids = base.evidence_ids.clone();
    let a = ordinary.provenance.last_mut().unwrap();
    a.activity_type = LEGACY_EDIT_ACTIVITY.into();
    a.config_digest = owner_edit_config_digest(LEGACY_EDIT_ACTIVITY);
    a.input_ids.truncate(1);
    a.input_checksums.truncate(1);
    validate_owner_edit_transition(base, &ordinary)
}

pub(crate) fn validate_evidence(
    root: &Path,
    base: &ItemRevision,
    next: &ItemRevision,
) -> Result<()> {
    validate_transition(base, next)?;
    let a = next.provenance.last().unwrap();
    let e: Evidence = load(
        root,
        root.join("evidence")
            .join(next.evidence_ids.last().unwrap())
            .join("evidence.json"),
    )?;
    let p: Proof = serde_json::from_slice(&blob(root, &e, PROOF_LIMIT)?)?;
    validate_proof(&p)?;
    let source = identity(root, base, p.identity.target.clone())?;
    anyhow::ensure!(
        source.identity == p.identity
            && source.markdown == p.before
            && base.sensitivity >= p.identity.sensitivity
            && next.revision_id == p.result_revision_id
            && (a.activity_type == RESTORE_ACTIVITY) == p.restore_of.is_some(),
        "content_format_invalid_result"
    );
    if let Some(original_version) = &p.restore_of {
        // The original proof remains in the base revision's immutable Evidence.
        let original_e: Evidence = load(
            root,
            root.join("evidence")
                .join(base.evidence_ids.last().unwrap())
                .join("evidence.json"),
        )?;
        let original: Proof = serde_json::from_slice(&blob(root, &original_e, PROOF_LIMIT)?)?;
        anyhow::ensure!(
            version(&original)? == *original_version
                && original.restore_of.is_none()
                && original.identity.target == p.identity.target
                && original.result_revision_id == base.revision_id
                && original.before == p.after
                && original.after == p.before
                && base.sensitivity >= original.identity.sensitivity,
            "content_format_restore_conflict"
        );
    }
    let (canonical, mut payloads) = replacement(root, base, &p.identity.target, &p.after)?;
    payloads.push((MEDIA, serde_json::to_vec(&p)?));
    anyhow::ensure!(
        canonical == next.content_markdown && payloads.len() + 1 == a.input_ids.len(),
        "content_format_invalid_result"
    );
    let source_e: Evidence = load(
        root,
        root.join("evidence")
            .join(&base.evidence_ids[0])
            .join("evidence.json"),
    )?;
    let mut capture_id = None;
    for (i, (media, expected)) in payloads.iter().enumerate() {
        let id = &a.input_ids[i + 1];
        let e: Evidence = load(root, root.join("evidence").join(id).join("evidence.json"))?;
        anyhow::ensure!(
            e.schema_version == 1
                && e.evidence_id == *id
                && e.source_id == source_e.source_id
                && e.sensitivity == p.identity.sensitivity
                && e.supersedes.is_none()
                && safe_component(&e.capture_id, "capture_")
                && capture_id.as_ref().is_none_or(|id| id == &e.capture_id)
                && e.blob_id == a.input_checksums[i + 1]
                && e.descriptor.as_ref().is_some_and(|d| d.schema_version == 1
                    && d.media_type == *media
                    && d.locator == e.logical_locator
                    && d.source_url.as_ref() == Some(&next.source_url)
                    && e.backend_ref == format!("memex-store:{}", d.locator))
                && blob(root, &e, PROOF_LIMIT)? == *expected,
            "content_format_invalid_result"
        );
        capture_id = Some(e.capture_id);
    }
    let id = capture_id.unwrap();
    let capture: CaptureRecord = load(root, root.join("captures").join(&id).join("capture.json"))?;
    anyhow::ensure!(
        capture.schema_version == 1
            && capture.capture_id == id
            && capture.source_id == source_e.source_id
            && capture.external_capture_id == next.revision_id
            && capture.adapter_id == "memex.desktop"
            && capture.adapter_version == "1"
            && capture.capture_method == a.activity_type
            && capture.captured_at == a.occurred_at
            && capture.sensitivity == p.identity.sensitivity
            && capture.source_url == base.source_url
            && capture.canonical_url == base.canonical_url
            && capture.evidence_descriptor_version == Some(1)
            && capture.blob_ids == a.input_checksums[1..],
        "content_format_invalid_result"
    );
    Ok(())
}

fn authorized(root: &Path, p: &Proof, ceiling: Sensitivity) -> Result<(Item, ItemRevision)> {
    let current = read_item_at_sensitivity(root, &p.identity.item_id, ceiling)?
        .context("library_resource_not_found")?;
    anyhow::ensure!(
        p.identity.sensitivity <= ceiling,
        "library_resource_not_found"
    );
    document_revision_locked(root, &current.1, ceiling)?;
    Ok(current)
}

fn settled(root: &Path, p: &Proof) -> Result<bool> {
    let Some(record) =
        metadata_v2::load_metadata_request(root, &key(&p.request_id, &p.principal)?)?
    else {
        return Ok(false);
    };
    anyhow::ensure!(
        record.request_sha256 == format!("sha256:{}", version(p)?)
            && record.receipt.item_id == p.identity.item_id
            && record.receipt.previous_revision_id == p.identity.revision_id
            && record.receipt.revision_id == p.result_revision_id,
        "request_id_conflict"
    );
    Ok(true)
}

fn finish(root: &Path, op: &Operation, ceiling: Sensitivity) -> Result<Receipt> {
    let p = &op.proof;
    let (item, base) = authorized(root, p, ceiling)?;
    if settled(root, p)? {
        return proof_receipt(p);
    }
    anyhow::ensure!(
        base.revision_id == p.identity.revision_id,
        "stale_item_revision"
    );
    let source = identity(root, &base, p.identity.target.clone())?;
    anyhow::ensure!(
        source.identity == p.identity
            && source.markdown == p.before
            && base.sensitivity >= p.identity.sensitivity,
        "stale_item_revision"
    );
    let id = key(&p.request_id, &p.principal)?;
    let hash = format!("sha256:{}", version(p)?);
    if let Some(next) = &op.next {
        let (_, legacy) = read_legacy_item(root, &item.item_id)?;
        anyhow::ensure!(
            base.revision_number - legacy.revision_number < MAX_ITEM_REVISION_CHAIN,
            "item_revision_limit_exceeded"
        );
        validate_evidence(root, &base, next)?;
        commit_owner_revision_locked(root, item, &base, next, Some((id, hash)))?;
    } else {
        anyhow::ensure!(p.before == p.after, "content_format_invalid_result");
        metadata_v2::write_metadata_request(
            root,
            &MetadataEditRequestRecord {
                schema_version: 1,
                request_id: id,
                request_sha256: hash,
                receipt: ItemMetadataUpdateReceipt {
                    schema_version: 1,
                    item_id: item.item_id,
                    previous_revision_id: base.revision_id.clone(),
                    revision_id: base.revision_id,
                    revision_number: base.revision_number,
                },
            },
        )?;
    }
    proof_receipt(p)
}

fn prepare(root: &Path, base: &ItemRevision, mut p: Proof) -> Result<Operation> {
    let next = if p.before == p.after {
        None
    } else {
        let activity = if p.restore_of.is_some() {
            RESTORE_ACTIVITY
        } else {
            ACTIVITY
        };
        let mut next =
            owner_edit_revision(base, activity, base.title.clone(), base.metadata.clone())?;
        p.result_revision_id = next.revision_id.clone();
        install_bundle(root, base, &mut next, &p)?;
        validate_evidence(root, base, &next)?;
        Some(next)
    };
    validate_proof(&p)?;
    let op = Operation { proof: p, next };
    let dir = ensure_contained_dir(root, Path::new("catalog/content-format"))?;
    #[cfg(unix)]
    fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    install_binary(
        root,
        &path(root, &op.proof.request_id, &op.proof.principal)?,
        &serde_json::to_vec(&op)?,
    )?;
    Ok(op)
}

/// Commit one validated target. request_id is a principal/library scoped operation
/// identity (the Agent maps its frozen batch target to this identity).
pub fn publish(
    library: &Path,
    identity: Identity,
    request_id: &str,
    principal: &str,
    applied: &edits::Applied,
    provenance: serde_json::Value,
    ceiling: Sensitivity,
) -> Result<Receipt> {
    valid_request(request_id, principal)?;
    anyhow::ensure!(
        identity.source_sha256 == applied.source_sha256()
            && identity.sensitivity <= ceiling
            && identity.sensitivity != Sensitivity::Restricted
            && serde_json::to_vec(&provenance)?.len() <= 65536,
        "content_format_invalid_result"
    );
    let root = existing_canonical_root(library)?;
    let _lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    let changes: Vec<_> = applied
        .changes()
        .iter()
        .map(|c| {
            let p = c.proposal();
            ChangeRow {
                id: c.id().into(),
                kind: match p.kind {
                    edits::Kind::Whitespace => "whitespace",
                    edits::Kind::ParagraphBoundary => "paragraph_boundary",
                    edits::Kind::GarbageDelete => "garbage_delete",
                    edits::Kind::Typo => "typo",
                    edits::Kind::ExtractionGarble => "extraction_garble",
                    edits::Kind::Punctuation => "punctuation",
                }
                .into(),
                basis: match p.basis {
                    edits::Basis::Deterministic => "deterministic",
                    edits::Basis::SourceVerified => "source_verified",
                    edits::Basis::ContextInferred => "context_inferred",
                }
                .into(),
                reason_code: match p.reason_code {
                    edits::Reason::Navigation => "navigation",
                    edits::Reason::Advertisement => "advertisement",
                    edits::Reason::SavedControl => "saved_control",
                    edits::Reason::AccountWidget => "account_widget",
                    edits::Reason::ParagraphLayout => "paragraph_layout",
                    edits::Reason::ShortCorrection => "short_correction",
                    edits::Reason::SourceGarble => "source_garble",
                    edits::Reason::Punctuation => "punctuation",
                }
                .into(),
                old_range: p.old_range,
                new_range: c.new_range(),
                old_sha256: p.old_sha256.clone(),
                new_sha256: c.new_sha256().into(),
                before_bytes: c.before().len(),
                after_bytes: c.after().len(),
                source_evidence_ids: p.source_evidence_ids.clone(),
            }
        })
        .collect();
    if let Some(op) = read_operation(&root, request_id, principal)? {
        anyhow::ensure!(
            op.proof.identity == identity
                && op.proof.after == applied.output()
                && op.proof.changes == changes
                && op.proof.provenance == provenance
                && op.proof.restore_of.is_none(),
            "request_id_conflict"
        );
        return finish(&root, &op, ceiling);
    }
    let (_, base) = read_item_at_sensitivity(&root, &identity.item_id, ceiling)?
        .context("library_resource_not_found")?;
    let actual = self::identity(&root, &base, identity.target.clone())?;
    anyhow::ensure!(actual.identity == identity, "stale_item_revision");
    let p = Proof {
        schema_version: 1,
        principal: principal.into(),
        request_id: request_id.into(),
        result_revision_id: identity.revision_id.clone(),
        identity,
        before: actual.markdown,
        after: applied.output().into(),
        changes,
        provenance,
        restore_of: None,
    };
    let op = prepare(&root, &base, p)?;
    finish(&root, &op, ceiling)
}

/// Recover a settled publication before looking up its now-stale input revision.
/// Pending owner journals reconcile under the normal consistent-snapshot lock.
pub fn receipt(
    library: &Path,
    item_id: &str,
    request_id: &str,
    principal: &str,
    ceiling: Sensitivity,
) -> Result<Option<Receipt>> {
    valid_request(request_id, principal)?;
    anyhow::ensure!(valid_item_id(item_id), "invalid_content_format_request");
    with_consistent_snapshot(library, |root| {
        read_item_at_sensitivity(root, item_id, ceiling)?.context("library_resource_not_found")?;
        let Some(op) = read_operation(root, request_id, principal)? else {
            return Ok(None);
        };
        anyhow::ensure!(
            op.proof.identity.item_id == item_id && op.proof.restore_of.is_none(),
            "request_id_conflict"
        );
        authorized(root, &op.proof, ceiling)?;
        if settled(root, &op.proof)? {
            Ok(Some(proof_receipt(&op.proof)?))
        } else {
            Ok(None)
        }
    })
}

fn readable(
    root: &Path,
    request: &str,
    principal: &str,
    expected: &str,
    ceiling: Sensitivity,
) -> Result<Proof> {
    let op = read_operation(root, request, principal)?.context("content_format_changes_stale")?;
    authorized(root, &op.proof, ceiling)?;
    anyhow::ensure!(
        settled(root, &op.proof)?
            && version(&op.proof)? == expected
            && op.proof.restore_of.is_none(),
        "content_format_changes_stale"
    );
    Ok(op.proof)
}

pub fn changes_list(
    library: &Path,
    request_id: &str,
    principal: &str,
    expected_version: &str,
    offset: usize,
    limit: usize,
    ceiling: Sensitivity,
) -> Result<Changes> {
    anyhow::ensure!((1..=64).contains(&limit), "invalid_content_format_request");
    with_consistent_snapshot(library, |root| {
        let p = readable(root, request_id, principal, expected_version, ceiling)?;
        let total = p.changes.len();
        anyhow::ensure!(offset <= total, "invalid_content_format_request");
        let end = offset.saturating_add(limit).min(total);
        Ok(Changes {
            version: expected_version.into(),
            item_id: p.identity.item_id,
            input_revision_id: p.identity.revision_id,
            result_revision_id: p.result_revision_id,
            target: p.identity.target,
            source_sha256: p.identity.source_sha256,
            result_sha256: reader_sha256(p.after.as_bytes()),
            total,
            offset,
            next_offset: (end < total).then_some(end),
            changes: p.changes[offset..end].to_vec(),
        })
    })
}

pub fn change_text(
    library: &Path,
    request_id: &str,
    principal: &str,
    expected_version: &str,
    change_id: &str,
    part: &str,
    start_byte: usize,
    limit_bytes: usize,
    ceiling: Sensitivity,
) -> Result<ChangeText> {
    anyhow::ensure!(
        (1..=8192).contains(&limit_bytes) && matches!(part, "before" | "after"),
        "invalid_content_format_request"
    );
    with_consistent_snapshot(library, |root| {
        let p = readable(root, request_id, principal, expected_version, ceiling)?;
        let c = p
            .changes
            .iter()
            .find(|c| c.id == change_id)
            .context("content_format_changes_stale")?;
        let text = if part == "before" {
            &p.before[c.old_range[0]..c.old_range[1]]
        } else {
            &p.after[c.new_range[0]..c.new_range[1]]
        };
        anyhow::ensure!(
            start_byte <= text.len() && text.is_char_boundary(start_byte),
            "invalid_reader_offset"
        );
        let mut end = start_byte.saturating_add(limit_bytes).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        anyhow::ensure!(
            end > start_byte || start_byte == text.len(),
            "invalid_reader_limit"
        );
        Ok(ChangeText {
            version: expected_version.into(),
            change_id: change_id.into(),
            part: part.into(),
            total_bytes: text.len(),
            start_byte,
            end_byte: end,
            next_offset: (end < text.len()).then_some(end),
            text: text[start_byte..end].into(),
        })
    })
}

/// Read an already settled restore without admitting a new mutation. This lets
/// callers honor exact replay before their active-job exclusion gate.
pub fn restore_receipt(
    library: &Path,
    operation_request_id: &str,
    principal: &str,
    expected_version: &str,
    expected_revision_id: &str,
    request_id: &str,
    ceiling: Sensitivity,
) -> Result<Option<RestoreReceipt>> {
    valid_request(request_id, principal)?;
    with_consistent_snapshot(library, |root| {
        let original = readable(
            root,
            operation_request_id,
            principal,
            expected_version,
            ceiling,
        )?;
        anyhow::ensure!(
            original.result_revision_id == expected_revision_id,
            "content_format_restore_conflict"
        );
        let Some(op) = read_operation(root, request_id, principal)? else {
            return Ok(None);
        };
        anyhow::ensure!(
            op.proof.restore_of.as_deref() == Some(expected_version)
                && op.proof.identity.revision_id == expected_revision_id,
            "request_id_conflict"
        );
        authorized(root, &op.proof, ceiling)?;
        if !settled(root, &op.proof)? {
            return Ok(None);
        }
        let r = proof_receipt(&op.proof)?;
        Ok(Some(RestoreReceipt {
            item_id: r.item_id,
            request_id: request_id.into(),
            revision_id: r.result_revision_id,
            changed: r.changed,
        }))
    })
}

pub fn restore(
    library: &Path,
    operation_request_id: &str,
    principal: &str,
    expected_version: &str,
    expected_revision_id: &str,
    request_id: &str,
    ceiling: Sensitivity,
) -> Result<RestoreReceipt> {
    valid_request(request_id, principal)?;
    let root = existing_canonical_root(library)?;
    let _lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    let original = readable(
        &root,
        operation_request_id,
        principal,
        expected_version,
        ceiling,
    )?;
    anyhow::ensure!(
        original.result_revision_id == expected_revision_id,
        "content_format_restore_conflict"
    );
    let op = if let Some(op) = read_operation(&root, request_id, principal)? {
        anyhow::ensure!(
            op.proof.restore_of.as_deref() == Some(expected_version)
                && op.proof.identity.revision_id == expected_revision_id,
            "request_id_conflict"
        );
        op
    } else {
        let (_, base) = authorized(&root, &original, ceiling)?;
        anyhow::ensure!(
            base.revision_id == expected_revision_id
                && base.sensitivity >= original.identity.sensitivity,
            "content_format_restore_conflict"
        );
        let actual = identity(&root, &base, original.identity.target.clone())?;
        anyhow::ensure!(
            actual.markdown == original.after,
            "content_format_restore_conflict"
        );
        prepare(
            &root,
            &base,
            Proof {
                schema_version: 1,
                principal: principal.into(),
                request_id: request_id.into(),
                result_revision_id: base.revision_id.clone(),
                identity: actual.identity,
                before: actual.markdown,
                after: original.before,
                changes: vec![],
                provenance: serde_json::json!({"operation": "restore"}),
                restore_of: Some(expected_version.into()),
            },
        )?
    };
    let r = finish(&root, &op, ceiling)?;
    Ok(RestoreReceipt {
        item_id: r.item_id,
        request_id: request_id.into(),
        revision_id: r.result_revision_id,
        changed: r.changed,
    })
}
