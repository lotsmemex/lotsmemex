//! Derived, revision-bound navigation indexes. Never edits either edition or Evidence.
use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BookAlignment {
    pub schema_version: u32,
    pub item_id: String,
    pub revision_id: String,
    pub chapters: Vec<ChapterAlignment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChapterAlignment {
    pub schema_version: u32,
    pub identity: Identity,
    pub alignment: AlignmentKind,
    pub anchors: Vec<NavigationAnchor>,
    pub quality: Quality,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlignmentKind {
    SparseNavigationNotCitations,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    NotCertified,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnchorBasis {
    ModelSampleNotFullSpanVerification,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub original_sha256: String,
    pub translation_sha256: String,
    pub binding: Binding,
    pub sampler_version: u32,
    pub sample_count: u8,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub item_id: String,
    pub revision_id: String,
    pub canonical_sha256: String,
    pub original: SideBinding,
    pub translation: SideBinding,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SideBinding {
    pub chapter: String,
    pub chapter_start: usize,
    pub sha256: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NavigationAnchor {
    pub original: [usize; 2],
    pub translation: [usize; 2],
    pub basis: AnchorBasis,
}

impl BookAlignment {
    pub fn version(&self) -> Result<String> {
        Ok(reader_sha256(&serde_json::to_vec(self)?))
    }
}

fn path(root: &Path, item: &str, revision: &str) -> PathBuf {
    root.join("catalog/book-alignments")
        .join(item)
        .join(format!("{revision}.json"))
}

fn stored(root: &Path, item: &str, revision: &str) -> Result<Option<BookAlignment>> {
    let file = path(root, item, revision);
    match fs::symlink_metadata(&file) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
        Ok(m) => {
            if m.len() > 4 * 1024 * 1024 || m.file_type().is_symlink() || !m.is_file() {
                anyhow::bail!("invalid_book_alignment")
            }
            Ok(Some(load(root, file)?))
        }
    }
}

/// The stored alignment of `revision`, else that of its newest same-body revision (only
/// metadata such as the title edited since) rebound to `revision`; validated by the caller
/// against `revision` like an own one.
fn effective_stored(root: &Path, item: &str, revision: &str) -> Result<Option<BookAlignment>> {
    if let Some(own) = stored(root, item, revision)? {
        return Ok(Some(own));
    }
    for source in crate::body::same_body_revisions(root, item, revision)? {
        if let Some(mut found) = stored(root, item, &source.revision_id)? {
            if found.item_id != item || found.revision_id != source.revision_id {
                anyhow::bail!("invalid_book_alignment")
            }
            found.revision_id = revision.into();
            for chapter in &mut found.chapters {
                chapter.identity.binding.revision_id = revision.into();
            }
            return Ok(Some(found));
        }
    }
    Ok(None)
}

fn validate(root: &Path, next: &BookAlignment, ceiling: Sensitivity) -> Result<()> {
    let (doc, bodies) = document_locked(root, &next.item_id, &next.revision_id, ceiling)?;
    if next.schema_version != 1 || doc.editions.len() != 2 || next.chapters.len() > 4096 {
        anyhow::bail!("invalid_book_alignment")
    }
    let original = &doc.editions[0];
    let translation = &doc.editions[1];
    let paired: Vec<_> = original
        .chapters
        .iter()
        .filter(|c| c.counterpart.is_some())
        .collect();
    if paired.is_empty() || next.chapters.len() != paired.len() {
        anyhow::bail!("incomplete_book_alignment")
    }
    for (index, left) in next.chapters.iter().zip(paired) {
        let right = translation
            .chapters
            .iter()
            .find(|c| Some(&c.key) == left.counterpart.as_ref())
            .context("invalid_book_alignment")?;
        let id = &index.identity;
        let binding = &id.binding;
        if index.schema_version != 1
            || id.sampler_version != 1
            || !(1..=7).contains(&id.sample_count)
            || index.anchors.len() > id.sample_count as usize
            || binding.item_id != next.item_id
            || binding.revision_id != next.revision_id
            || binding.canonical_sha256 != doc.canonical_sha256
            || binding.original
                != (SideBinding {
                    chapter: left.key.clone(),
                    chapter_start: left.start,
                    sha256: original.sha256.clone(),
                })
            || binding.translation
                != (SideBinding {
                    chapter: right.key.clone(),
                    chapter_start: right.start,
                    sha256: translation.sha256.clone(),
                })
        {
            anyhow::bail!("invalid_book_alignment")
        }
        for (side, chapter, expected_hash) in [
            ("original", left, &id.original_sha256),
            ("translation", right, &id.translation_sha256),
        ] {
            let body = &bodies[side].0[chapter.start..chapter.end];
            if reader_sha256(body.as_bytes()) != *expected_hash {
                anyhow::bail!("invalid_book_alignment")
            }
            let mut end = 0;
            for anchor in &index.anchors {
                let range = if side == "original" {
                    anchor.original
                } else {
                    anchor.translation
                };
                if range[0] < end || range[0] >= range[1] || body.get(range[0]..range[1]).is_none()
                {
                    anyhow::bail!("invalid_book_alignment")
                }
                end = range[1];
            }
        }
    }
    Ok(())
}

pub fn read(
    library: &Path,
    item: &str,
    revision: &str,
    ceiling: Sensitivity,
) -> Result<Option<BookAlignment>> {
    with_consistent_snapshot(library, |root| {
        document_locked(root, item, revision, ceiling)?;
        let value = effective_stored(root, item, revision)?;
        if let Some(ref next) = value {
            if next.item_id != item || next.revision_id != revision {
                anyhow::bail!("invalid_book_alignment")
            }
            validate(root, next, ceiling)?;
        }
        Ok(value)
    })
}

/// Single-file atomic commit under the same writer lock as canonical edits.
/// Identical retries are harmless; changed results require the previous version.
pub fn save(
    library: &Path,
    expected: &str,
    next: BookAlignment,
    ceiling: Sensitivity,
) -> Result<BookAlignment> {
    if serde_json::to_vec(&next)?.len() > 4 * 1024 * 1024 {
        anyhow::bail!("invalid_book_alignment")
    }
    let root = existing_canonical_root(library)?;
    let lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    validate(&root, &next, ceiling)?;
    let previous = effective_stored(&root, &next.item_id, &next.revision_id)?;
    if previous.as_ref() == Some(&next) {
        return Ok(next);
    }
    let version = previous
        .as_ref()
        .map(BookAlignment::version)
        .transpose()?
        .unwrap_or_else(|| "missing".into());
    if version != expected {
        anyhow::bail!("stale_book_alignment")
    }
    let relative = Path::new("catalog/book-alignments").join(&next.item_id);
    ensure_contained_dir(&root, &relative)?;
    durable_replace(
        &root,
        &path(&root, &next.item_id, &next.revision_id),
        &serde_json::to_vec(&next)?,
    )?;
    lock.unlock()?;
    Ok(next)
}
