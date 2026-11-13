//! Retrospective correspondence never changes the immutable translation projection.
use super::*;
use chapter_translation as tr;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub source: tr::Identity,
    pub projection_identity: tr::Identity,
    /// Hash of the complete stored projection, not its inherited view version.
    pub projection_sha256: String,
    pub translation_version: String,
    pub target_sha256: String,
    pub target_bytes: usize,
}

// Deliberately neither Debug nor Serialize: bodies are not diagnostics.
pub struct Source {
    pub identity: Identity,
    pub source_markdown: String,
    pub target_markdown: String,
    pub recorded_map: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    pub route: tr::Route,
    pub input_profile_sha256: String,
    pub prompt_version: String,
    pub prompt_sha256: String,
    pub algorithm: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub job_id: String,
    pub identity: Identity,
    pub map_version: String,
}

#[derive(Debug, Serialize)]
pub struct Page {
    #[serde(flatten)]
    pub alignment: tr::AlignmentPage,
    pub translation_version: String,
    pub certified: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    schema_version: u32,
    identity: Identity,
    provenance: Provenance,
    job_id: String,
    pairs: Vec<paragraphs::Pair>,
}

fn source_locked(
    root: &Path,
    item: &str,
    revision: &str,
    chapter: &str,
    translation_version: &str,
    ceiling: Sensitivity,
) -> Result<Source> {
    let s = tr::source_locked(root, item, revision, chapter, ceiling)?;
    let (p, m) = tr::effective(root, &s, ceiling)?.context("stale_reader_translation")?;
    anyhow::ensure!(m.version == translation_version, "stale_reader_translation");
    Ok(Source {
        identity: Identity {
            source: s.identity,
            projection_sha256: p.metadata()?.version,
            projection_identity: p.identity,
            translation_version: m.version,
            target_sha256: m.sha256,
            target_bytes: m.bytes,
        },
        source_markdown: s.markdown,
        target_markdown: p.markdown,
        recorded_map: p.blocks.is_some(),
    })
}

pub fn source(
    library: &Path,
    item: &str,
    revision: &str,
    chapter: &str,
    translation_version: &str,
    ceiling: Sensitivity,
) -> Result<Source> {
    with_consistent_snapshot(library, |root| {
        let s = source_locked(root, item, revision, chapter, translation_version, ceiling)?;
        effective(root, &s, ceiling)?;
        Ok(s)
    })
}

fn path(root: &Path, id: &tr::Identity) -> PathBuf {
    root.join("catalog/chapter-translation-maps")
        .join(&id.item_id)
        .join(&id.revision_id)
        .join(format!(
            "{}.json",
            reader_sha256(id.original_chapter.as_bytes())
        ))
}

fn validate(a: &Artifact, id: &Identity, s: &Source) -> Result<()> {
    let p = &a.provenance;
    anyhow::ensure!(
        a.schema_version == 1
            && a.identity == *id
            && valid_text(&a.job_id, 128)
            && valid_text(&p.algorithm, 128)
            && valid_text(&p.prompt_version, 128)
            && valid_lower_hex_64(&p.prompt_sha256)
            && valid_lower_hex_64(&p.input_profile_sha256)
            && valid_text(&p.route.provider_id, 80)
            && valid_text(&p.route.model_id, 256)
            && p.route.reasoning_effort.as_deref().is_none_or(|e| matches!(
                e,
                "none" | "minimal" | "low" | "medium" | "high" | "xhigh"
            )),
        "invalid_reader_alignment_request"
    );
    // Reuse the established complete-block, monotone partition validator.
    let side = |sha256: &str, bytes| paragraphs::Side {
        chapter: id.source.original_chapter.clone(),
        chapter_start: 0,
        edition_sha256: sha256.into(),
        sha256: sha256.into(),
        bytes,
    };
    let expected = paragraphs::Identity {
        item_id: id.source.item_id.clone(),
        revision_id: id.source.revision_id.clone(),
        canonical_sha256: id.source.source_sha256.clone(),
        original: side(&id.source.source_sha256, id.source.source_bytes),
        translation: Some(side(&id.target_sha256, id.target_bytes)),
    };
    paragraphs::validate(
        &paragraphs::Index {
            schema_version: 1,
            identity: expected.clone(),
            algorithm: "chapter-paragraphs-v1".into(),
            input_profile_sha256: p.input_profile_sha256.clone(),
            pairs: a.pairs.clone(),
        },
        &expected,
        &s.source_markdown,
        Some(&s.target_markdown),
    )
}

fn stored(root: &Path, id: &Identity, s: &Source) -> Result<Option<Artifact>> {
    let file = path(root, &id.source);
    match fs::symlink_metadata(&file) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
        Ok(m) => {
            anyhow::ensure!(
                m.is_file() && !m.file_type().is_symlink() && m.len() <= 8 * 1024 * 1024,
                "invalid_reader_alignment_request"
            );
            let a = load(root, file)?;
            validate(&a, id, s)?;
            Ok(Some(a))
        }
    }
}

