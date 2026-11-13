//! Complete chapter partitions, deliberately separate from sparse navigation hints.
use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Paragraph,
    Heading,
    Note,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    Matched,
    OriginalOnly,
    TranslationOnly,
    Unresolved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pair {
    pub o: Option<[usize; 2]>,
    pub t: Option<[usize; 2]>,
    pub kind: Kind,
    pub relation: Relation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Side {
    pub chapter: String,
    pub chapter_start: usize,
    pub edition_sha256: String,
    pub sha256: String,
    pub bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub item_id: String,
    pub revision_id: String,
    pub canonical_sha256: String,
    pub original: Side,
    pub translation: Option<Side>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Index {
    pub schema_version: u32,
    pub identity: Identity,
    pub algorithm: String,
    pub input_profile_sha256: String,
    pub pairs: Vec<Pair>,
}

impl Index {
    pub fn version(&self) -> Result<String> {
        Ok(reader_sha256(&serde_json::to_vec(self)?))
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Page {
    pub schema_version: u32,
    pub item_id: String,
    pub revision_id: String,
    pub version: String,
    pub original_chapter: String,
    pub translation_chapter: Option<String>,
    pub original_sha256: String,
    pub translation_sha256: Option<String>,
    pub original_bytes: usize,
    pub translation_bytes: Option<usize>,
    pub status: String,
    pub quality: alignment::Quality,
    pub offset: usize,
    pub total: usize,
    pub next_offset: Option<usize>,
    pub pairs: Vec<Pair>,
}

/// Conservative Markdown blocks. Blank separators belong to the preceding block;
/// an inline note marker is never a note boundary or permission to discard text.
pub fn blocks(body: &str) -> Vec<([usize; 2], Kind)> {
    let mut result: Vec<([usize; 2], Kind)> = Vec::new();
    let mut offset = 0;
    let mut separated = true;
    let mut fence: Option<&str> = None;
    let mut in_note = false;
    for line in body.split_inclusive('\n') {
        let trimmed = line.trim();
        let heading = line.bytes().take_while(|b| *b == b'#').count();
        let note_definition = line.starts_with("[^") && line.contains("]:");
        let note_continuation = in_note && (line.starts_with("    ") || line.starts_with('\t'));
        let kind = if (1..=6).contains(&heading) && line.as_bytes().get(heading) == Some(&b' ') {
            Kind::Heading
        } else if note_definition || note_continuation {
            Kind::Note
        } else {
            Kind::Paragraph
        };
        let is_fence = if line.starts_with("```") {
            Some("```")
        } else if line.starts_with("~~~") {
            Some("~~~")
        } else {
            None
        };
        let start = !trimmed.is_empty()
            && fence.is_none()
            && !note_continuation
            && (separated
                || kind != Kind::Paragraph
                || result.last().is_some_and(|b| b.1 == Kind::Heading));
        if start && !result.is_empty() {
            result.push(([offset, offset + line.len()], kind));
        } else if let Some(last) = result.last_mut() {
            last.0[1] = offset + line.len();
        } else {
            result.push(([0, offset + line.len()], kind));
        }
        if let Some(marker) = is_fence {
            if fence == Some(marker) {
                fence = None;
            } else if fence.is_none() {
                fence = Some(marker);
            }
        }
        if !trimmed.is_empty() {
            // Lazy continuation is attached to the existing block above. Its
            // local line classification must not terminate that note's scope.
            in_note = result.last().is_some_and(|block| block.1 == Kind::Note);
        }
        separated = trimmed.is_empty() && fence.is_none();
        offset += line.len();
    }
    result
}

fn identity_locked(
    root: &Path,
    item: &str,
    revision: &str,
    chapter: &str,
    ceiling: Sensitivity,
) -> Result<(Identity, String, Option<String>)> {
    let (doc, bodies) = document_locked(root, item, revision, ceiling)?;
    bind(&doc, &bodies, chapter)
}

pub(super) fn bind(
    doc: &ReaderDocument,
    bodies: &BTreeMap<String, (String, Option<String>)>,
    chapter: &str,
) -> Result<(Identity, String, Option<String>)> {
    let original = doc
        .editions
        .iter()
        .find(|e| e.side == "original")
        .context("reader_side_not_found")?;
    let left = original
        .chapters
        .iter()
        .find(|c| c.key == chapter)
        .context("reader_chapter_not_found")?;
    let side = |edition: &Edition, ch: &Chapter| {
        let body = bodies[&edition.side].0[ch.start..ch.end].to_owned();
        (
            Side {
                chapter: ch.key.clone(),
                chapter_start: ch.start,
                edition_sha256: edition.sha256.clone(),
                sha256: reader_sha256(body.as_bytes()),
                bytes: body.len(),
            },
            body,
        )
    };
    let (o, original_body) = side(original, left);
    let right = left
        .counterpart
        .as_ref()
        .map(|key| -> Result<_> {
            let edition = doc
                .editions
                .iter()
                .find(|e| e.side == "translation")
                .context("reader_side_not_found")?;
            let ch = edition
                .chapters
                .iter()
                .find(|c| &c.key == key)
                .context("reader_chapter_not_found")?;
            Ok(side(edition, ch))
        })
        .transpose()?;
    Ok((
        Identity {
            item_id: doc.item_id.clone(),
            revision_id: doc.revision_id.clone(),
            canonical_sha256: doc.canonical_sha256.clone(),
            original: o,
            translation: right.as_ref().map(|r| r.0.clone()),
        },
        original_body,
        right.map(|r| r.1),
    ))
}

pub fn identity(
    library: &Path,
    item: &str,
    revision: &str,
    chapter: &str,
    ceiling: Sensitivity,
) -> Result<Identity> {
    with_consistent_snapshot(library, |root| {
        Ok(identity_locked(root, item, revision, chapter, ceiling)?.0)
    })
}

pub(super) fn validate(
    index: &Index,
    expected: &Identity,
    original: &str,
    translation: Option<&str>,
) -> Result<()> {
    anyhow::ensure!(
        index.schema_version == 1
            && &index.identity == expected
            && index.algorithm == "chapter-paragraphs-v1"
            && valid_lower_hex_64(&index.input_profile_sha256)
            && index.pairs.len() <= 65536,
        "invalid_reader_alignment_request"
    );
    let sides = [blocks(original), blocks(translation.unwrap_or(""))];
    let mut cursors = [0, 0];
    for pair in &index.pairs {
        let presence = (pair.o.is_some(), pair.t.is_some());
        anyhow::ensure!(
            match pair.relation {
                Relation::Matched => presence == (true, true),
                Relation::OriginalOnly => presence == (true, false),
                Relation::TranslationOnly => presence == (false, true),
                Relation::Unresolved => presence == (true, false) || presence == (false, true),
            },
            "invalid_reader_alignment_request"
        );
        for (side, range) in [pair.o, pair.t].into_iter().enumerate() {
            if let Some([start, end]) = range {
                let blocks = &sides[side];
                let cursor = &mut cursors[side];
                anyhow::ensure!(
                    start < end && blocks.get(*cursor).is_some_and(|b| b.0[0] == start),
                    "invalid_reader_alignment_request"
                );
                loop {
                    let block = blocks
                        .get(*cursor)
                        .context("invalid_reader_alignment_request")?;
                    anyhow::ensure!(
                        block.1 == pair.kind && block.0[1] <= end,
                        "invalid_reader_alignment_request"
                    );
                    *cursor += 1;
                    if block.0[1] == end {
                        break;
                    }
                }
            }
        }
    }
    anyhow::ensure!(
        cursors == [sides[0].len(), sides[1].len()],
        "invalid_reader_alignment_request"
    );
    Ok(())
}

pub(super) fn path(root: &Path, id: &Identity) -> PathBuf {
    root.join("catalog/chapter-alignments")
        .join(&id.item_id)
        .join(&id.revision_id)
        .join(format!(
            "{}.json",
            reader_sha256(id.original.chapter.as_bytes())
        ))
}

pub(super) fn stored(root: &Path, id: &Identity) -> Result<Option<Index>> {
    let file = path(root, id);
    match fs::symlink_metadata(&file) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
        Ok(m) => {
            anyhow::ensure!(
                m.is_file() && !m.file_type().is_symlink() && m.len() <= 8 * 1024 * 1024,
                "invalid_reader_alignment_request"
            );
            Ok(Some(load(root, file)?))
        }
    }
}

pub(super) fn effective(root: &Path, id: &Identity, ceiling: Sensitivity) -> Result<(Option<Index>, String)> {
    let (index, version, _) = resolved(root, id, ceiling)?;
    Ok((index, version))
}

/// Immutable artifact identity, separate from its revision-dependent read view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactBinding {
    pub revision_id: String,
    pub sha256: String,
}

pub fn artifact_binding(library: &Path, item: &str, revision: &str, chapter: &str,
    ceiling: Sensitivity) -> Result<Option<ArtifactBinding>> {
    with_consistent_snapshot(library, |root| {
        let (id, _, _) = identity_locked(root, item, revision, chapter, ceiling)?;
        Ok(resolved(root, &id, ceiling)?.2)
    })
}

fn resolved(root: &Path, id: &Identity, ceiling: Sensitivity) -> Result<(Option<Index>, String, Option<ArtifactBinding>)> {
    let (_, mut revision) = read_item_at_sensitivity(root, &id.item_id, ceiling)?.context("library_resource_not_found")?;
    anyhow::ensure!(revision.revision_id == id.revision_id, "stale_item_revision");
    let mut current = true;
    loop {
        let (doc, bodies) = document_revision_locked(root, &revision, ceiling)?;
        if !current && !binds(&doc, id) {
            return Ok((None, "missing".into(), None));
        }
        let (candidate, o, t) = bind(&doc, &bodies, &id.original.chapter)?;
        if current {
            let mut rebound = candidate.clone();
            rebound.revision_id = id.revision_id.clone();
            anyhow::ensure!(rebound == *id, "stale_chapter_alignment");
        } else if !same_chapters(&candidate, id) {
            // Either side of this chapter differs here: older indexes describe other text.
            return Ok((None, "missing".into(), None));
        }
        current = false;
        if let Some(mut index) = stored(root, &candidate)? {
            validate(&index, &candidate, &o, t.as_deref())?;
            let binding = ArtifactBinding { revision_id: candidate.revision_id.clone(), sha256: index.version()? };
            let version = metadata_view::version("paragraph_alignment", &id.item_id, &id.revision_id, &candidate.revision_id, &index.version()?)?;
            index.identity = id.clone();
            validate(&index, id, &o, t.as_deref())?;
            return Ok((Some(index), version, Some(binding)));
        }
        let Some(parent) = metadata_view::chapter_parent(root, &revision, ceiling)? else {
            return Ok((None, "missing".into(), None));
        };
        revision = parent;
    }
}

/// An older revision still has this chapter (and its counterpart) under the same keys.
fn binds(doc: &ReaderDocument, id: &Identity) -> bool {
    let has = |side: &str, key: &str| doc.editions.iter().any(|e| e.side == side && e.chapters.iter().any(|c| c.key == key));
    let Some(left) = doc.editions.iter().find(|e| e.side == "original").and_then(|e| e.chapters.iter().find(|c| c.key == id.original.chapter)) else { return false };
    left.counterpart.as_deref() == id.translation.as_ref().map(|t| t.chapter.as_str())
        && id.translation.as_ref().is_none_or(|t| has("translation", &t.chapter))
}

/// Same item and byte-identical chapter text on both sides (positions and the
/// rest of each edition may differ).
fn same_chapters(a: &Identity, b: &Identity) -> bool {
    let side = |x: &Side, y: &Side| x.chapter == y.chapter && x.sha256 == y.sha256 && x.bytes == y.bytes;
    a.item_id == b.item_id
        && side(&a.original, &b.original)
        && match (&a.translation, &b.translation) {
            (None, None) => true,
            (Some(x), Some(y)) => side(x, y),
            _ => false,
        }
}

pub fn save(
    library: &Path,
    expected_version: &str,
    next: Index,
    ceiling: Sensitivity,
) -> Result<Index> {
    anyhow::ensure!(
        serde_json::to_vec(&next)?.len() <= 8 * 1024 * 1024,
        "invalid_reader_alignment_request"
    );
    let root = existing_canonical_root(library)?;
    let lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    let (id, o, t) = identity_locked(
        &root,
        &next.identity.item_id,
        &next.identity.revision_id,
        &next.identity.original.chapter,
        ceiling,
    )?;
    validate(&next, &id, &o, t.as_deref())?;
    let (previous, version) = effective(&root, &id, ceiling)?;
    if previous.as_ref() == Some(&next) && stored(&root, &id)?.as_ref() == Some(&next) {
        return Ok(next);
    }
    anyhow::ensure!(version == expected_version, "stale_chapter_alignment");
    let file = path(&root, &id);
    ensure_contained_dir(&root, file.parent().unwrap().strip_prefix(&root)?)?;
    durable_replace(&root, &file, &serde_json::to_vec(&next)?)?;
    lock.unlock()?;
    Ok(next)
}

pub fn read(
    library: &Path,
    item: &str,
    revision: &str,
    chapter: &str,
    offset: usize,
    limit: usize,
    expected_version: &str,
    ceiling: Sensitivity,
) -> Result<Page> {
    anyhow::ensure!(
        (1..=256).contains(&limit)
            && offset as u128 <= 9_007_199_254_740_991
            && (expected_version == "missing"
                || valid_lower_hex_64(expected_version)
                || (expected_version == "any" && offset == 0)),
        "invalid_reader_alignment_request"
    );
    with_consistent_snapshot(library, |root| {
        let (id, o, t) = identity_locked(root, item, revision, chapter, ceiling)?;
        let (index, version) = effective(root, &id, ceiling)?;
        if let Some(index) = &index {
            validate(index, &id, &o, t.as_deref())?;
        }
        anyhow::ensure!(
            expected_version == "any" || expected_version == version,
            "stale_chapter_alignment"
        );
        let pairs = index.map(|i| i.pairs).unwrap_or_default();
        anyhow::ensure!(offset <= pairs.len(), "invalid_reader_alignment_request");
        let matched = pairs
            .iter()
            .any(|p| p.relation == Relation::Matched && p.kind == Kind::Paragraph);
        let status = if !matched {
            "none"
        } else if pairs.iter().any(|p| p.relation == Relation::Unresolved) {
            "partial"
        } else {
            "aligned"
        };
        let end = offset.saturating_add(limit).min(pairs.len());
        Ok(Page {
            schema_version: 1,
            item_id: item.into(),
            revision_id: revision.into(),
            version,
            original_chapter: id.original.chapter,
            translation_chapter: id.translation.as_ref().map(|s| s.chapter.clone()),
            original_sha256: id.original.sha256,
            translation_sha256: id.translation.as_ref().map(|s| s.sha256.clone()),
            original_bytes: id.original.bytes,
            translation_bytes: id.translation.as_ref().map(|s| s.bytes),
            status: status.into(),
            quality: alignment::Quality::NotCertified,
            offset,
            total: pairs.len(),
            next_offset: (end < pairs.len()).then_some(end),
            pairs: pairs[offset..end].to_vec(),
        })
    })
}
