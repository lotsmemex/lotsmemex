//! Immutable machine translations, separate from human editions and search.
//! The worker validates complete batch coverage before publishing; this layer
//! enforces current source identity, access, immutable provenance and publication.
use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub item_id: String,
    pub revision_id: String,
    pub original_chapter: String,
    pub source_sha256: String,
    pub source_bytes: usize,
    pub source_evidence_id: String,
}

// No Debug/Serialize: the source must not become diagnostic metadata.
pub struct Source {
    pub identity: Identity,
    pub markdown: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    pub provider_id: String,
    pub model_id: String,
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    pub route: Route,
    pub prompt_version: String,
    pub prompt_sha256: String,
    pub plan_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metadata {
    pub version: String,
    pub item_id: String,
    pub revision_id: String,
    pub original_chapter: String,
    pub source_sha256: String,
    pub target_language: String,
    pub sha256: String,
    pub bytes: usize,
    /// `machine_translation`, or `owner_edit` for a human-edited AI translation.
    pub provenance: String,
    /// Model route of the machine origin (an owner edit keeps its generation chain).
    pub route: Route,
    /// Present only for `provenance:"owner_edit"`; absent on every AI projection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_edit: Option<edit::OwnerEdit>,
}

#[derive(Serialize)]
pub struct Chunk {
    #[serde(flatten)]
    pub metadata: Metadata,
    pub start: usize,
    pub end: usize,
    pub markdown: String,
    pub next_offset: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranslationBlock {
    pub id: String,
    pub source_range: [usize; 2],
    pub target_range: [usize; 2],
    pub kind: paragraphs::Kind,
}

#[derive(Debug, Serialize)]
pub struct AlignmentPage {
    pub item_id: String,
    pub revision_id: String,
    pub version: String,
    pub original_chapter: String,
    pub translation_chapter: Option<String>,
    pub original_sha256: String,
    pub translation_sha256: String,
    pub original_bytes: usize,
    pub translation_bytes: usize,
    pub status: String,
    pub quality: alignment::Quality,
    pub basis: String,
    pub offset: usize,
    pub total: usize,
    pub next_offset: Option<usize>,
    pub pairs: Vec<paragraphs::Pair>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Projection {
    schema_version: u32,
    pub(super) identity: Identity,
    provenance: Provenance,
    sha256: String,
    pub(super) markdown: String,
    // Omission is essential: schema-1 versions hash the exact legacy field set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) blocks: Option<Vec<TranslationBlock>>,
    /// Resolved owner edit (never part of a stored AI projection or its hash).
    #[serde(skip)]
    pub(super) owner: Option<Box<edit::View>>,
}

impl Projection {
    pub(super) fn metadata(&self) -> Result<Metadata> {
        if let Some(view) = &self.owner {
            return Ok(view.metadata.clone());
        }
        Ok(Metadata {
            version: reader_sha256(&serde_json::to_vec(self)?),
            item_id: self.identity.item_id.clone(),
            revision_id: self.identity.revision_id.clone(),
            original_chapter: self.identity.original_chapter.clone(),
            source_sha256: self.identity.source_sha256.clone(),
            target_language: "zh-CN".into(),
            sha256: reader_sha256(self.markdown.as_bytes()),
            bytes: self.markdown.len(),
            provenance: "machine_translation".into(),
            route: self.provenance.route.clone(),
            owner_edit: None,
        })
    }
    fn validate(&self, identity: &Identity, source: &str) -> Result<()> {
        let p = &self.provenance;
        anyhow::ensure!(
            matches!(
                (self.schema_version, self.blocks.is_some()),
                (1, false) | (2, true)
            ) && self.identity == *identity
                && identity.source_bytes == source.len()
                && identity.source_sha256 == reader_sha256(source.as_bytes())
                && !self.markdown.trim().is_empty()
                && self.markdown.len() <= MAX_TEXT
                && self.sha256 == reader_sha256(self.markdown.as_bytes())
                && !self.markdown.contains('\0')
                && valid_lower_hex_64(&p.prompt_sha256)
                && valid_lower_hex_64(&p.plan_sha256)
                && valid_text(&p.prompt_version, 128)
                && valid_text(&p.route.provider_id, 80)
                && valid_text(&p.route.model_id, 256)
                && p.route.reasoning_effort.as_deref().is_none_or(|e| matches!(
                    e,
                    "none" | "minimal" | "low" | "medium" | "high" | "xhigh"
                )),
            "invalid_reader_translation_request"
        );
        if let Some(blocks) = &self.blocks {
            let planned = paragraphs::blocks(source);
            anyhow::ensure!(
                !blocks.is_empty() && blocks.len() == planned.len() && blocks.len() <= 65536,
                "invalid_reader_translation_request"
            );
            let mut target_end = 0;
            for (i, (block, (range, kind))) in blocks.iter().zip(planned).enumerate() {
                let [start, end] = block.target_range;
                anyhow::ensure!(
                    block.id == format!("block-{i}")
                        && block.source_range == range
                        && block.kind == kind
                        && start == target_end
                        && start < end
                        && end <= self.markdown.len()
                        && self.markdown.is_char_boundary(start)
                        && self.markdown.is_char_boundary(end)
                        && !self.markdown[start..end].trim().is_empty()
                        && (i + 1 == blocks.len() || self.markdown[start..end].ends_with("\n\n")),
                    "invalid_reader_translation_request"
                );
                target_end = end;
            }
            anyhow::ensure!(
                target_end == self.markdown.len(),
                "invalid_reader_translation_request"
            );
        }
        Ok(())
    }
}

pub(super) fn source_locked(
    root: &Path,
    item: &str,
    revision: &str,
    chapter: &str,
    ceiling: Sensitivity,
) -> Result<Source> {
    let (doc, bodies) = document_locked(root, item, revision, ceiling)?;
    source_strict(&doc, &bodies, item, revision, chapter)
}

/// `None` when this revision has no untranslated English chapter `chapter`
/// (an older revision is then simply not a source to inherit from).
pub(super) fn source_from(
    doc: &ReaderDocument,
    bodies: &ReaderBodies,
    item: &str,
    revision: &str,
    chapter: &str,
) -> Option<Result<Source>> {
    let edition = doc.editions.iter().find(|e| e.side == "original")?;
    let ch = edition.chapters.iter().find(|c| c.key == chapter)?;
    let language = edition.language.to_ascii_lowercase();
    if ch.counterpart.is_some() || !(language == "en" || language.starts_with("en-")) {
        return None;
    }
    Some(source_strict(doc, bodies, item, revision, chapter))
}

fn source_strict(
    doc: &ReaderDocument,
    bodies: &ReaderBodies,
    item: &str,
    revision: &str,
    chapter: &str,
) -> Result<Source> {
    let edition = doc
        .editions
        .iter()
        .find(|e| e.side == "original")
        .context("reader_translation_language_unsupported")?;
    let ch = edition
        .chapters
        .iter()
        .find(|c| c.key == chapter)
        .context("reader_chapter_not_found")?;
    anyhow::ensure!(ch.counterpart.is_none(), "reader_translation_not_needed");
    let language = edition.language.to_ascii_lowercase();
    anyhow::ensure!(
        language == "en" || language.starts_with("en-"),
        "reader_translation_language_unsupported"
    );
    let (body, evidence) = &bodies["original"];
    let markdown = body[ch.start..ch.end].to_owned();
    Ok(Source {
        identity: Identity {
            item_id: item.into(),
            revision_id: revision.into(),
            original_chapter: chapter.into(),
            source_sha256: reader_sha256(markdown.as_bytes()),
            source_bytes: markdown.len(),
            source_evidence_id: evidence.clone().context("reader_translation_unavailable")?,
        },
        markdown,
    })
}

pub fn source(
    library: &Path,
    item: &str,
    revision: &str,
    chapter: &str,
    ceiling: Sensitivity,
) -> Result<Source> {
    with_consistent_snapshot(library, |root| {
        source_locked(root, item, revision, chapter, ceiling)
    })
}

fn path(root: &Path, id: &Identity) -> PathBuf {
    root.join("catalog/chapter-translations")
        .join(&id.item_id)
        .join(&id.revision_id)
        .join(format!(
            "{}.json",
            reader_sha256(id.original_chapter.as_bytes())
        ))
}

pub(super) fn stored(root: &Path, id: &Identity, source: &str) -> Result<Option<Projection>> {
    let path = path(root, id);
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
        Ok(m) => {
            anyhow::ensure!(
                m.is_file()
                    && !m.file_type().is_symlink()
                    && m.len() <= 6 * MAX_TEXT as u64 + 16384,
                "invalid_reader_translation_request"
            );
            let p: Projection = load(root, path)?;
            p.validate(id, source)?;
            Ok(Some(p))
        }
    }
}

pub(super) fn effective(
    root: &Path,
    source: &Source,
    ceiling: Sensitivity,
) -> Result<Option<(Projection, Metadata)>> {
    let id = &source.identity;
    let (_, mut revision) = read_item_at_sensitivity(root, &id.item_id, ceiling)?
        .context("library_resource_not_found")?;
    anyhow::ensure!(
        revision.revision_id == id.revision_id,
        "stale_item_revision"
    );
    let mut current = true;
    loop {
        let candidate = if current {
            id.clone()
        } else {
            // The older revision's own source identity (its evidence) must
            // carry byte-identical text for the same untranslated chapter.
            let (doc, bodies) = document_revision_locked(root, &revision, ceiling)?;
            match source_from(&doc, &bodies, &id.item_id, &revision.revision_id, &id.original_chapter) {
                Some(Ok(older)) if older.identity.source_sha256 == id.source_sha256
                    && older.identity.source_bytes == id.source_bytes
                    && older.markdown == source.markdown => older.identity,
                Some(Err(e)) => return Err(e),
                _ => return Ok(None),
            }
        };
        current = false;
        // An owner edit at this revision supersedes the AI projection it replaced.
        let found = match edit::head(root, &candidate, &source.markdown)? {
            Some(p) => Some(p),
            None => stored(root, &candidate, &source.markdown)?,
        };
        if let Some(p) = found {
            let mut metadata = p.metadata()?;
            metadata.version = metadata_view::version(
                "chapter_translation",
                &id.item_id,
                &id.revision_id,
                &candidate.revision_id,
                &metadata.version,
            )?;
            metadata.revision_id = id.revision_id.clone();
            return Ok(Some((p, metadata)));
        }
        let Some(parent) = metadata_view::chapter_parent(root, &revision, ceiling)? else {
            return Ok(None);
        };
        revision = parent;
    }
}

pub fn metadata(
    library: &Path,
    item: &str,
    revision: &str,
    chapter: &str,
    ceiling: Sensitivity,
) -> Result<Option<Metadata>> {
    with_consistent_snapshot(library, |root| {
        let source = source_locked(root, item, revision, chapter, ceiling)?;
        Ok(effective(root, &source, ceiling)?.map(|(_, metadata)| metadata))
    })
}

/// Publish a fully assembled, validated worker result in one atomic immutable file.
/// A lost acknowledgement can replay without a new revision or replacing output.
pub fn publish(
    library: &Path,
    identity: Identity,
    provenance: Provenance,
    markdown: String,
    ceiling: Sensitivity,
) -> Result<Metadata> {
    publish_inner(library, identity, provenance, markdown, None, ceiling)
}

/// Publish recorded planner correspondence atomically with the generated body.
pub fn publish_mapped(
    library: &Path,
    identity: Identity,
    provenance: Provenance,
    markdown: String,
    blocks: Vec<TranslationBlock>,
    ceiling: Sensitivity,
) -> Result<Metadata> {
    publish_inner(
        library,
        identity,
        provenance,
        markdown,
        Some(blocks),
        ceiling,
    )
}

fn publish_inner(
    library: &Path,
    identity: Identity,
    provenance: Provenance,
    markdown: String,
    blocks: Option<Vec<TranslationBlock>>,
    ceiling: Sensitivity,
) -> Result<Metadata> {
    let root = existing_canonical_root(library)?;
    let lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    let current = source_locked(
        &root,
        &identity.item_id,
        &identity.revision_id,
        &identity.original_chapter,
        ceiling,
    )?;
    anyhow::ensure!(current.identity == identity, "stale_reader_translation");
    let p = Projection {
        schema_version: if blocks.is_some() { 2 } else { 1 },
        identity,
        provenance,
        sha256: reader_sha256(markdown.as_bytes()),
        markdown,
        blocks,
        owner: None,
    };
    // A malformed new map is never accepted, even on an immutable replay.
    if p.blocks.is_some() {
        p.validate(&current.identity, &current.markdown)?;
    }
    if let Some(existing) = stored(&root, &p.identity, &current.markdown)? {
        // A prior install may have linked the file but failed directory sync.
        // Replaying publication must finish durability before acknowledging it.
        crate::sync_dir(path(&root, &p.identity).parent().unwrap())?;
        return existing.metadata();
    }
    // A late worker never shadows or replaces an owner-edited translation.
    if let Some((effective, _)) = effective(&root, &current, ceiling)? {
        anyhow::ensure!(effective.owner.is_none(), "stale_reader_translation");
    }
    p.validate(&current.identity, &current.markdown)?;
    let file = path(&root, &p.identity);
    let bytes = serde_json::to_vec(&p)?;
    anyhow::ensure!(
        bytes.len() <= 6 * MAX_TEXT + 16384,
        "invalid_reader_translation_request"
    );
    ensure_contained_dir(&root, file.parent().unwrap().strip_prefix(&root)?)?;
    install_binary(&root, &file, &bytes)?;
    lock.unlock()?;
    p.metadata()
}

pub fn chunk(
    library: &Path,
    item: &str,
    revision: &str,
    chapter: &str,
    expected_version: &str,
    offset: usize,
    limit: usize,
    ceiling: Sensitivity,
) -> Result<Chunk> {
    anyhow::ensure!(
        (4..=READER_CHUNK_BYTES).contains(&limit),
        "invalid_reader_limit"
    );
    with_consistent_snapshot(library, |root| {
        let source = source_locked(root, item, revision, chapter, ceiling)?;
        let (p, metadata) =
            effective(root, &source, ceiling)?.context("reader_translation_unavailable")?;
        anyhow::ensure!(
            metadata.version == expected_version,
            "stale_reader_translation"
        );
        anyhow::ensure!(
            offset <= p.markdown.len() && p.markdown.is_char_boundary(offset),
            "invalid_reader_offset"
        );
        let mut end = offset.saturating_add(limit).min(p.markdown.len());
        while !p.markdown.is_char_boundary(end) {
            end -= 1;
        }
        Ok(Chunk {
            metadata,
            start: offset,
            end,
            markdown: p.markdown[offset..end].into(),
            next_offset: (end < p.markdown.len()).then_some(end),
        })
    })
}

pub fn alignment(
    library: &Path,
    item: &str,
    revision: &str,
    chapter: &str,
    expected_version: &str,
    offset: usize,
    limit: usize,
    ceiling: Sensitivity,
) -> Result<AlignmentPage> {
    anyhow::ensure!((1..=256).contains(&limit), "invalid_reader_limit");
    with_consistent_snapshot(library, |root| {
        let source = source_locked(root, item, revision, chapter, ceiling)?;
        let (p, metadata) =
            effective(root, &source, ceiling)?.context("reader_translation_unavailable")?;
        anyhow::ensure!(
            metadata.version == expected_version,
            "stale_reader_translation"
        );
        // An owner edit keeps only correspondence rows proven byte-identical;
        // others are `unresolved` with t:null (comparison/jump unavailable).
        let edited = p.owner.as_deref().and_then(edit::pairs);
        let total = match (&p.owner, &edited) {
            (Some(_), rows) => rows.as_ref().map_or(0, Vec::len),
            (None, _) => p.blocks.as_ref().map_or(0, Vec::len),
        };
        anyhow::ensure!(offset <= total, "invalid_reader_offset");
        let end = offset.saturating_add(limit).min(total);
        let pairs = if p.owner.is_some() {
            edited.map(|rows| rows[offset..end].to_vec()).unwrap_or_default()
        } else { p
            .blocks
            .as_ref()
            .map(|blocks| {
                blocks[offset..end]
                    .iter()
                    .map(|b| paragraphs::Pair {
                        o: Some(b.source_range),
                        t: Some(b.target_range),
                        kind: b.kind,
                        relation: paragraphs::Relation::Matched,
                    })
                    .collect()
            })
            .unwrap_or_default() };
        let status = match p.owner.as_deref() {
            Some(view) => edit::status(view),
            None if p.blocks.is_some() => "aligned",
            None => "missing",
        };
        Ok(AlignmentPage {
            item_id: metadata.item_id,
            revision_id: metadata.revision_id,
            version: metadata.version,
            original_chapter: metadata.original_chapter,
            translation_chapter: None,
            original_sha256: metadata.source_sha256,
            translation_sha256: metadata.sha256,
            original_bytes: source.identity.source_bytes,
            translation_bytes: metadata.bytes,
            status: status.into(),
            quality: alignment::Quality::NotCertified,
            basis: "translation_blocks".into(),
            offset,
            total,
            next_offset: (end < total).then_some(end),
            pairs,
        })
    })
}

pub mod edit;
pub mod speakers;

#[cfg(test)]
mod tests;