fn effective(root: &Path, s: &Source, ceiling: Sensitivity) -> Result<Option<(Artifact, String)>> {
    let id = &s.identity;
    let (_, mut revision) = read_item_at_sensitivity(root, &id.source.item_id, ceiling)?
        .context("library_resource_not_found")?;
    anyhow::ensure!(
        revision.revision_id == id.source.revision_id,
        "stale_item_revision"
    );
    loop {
        let (doc, bodies) = document_revision_locked(root, &revision, ceiling)?;
        let old = match tr::source_from(
            &doc,
            &bodies,
            &id.source.item_id,
            &revision.revision_id,
            &id.source.original_chapter,
        ) {
            Some(s) => s?,
            None => return Ok(None),
        };
        if old.markdown != s.source_markdown {
            return Ok(None);
        }
        // Never skip an intermediate projection, even when its body happens to match.
        if let Some(p) = tr::stored(root, &old.identity, &old.markdown)? {
            if p.metadata()?.version != id.projection_sha256 {
                return Ok(None);
            }
        }
        let mut candidate = id.clone();
        candidate.source = old.identity;
        candidate.translation_version = metadata_view::version(
            "chapter_translation",
            &id.source.item_id,
            &revision.revision_id,
            &id.projection_identity.revision_id,
            &id.projection_sha256,
        )?;
        if let Some(a) = stored(root, &candidate, s)? {
            let hash = reader_sha256(&serde_json::to_vec(&a)?);
            let version = metadata_view::version(
                "chapter_translation_map",
                &id.source.item_id,
                &id.source.revision_id,
                &revision.revision_id,
                &hash,
            )?;
            return Ok(Some((a, version)));
        }
        // No map could predate the projection it describes.
        if revision.revision_id == id.projection_identity.revision_id {
            return Ok(None);
        }
        let Some(parent) = metadata_view::chapter_parent(root, &revision, ceiling)? else {
            return Ok(None);
        };
        revision = parent;
    }
}

pub fn read(
    library: &Path,
    item: &str,
    revision: &str,
    chapter: &str,
    translation_version: &str,
    expected_version: &str,
    offset: usize,
    limit: usize,
    ceiling: Sensitivity,
) -> Result<Page> {
    anyhow::ensure!((1..=256).contains(&limit), "invalid_reader_limit");
    anyhow::ensure!(
        expected_version != "any" || offset == 0,
        "stale_chapter_alignment"
    );
    with_consistent_snapshot(library, |root| {
        let s = source_locked(root, item, revision, chapter, translation_version, ceiling)?;
        let found = effective(root, &s, ceiling)?;
        let version = found.as_ref().map_or("missing", |(_, v)| v.as_str());
        anyhow::ensure!(
            expected_version == "any" || expected_version == version,
            "stale_chapter_alignment"
        );
        let pairs = found.as_ref().map_or(&[][..], |(a, _)| a.pairs.as_slice());
        anyhow::ensure!(offset <= pairs.len(), "invalid_reader_offset");
        let end = offset.saturating_add(limit).min(pairs.len());
        let status = if found.is_none() {
            "missing"
        } else if !pairs
            .iter()
            .any(|p| p.relation == paragraphs::Relation::Matched)
        {
            "none"
        } else if pairs
            .iter()
            .any(|p| p.relation == paragraphs::Relation::Unresolved)
        {
            "partial"
        } else {
            "aligned"
        };
        Ok(Page {
            translation_version: translation_version.into(),
            certified: false,
            alignment: tr::AlignmentPage {
                item_id: item.into(),
                revision_id: revision.into(),
                version: version.into(),
                original_chapter: chapter.into(),
                translation_chapter: None,
                original_sha256: s.identity.source.source_sha256,
                translation_sha256: s.identity.target_sha256,
                original_bytes: s.identity.source.source_bytes,
                translation_bytes: s.identity.target_bytes,
                status: status.into(),
                quality: alignment::Quality::NotCertified,
                basis: "retrospective_model_alignment_v1".into(),
                offset,
                total: pairs.len(),
                next_offset: (end < pairs.len()).then_some(end),
                pairs: pairs[offset..end].to_vec(),
            },
        })
    })
}

fn artifact_receipt(a: &Artifact) -> Result<Receipt> {
    Ok(Receipt {
        job_id: a.job_id.clone(),
        identity: a.identity.clone(),
        map_version: reader_sha256(&serde_json::to_vec(a)?),
    })
}

