//! Selected-chapter formatting transactions. Plans contain source-range edits,
//! never model-authored replacement prose. Immutable proof precedes publication.
use super::*;
use paragraphs::{Identity, Index, Pair};

pub(crate) const ACTIVITY: &str = "reader_chapter_repair_v1";
const PROOF: &str = "application/vnd.memex.chapter-repair+json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edit {
    pub range: [usize; 2],
    pub replacement: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub schema_version: u32,
    pub identity: Identity,
    pub expected_alignment_version: String,
    pub input_profile_sha256: String,
    pub original_edits: Vec<Edit>,
    pub translation_edits: Vec<Edit>,
    /// Complete partitions in the resulting chapter coordinates.
    pub pairs: Vec<Pair>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Proof {
    plan: Plan,
    index: Index,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Receipt {
    pub revision_id: String,
    pub alignment_version: String,
}

/// Read-only offline plan. Without corroborated layout evidence, preserve all
/// boundaries and represent unknown correspondence explicitly, never by position.
pub fn plan(
    library: &Path,
    item: &str,
    revision: &str,
    chapter: &str,
    ceiling: Sensitivity,
) -> Result<Plan> {
    with_consistent_snapshot(library, |root| {
        let (doc, bodies) = document_locked(root, item, revision, ceiling)?;
        let (identity, o, t) = paragraphs::bind(&doc, &bodies, chapter)?;
        let (previous, expected_alignment_version) = paragraphs::effective(root, &identity, ceiling)?;
        if let Some(index) = &previous {
            paragraphs::validate(index, &identity, &o, t.as_deref())?;
        }
        let pairs = previous
            .as_ref()
            .map(|i| i.pairs.clone())
            .unwrap_or_else(|| {
                paragraphs::blocks(&o)
                    .into_iter()
                    .map(|(range, kind)| Pair {
                        o: Some(range),
                        t: None,
                        kind,
                        relation: paragraphs::Relation::Unresolved,
                    })
                    .chain(
                        paragraphs::blocks(t.as_deref().unwrap_or(""))
                            .into_iter()
                            .map(|(range, kind)| Pair {
                                o: None,
                                t: Some(range),
                                kind,
                                relation: paragraphs::Relation::Unresolved,
                            }),
                    )
                    .collect()
            });
        Ok(Plan {
            schema_version: 1,
            identity,
            expected_alignment_version,
            input_profile_sha256: previous
                .map(|i| i.input_profile_sha256)
                .unwrap_or_else(|| reader_sha256(b"offline-no-layout-v1")),
            original_edits: vec![],
            translation_edits: vec![],
            pairs,
        })
    })
}

fn edited(body: &str, edits: &[Edit]) -> Result<String> {
    anyhow::ensure!(edits.len() <= 65536, "invalid_reader_alignment_request");
    let mut result = String::new();
    let mut cursor = 0;
    for edit in edits {
        let [start, end] = edit.range;
        let before = body
            .get(cursor..start)
            .context("invalid_reader_alignment_request")?;
        let removed = body
            .get(start..end)
            .context("invalid_reader_alignment_request")?;
        // No furniture/hyphen deletion without corroborated source geometry.
        // This first transaction primitive accepts whitespace boundaries only.
        anyhow::ensure!(
            start < end
                && removed.chars().all(char::is_whitespace)
                && matches!(edit.replacement.as_str(), " " | "\n" | "\n\n"),
            "invalid_reader_alignment_request"
        );
        result.push_str(before);
        result.push_str(&edit.replacement);
        cursor = end;
    }
    result.push_str(
        body.get(cursor..)
            .context("invalid_reader_alignment_request")?,
    );
    // Preserve heading/note membership as well as all non-whitespace text.
    // In particular, whitespace edits may not turn a footnote into body prose.
    let typed_text = |text: &str| {
        let mut parts = [String::new(), String::new(), String::new()];
        for ([start, end], kind) in paragraphs::blocks(text) {
            let slot = match kind {
                paragraphs::Kind::Paragraph => 0,
                paragraphs::Kind::Heading => 1,
                paragraphs::Kind::Note => 2,
            };
            parts[slot].extend(text[start..end].chars().filter(|c| !c.is_whitespace()));
        }
        parts
    };
    anyhow::ensure!(
        typed_text(body) == typed_text(&result),
        "invalid_reader_alignment_request"
    );
    Ok(result)
}

fn prepare(
    doc: &ReaderDocument,
    bodies: &BTreeMap<String, (String, Option<String>)>,
    plan: &Plan,
) -> Result<(
    BookManifest,
    BTreeMap<String, (String, Option<String>)>,
    Index,
)> {
    let (identity, original, translation) =
        paragraphs::bind(doc, bodies, &plan.identity.original.chapter)?;
    anyhow::ensure!(
        plan.schema_version == 1
            && identity == plan.identity
            && (plan.expected_alignment_version == "missing"
                || valid_lower_hex_64(&plan.expected_alignment_version))
            && (translation.is_some() || plan.translation_edits.is_empty()),
        "invalid_reader_alignment_request"
    );
    let mut manifest = BookManifest {
        schema_version: 1,
        alignment: doc.alignment.clone(),
        editions: doc.editions.clone(),
    };
    let mut next_bodies = bodies.clone();
    for (side, key, body, edits) in [
        (
            "original",
            Some(&identity.original.chapter),
            Some(original.as_str()),
            &plan.original_edits,
        ),
        (
            "translation",
            identity.translation.as_ref().map(|s| &s.chapter),
            translation.as_deref(),
            &plan.translation_edits,
        ),
    ] {
        let (Some(key), Some(body)) = (key, body) else {
            continue;
        };
        let new = edited(body, edits)?;
        let edition = manifest
            .editions
            .iter_mut()
            .find(|e| e.side == side)
            .context("reader_side_not_found")?;
        let position = edition
            .chapters
            .iter()
            .position(|c| &c.key == key)
            .context("reader_chapter_not_found")?;
        let chapter = &edition.chapters[position];
        let (full, evidence) = next_bodies.get_mut(side).context("reader_side_not_found")?;
        full.replace_range(chapter.start..chapter.end, &new);
        *evidence = None;
        let mut offset = edition.chapters[position].start;
        for (i, chapter) in edition.chapters.iter_mut().enumerate().skip(position) {
            let len = if i == position {
                new.len()
            } else {
                chapter.end - chapter.start
            };
            chapter.start = offset;
            offset += len;
            chapter.end = offset;
        }
        edition.bytes = full.len();
        edition.sha256 = reader_sha256(full.as_bytes());
        validate_edition(edition, full)?;
    }
    let next_doc = ReaderDocument {
        item_id: doc.item_id.clone(),
        revision_id: doc.revision_id.clone(),
        canonical_sha256: reader_sha256(next_bodies["translation"].0.as_bytes()),
        default_side: doc.default_side.clone(),
        alignment: doc.alignment.clone(),
        editions: manifest.editions.clone(),
    };
    let (identity, o, t) =
        paragraphs::bind(&next_doc, &next_bodies, &plan.identity.original.chapter)?;
    let index = Index {
        schema_version: 1,
        identity,
        algorithm: "chapter-paragraphs-v1".into(),
        input_profile_sha256: plan.input_profile_sha256.clone(),
        pairs: plan.pairs.clone(),
    };
    paragraphs::validate(&index, &index.identity, &o, t.as_deref())?;
    Ok((manifest, next_bodies, index))
}

pub(crate) fn validate_transition(base: &ItemRevision, next: &ItemRevision) -> Result<()> {
    validate_transition_for(base, next, ACTIVITY)
}

pub(super) fn validate_transition_for(base: &ItemRevision, next: &ItemRevision, activity: &str) -> Result<()> {
    let a = next
        .provenance
        .last()
        .context("invalid_reader_alignment_request")?;
    anyhow::ensure!(
        a.activity_type == activity
            && next.title == base.title
            && next.metadata == base.metadata
            && next.evidence_ids.len() == base.evidence_ids.len() + 4
            && next.evidence_ids[..base.evidence_ids.len()] == base.evidence_ids
            && next.evidence_ids.iter().collect::<BTreeSet<_>>().len() == next.evidence_ids.len()
            && next
                .evidence_ids
                .iter()
                .all(|id| safe_component(id, "evidence_"))
            && a.input_ids.len() == 5
            && a.input_checksums.len() == 5
            && a.input_ids[1..] == next.evidence_ids[base.evidence_ids.len()..]
            && a.input_checksums[1..].iter().all(|s| valid_sha256(s)),
        "invalid_reader_alignment_request"
    );
    let mut ordinary = next.clone();
    ordinary.content_markdown = base.content_markdown.clone();
    ordinary.evidence_ids = base.evidence_ids.clone();
    let a = ordinary.provenance.last_mut().unwrap();
    a.activity_type = LEGACY_EDIT_ACTIVITY.into();
    a.config_digest = owner_edit_config_digest(LEGACY_EDIT_ACTIVITY);
    a.input_ids.truncate(1);
    a.input_checksums.truncate(1);
    validate_owner_edit_transition(base, &ordinary)?;
    anyhow::ensure!(
        next.provenance.last().unwrap().config_digest == owner_edit_config_digest(activity),
        "invalid_reader_alignment_request"
    );
    Ok(())
}

fn proof(root: &Path, next: &ItemRevision) -> Result<Proof> {
    let id = next
        .evidence_ids
        .last()
        .context("invalid_reader_alignment_request")?;
    let e: Evidence = load(root, root.join("evidence").join(id).join("evidence.json"))?;
    anyhow::ensure!(
        e.descriptor.as_ref().is_some_and(|d| d.media_type == PROOF),
        "invalid_reader_alignment_request"
    );
    Ok(serde_json::from_slice(&blob(root, &e, 8 * 1024 * 1024)?)?)
}

pub(super) fn validate_bundle(
    root: &Path,
    base: &ItemRevision,
    next: &ItemRevision,
    activity: &str,
    proof_type: &str,
) -> Result<()> {
    validate_transition_for(base, next, activity)?;
    let a = next.provenance.last().unwrap();
    let source: Evidence = load(
        root,
        root.join("evidence")
            .join(&base.evidence_ids[0])
            .join("evidence.json"),
    )?;
    let mut capture_id = None;
    for (i, media) in ["text/markdown", "text/markdown", BOOK_MANIFEST, proof_type]
        .iter()
        .enumerate()
    {
        let id = &a.input_ids[i + 1];
        let e: Evidence = load(root, root.join("evidence").join(id).join("evidence.json"))?;
        anyhow::ensure!(
            e.schema_version == SCHEMA_VERSION
                && e.evidence_id == *id
                && e.sensitivity == next.sensitivity
                && e.blob_id == a.input_checksums[i + 1]
                && e.source_id == source.source_id
                && safe_component(&e.capture_id, "capture_")
                && capture_id.as_ref().is_none_or(|id| id == &e.capture_id)
                && e.supersedes.is_none()
                && e.descriptor.as_ref().is_some_and(|d| d.media_type == *media
                    && d.schema_version == SCHEMA_VERSION
                    && d.locator == e.logical_locator
                    && d.source_url.as_ref() == Some(&next.source_url)
                    && e.backend_ref == format!("memex-store:{}", d.locator)),
            "invalid_reader_alignment_request"
        );
        blob(root, &e, MAX_TEXT)?;
        capture_id = Some(e.capture_id);
    }
    let capture_id = capture_id.unwrap();
    let capture: CaptureRecord = load(
        root,
        root.join("captures").join(&capture_id).join("capture.json"),
    )?;
    anyhow::ensure!(
        capture.schema_version == SCHEMA_VERSION
            && capture.capture_id == capture_id
            && capture.source_id == source.source_id
            && capture.external_capture_id == next.revision_id
            && capture.adapter_id == "memex.desktop"
            && capture.adapter_version == "1"
            && capture.capture_method == activity
            && capture.captured_at == a.occurred_at
            && capture.sensitivity == next.sensitivity
            && capture.source_url == next.source_url
            && capture.canonical_url == next.canonical_url
            && capture.evidence_descriptor_version == Some(1)
            && capture.blob_ids == a.input_checksums[1..],
        "invalid_reader_alignment_request"
    );
    Ok(())
}

pub(crate) fn validate_evidence(root: &Path, base: &ItemRevision, next: &ItemRevision) -> Result<()> {
    validate_bundle(root, base, next, ACTIVITY, PROOF)?;
    let a = next.provenance.last().unwrap();
    let proof = proof(root, next)?;
    let (old_doc, old_bodies) = document_revision_locked(root, base, base.sensitivity)?;
    let (manifest, bodies, mut expected) = prepare(&old_doc, &old_bodies, &proof.plan)?;
    expected.identity.revision_id = next.revision_id.clone();
    let (new_doc, new_bodies) = document_revision_locked(root, next, next.sensitivity)?;
    anyhow::ensure!(
        proof.index == expected
            && new_doc.editions == manifest.editions
            && new_bodies
                .iter()
                .all(|(side, body)| bodies.get(side).is_some_and(|b| b.0 == body.0))
            && a.input_checksums[1]
                == format!("sha256:{}", reader_sha256(bodies["original"].0.as_bytes()))
            && a.input_checksums[2]
                == format!(
                    "sha256:{}",
                    reader_sha256(bodies["translation"].0.as_bytes())
                )
            && next.content_markdown == bodies["translation"].0,
        "invalid_reader_alignment_request"
    );
    Ok(())
}

// Replayed under the same owner journal lock, before publishing current.json.
pub(crate) fn install_index(root: &Path, revision: &ItemRevision) -> Result<()> {
    if revision
        .provenance
        .last()
        .is_none_or(|a| a.activity_type != ACTIVITY)
    {
        return Ok(());
    }
    let index = proof(root, revision)?.index;
    let file = paragraphs::path(root, &index.identity);
    ensure_contained_dir(root, file.parent().unwrap().strip_prefix(root)?)?;
    install_binary(root, &file, &serde_json::to_vec(&index)?)
}

/// Read-only reconciliation of an exact persisted commit intent. This never
/// installs an index or changes a revision when the commit has not happened.
pub fn committed(library: &Path, plan: &Plan, ceiling: Sensitivity) -> Result<Option<Receipt>> {
    with_consistent_snapshot(library, |root| {
        let (_, base) = read_item_at_sensitivity(root, &plan.identity.item_id, ceiling)?
            .context("library_resource_not_found")?;
        if base.revision_id != plan.identity.revision_id {
            if base.provenance.last().is_some_and(|a| a.activity_type == ACTIVITY) {
                let previous = proof(root, &base)?;
                if previous.plan == *plan {
                    return Ok(Some(Receipt { revision_id: previous.index.identity.revision_id.clone(),
                        alignment_version: previous.index.version()? }));
                }
            }
            return Ok(None);
        }
        let (doc, bodies) = document_revision_locked(root, &base, ceiling)?;
        let (_, _, index) = prepare(&doc, &bodies, plan)?;
        let (prior, _) = paragraphs::effective(root, &plan.identity, ceiling)?;
        if prior.as_ref() == Some(&index) && paragraphs::stored(root, &plan.identity)?.as_ref() == Some(&index) {
            return Ok(Some(Receipt { revision_id: index.identity.revision_id.clone(), alignment_version: index.version()? }));
        }
        Ok(None)
    })
}

/// Caller explicitly applies a source-bound plan. No inference or extraction is
/// performed while holding the writer lock. No content change means no revision.
pub fn apply(library: &Path, plan: Plan, ceiling: Sensitivity) -> Result<Receipt> {
    apply_inner(library, plan, ceiling, None::<fn(Box<dyn FnOnce() -> Result<()> + '_>) -> Result<()>>)
}

/// Store writer lock precedes the caller's authority fence. Prepare and validate
/// outside that fence; inside it the caller rechecks authority and executes the
/// single-use final publication action. Replay skips the gate. New revisions
/// deliberately create no replayable journal before publication.
pub fn apply_with_gate<G>(library: &Path, plan: Plan, ceiling: Sensitivity, gate: G) -> Result<Receipt>
where
    G: FnOnce(Box<dyn FnOnce() -> Result<()> + '_>) -> Result<()>,
{
    apply_inner(library, plan, ceiling, Some(gate))
}

fn apply_inner<G>(library: &Path, plan: Plan, ceiling: Sensitivity, gate: Option<G>) -> Result<Receipt>
where
    G: FnOnce(Box<dyn FnOnce() -> Result<()> + '_>) -> Result<()>,
{
    anyhow::ensure!(
        serde_json::to_vec(&plan)?.len() <= 8 * 1024 * 1024,
        "invalid_reader_alignment_request"
    );
    let root = existing_canonical_root(library)?;
    let lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    let (item, base) = read_item_at_sensitivity(&root, &plan.identity.item_id, ceiling)?
        .context("library_resource_not_found")?;
    // A caller may have lost the response after the journal committed. Reuse
    // that exact immutable plan, never redo a content edit or infer a new one.
    if base.revision_id != plan.identity.revision_id
        && base
            .provenance
            .last()
            .is_some_and(|a| a.activity_type == ACTIVITY)
    {
        let previous = proof(&root, &base)?;
        if previous.plan == plan {
            return Ok(Receipt {
                revision_id: previous.index.identity.revision_id.clone(),
                alignment_version: previous.index.version()?,
            });
        }
    }
    anyhow::ensure!(
        base.revision_id == plan.identity.revision_id,
        "stale_item_revision"
    );
    let (doc, bodies) = document_revision_locked(&root, &base, ceiling)?;
    let (manifest, new_bodies, mut index) = prepare(&doc, &bodies, &plan)?;
    let (prior, version) = paragraphs::effective(&root, &plan.identity, ceiling)?;
    if prior.as_ref() == Some(&index) && paragraphs::stored(&root, &plan.identity)?.as_ref() == Some(&index) {
        return Ok(Receipt {
            revision_id: index.identity.revision_id.clone(),
            alignment_version: index.version()?,
        });
    }
    anyhow::ensure!(
        version == plan.expected_alignment_version,
        "stale_chapter_alignment"
    );
    if bodies
        .iter()
        .all(|(side, body)| new_bodies[side].0 == body.0)
    {
        let file = paragraphs::path(&root, &index.identity);
        ensure_contained_dir(&root, file.parent().unwrap().strip_prefix(&root)?)?;
        let bytes = serde_json::to_vec(&index)?;
        if let Some(gate) = gate {
            durable_replace_with_gate(&root, &file, &bytes, gate)?;
        } else {
            durable_replace(&root, &file, &bytes)?;
        }
    } else {
        let (_, legacy) = read_legacy_item(&root, &item.item_id)?;
        anyhow::ensure!(
            base.revision_number - legacy.revision_number < MAX_ITEM_REVISION_CHAIN,
            "item_revision_limit_exceeded"
        );
        let mut next =
            owner_edit_revision(&base, ACTIVITY, base.title.clone(), base.metadata.clone())?;
        next.content_markdown = new_bodies["translation"].0.clone();
        index.identity.revision_id = next.revision_id.clone();
        let proof = Proof {
            plan,
            index: index.clone(),
        };
        anyhow::ensure!(
            serde_json::to_vec(&proof)?.len() <= 8 * 1024 * 1024,
            "invalid_reader_alignment_request"
        );
        let capture_id = new_id("capture");
        let source: Evidence = load(
            &root,
            root.join("evidence")
                .join(&base.evidence_ids[0])
                .join("evidence.json"),
        )?;
        let payloads = [
            (
                "text/markdown",
                new_bodies["original"].0.as_bytes().to_vec(),
            ),
            (
                "text/markdown",
                new_bodies["translation"].0.as_bytes().to_vec(),
            ),
            (BOOK_MANIFEST, serde_json::to_vec(&manifest)?),
            (PROOF, serde_json::to_vec(&proof)?),
        ];
        ensure_contained_dir(&root, Path::new("reader-blobs"))?;
        let mut blob_ids = Vec::new();
        for (media, bytes) in payloads {
            let sha = reader_sha256(&bytes);
            let id = new_id("evidence");
            let relative = format!("reader-blobs/{sha}");
            let e = Evidence {
                schema_version: SCHEMA_VERSION,
                evidence_id: id.clone(),
                source_id: source.source_id.clone(),
                capture_id: capture_id.clone(),
                blob_id: format!("sha256:{sha}"),
                logical_locator: relative.clone(),
                backend_ref: format!("memex-store:{relative}"),
                sensitivity: base.sensitivity,
                supersedes: None,
                descriptor: Some(memex_core::BlobDescriptor {
                    schema_version: SCHEMA_VERSION,
                    locator: relative.clone(),
                    media_type: media.into(),
                    byte_size: bytes.len() as u64,
                    sha256: sha,
                    source_url: Some(base.source_url.clone()),
                }),
            };
            install_binary(&root, &root.join(relative), &bytes)?;
            ensure_contained_dir(&root, Path::new(&format!("evidence/{id}")))?;
            install_binary(
                &root,
                &root.join(format!("evidence/{id}/evidence.json")),
                &serde_json::to_vec(&e)?,
            )?;
            next.evidence_ids.push(id.clone());
            let a = next.provenance.last_mut().unwrap();
            a.input_ids.push(id);
            a.input_checksums.push(e.blob_id.clone());
            blob_ids.push(e.blob_id);
        }
        let capture = CaptureRecord {
            schema_version: SCHEMA_VERSION,
            capture_id: capture_id.clone(),
            adapter_id: "memex.desktop".into(),
            adapter_version: "1".into(),
            external_capture_id: next.revision_id.clone(),
            source_id: source.source_id,
            captured_at: next.provenance.last().unwrap().occurred_at.clone(),
            capture_method: ACTIVITY.into(),
            route_summary: Some("selected-chapter formatting and alignment".into()),
            source_url: base.source_url.clone(),
            canonical_url: base.canonical_url.clone(),
            sensitivity: base.sensitivity,
            blob_ids,
            ingest_input_sha256: None,
            evidence_descriptor_version: Some(1),
        };
        ensure_contained_dir(&root, Path::new(&format!("captures/{capture_id}")))?;
        install_binary(
            &root,
            &root.join(format!("captures/{capture_id}/capture.json")),
            &serde_json::to_vec(&capture)?,
        )?;
        validate_evidence(&root, &base, &next)?;
        if let Some(gate) = gate {
            commit_owner_revision_with_gate_locked(&root, item, &base, &next, gate)?;
        } else {
            commit_owner_revision_locked(&root, item, &base, &next, None)?;
        }
    }
    lock.unlock()?;
    Ok(Receipt {
        revision_id: index.identity.revision_id.clone(),
        alignment_version: index.version()?,
    })
}