/// Map-only receipt lives in the same atomic artifact, eliminating a two-file crash gap.
pub fn receipt(
    library: &Path,
    identity: &Identity,
    job_id: &str,
    ceiling: Sensitivity,
) -> Result<Option<Receipt>> {
    with_consistent_snapshot(library, |root| {
        let id = &identity.source;
        anyhow::ensure!(
            valid_item_id(&id.item_id) && valid_revision_id(&id.revision_id),
            "invalid_reader_request"
        );
        // Recovery must still work after the current revision changes. Reach the
        // publication revision only through the fully validated current chain.
        let (_, mut revision) = read_item_at_sensitivity(root, &id.item_id, ceiling)?
            .context("library_resource_not_found")?;
        let (_, legacy) = read_legacy_item(root, &id.item_id)?;
        while revision.revision_id != id.revision_id {
            anyhow::ensure!(
                revision.revision_id != legacy.revision_id,
                "stale_item_revision"
            );
            let parent = revision
                .provenance
                .last()
                .and_then(|a| a.input_ids.first())
                .filter(|v| valid_revision_id(v))
                .context("stale_item_revision")?;
            revision = load(
                root,
                root.join("items")
                    .join(&id.item_id)
                    .join("revisions")
                    .join(format!("{parent}.json")),
            )?;
        }
        anyhow::ensure!(
            revision.item_id == id.item_id && revision.sensitivity <= ceiling,
            "library_resource_not_found"
        );
        let (doc, bodies) = document_revision_locked(root, &revision, ceiling)?;
        let original = tr::source_from(
            &doc,
            &bodies,
            &id.item_id,
            &id.revision_id,
            &id.original_chapter,
        )
        .context("reader_translation_unavailable")??;
        anyhow::ensure!(original.identity == *id, "stale_reader_translation");
        loop {
            let (doc, bodies) = document_revision_locked(root, &revision, ceiling)?;
            let candidate = tr::source_from(
                &doc,
                &bodies,
                &id.item_id,
                &revision.revision_id,
                &id.original_chapter,
            )
            .context("reader_translation_unavailable")??;
            anyhow::ensure!(
                candidate.markdown == original.markdown,
                "stale_reader_translation"
            );
            if let Some(p) = tr::stored(root, &candidate.identity, &candidate.markdown)? {
                let m = p.metadata()?;
                anyhow::ensure!(
                    p.identity == identity.projection_identity
                        && m.version == identity.projection_sha256
                        && m.sha256 == identity.target_sha256
                        && m.bytes == identity.target_bytes
                        && metadata_view::version(
                            "chapter_translation",
                            &id.item_id,
                            &id.revision_id,
                            &p.identity.revision_id,
                            &m.version
                        )? == identity.translation_version,
                    "stale_reader_translation"
                );
                let s = Source {
                    identity: identity.clone(),
                    source_markdown: original.markdown,
                    target_markdown: p.markdown,
                    recorded_map: p.blocks.is_some(),
                };
                return stored(root, identity, &s)?
                    .filter(|a| a.job_id == job_id)
                    .map(|a| artifact_receipt(&a))
                    .transpose();
            }
            revision = metadata_view::chapter_parent(root, &revision, ceiling)?
                .context("reader_translation_unavailable")?;
        }
    })
}

/// CAS is always missing. A different job or payload can never replace an installed map.
pub fn publish(
    library: &Path,
    identity: Identity,
    provenance: Provenance,
    pairs: Vec<paragraphs::Pair>,
    job_id: &str,
    ceiling: Sensitivity,
) -> Result<Receipt> {
    publish_with_gate(library, identity, provenance, pairs, job_id, ceiling, |publish| publish())
}

/// Internal authority integration seam, not an authorization policy. The caller
/// must check authority and hold its fence while invoking the one-shot action.
/// Store holds its writer lock before entering the gate; never acquire Store
/// locks from the gate. Exact receipt replay does not enter the gate.
pub fn publish_with_gate<G>(
    library: &Path,
    identity: Identity,
    provenance: Provenance,
    pairs: Vec<paragraphs::Pair>,
    job_id: &str,
    ceiling: Sensitivity,
    gate: G,
) -> Result<Receipt>
where
    G: FnOnce(Box<dyn FnOnce() -> Result<()> + '_>) -> Result<()>,
{
    let root = existing_canonical_root(library)?;
    let lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    let id = &identity.source;
    let s = source_locked(
        &root,
        &id.item_id,
        &id.revision_id,
        &id.original_chapter,
        &identity.translation_version,
        ceiling,
    )?;
    anyhow::ensure!(s.identity == identity, "stale_reader_translation");
    anyhow::ensure!(!s.recorded_map, "stale_chapter_alignment");
    let a = Artifact {
        schema_version: 1,
        identity,
        provenance,
        job_id: job_id.into(),
        pairs,
    };
    validate(&a, &s.identity, &s)?;
    let bytes = serde_json::to_vec(&a)?;
    anyhow::ensure!(
        bytes.len() <= 8 * 1024 * 1024,
        "invalid_reader_alignment_request"
    );
    if let Some((existing, _)) = effective(&root, &s, ceiling)? {
        anyhow::ensure!(
            serde_json::to_vec(&existing)? == bytes,
            "stale_chapter_alignment"
        );
        crate::sync_dir(path(&root, &existing.identity.source).parent().unwrap())?;
        return artifact_receipt(&existing);
    }
    let file = path(&root, &a.identity.source);
    ensure_contained_dir(&root, file.parent().unwrap().strip_prefix(&root)?)?;
    install_binary_with_gate(&root, &file, &bytes, gate)?;
    lock.unlock()?;
    artifact_receipt(&a)
}

#[cfg(test)]
mod tests;
