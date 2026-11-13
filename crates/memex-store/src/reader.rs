//! Bounded, revision-bound long-document reads and owner reading state.
//! Parallel editions remain independent evidence streams, never original-text coordinates.
use super::*;
use serde::{Deserialize, Serialize};

pub mod alignment;
pub mod annotations;
pub mod annotations_ai;
pub mod annotations_ai_document;
pub mod chapter_edit;
pub mod chapter_translation;
pub mod chapter_translation_map;
pub mod content_format;
mod metadata_view;
pub mod paragraphs;
pub mod passes;
pub mod repair;
pub mod revision_text;

pub const READER_CHUNK_BYTES: usize = 32 * 1024;
pub const BOOK_MANIFEST: &str = "application/vnd.memex.parallel-book+json";
const MAX_TEXT: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chapter {
    pub key: String,
    pub title: String,
    pub level: u8,
    pub start: usize,
    pub end: usize,
    /// Only explicit chapter numbering/order; not sentence or paragraph alignment.
    pub counterpart: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edition {
    pub side: String,
    pub language: String,
    pub sha256: String,
    pub bytes: usize,
    pub chapters: Vec<Chapter>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BookManifest {
    pub schema_version: u32,
    pub alignment: String,
    pub editions: Vec<Edition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReaderDocument {
    pub item_id: String,
    pub revision_id: String,
    pub canonical_sha256: String,
    pub default_side: String,
    pub alignment: String,
    pub editions: Vec<Edition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReaderChunk {
    pub item_id: String,
    pub revision_id: String,
    pub side: String,
    pub sha256: String,
    pub evidence_id: Option<String>,
    pub chapter: String,
    pub start: usize,
    pub end: usize,
    pub next_offset: Option<usize>,
    pub markdown: String,
}

pub fn reader_sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn validate_edition(edition: &Edition, body: &str) -> Result<()> {
    if body.is_empty()
        || body.len() > MAX_TEXT
        || body.contains('\0')
        || edition.sha256 != reader_sha256(body.as_bytes())
        || edition.bytes != body.len()
        || !valid_text(&edition.language, 64)
        || !valid_text(&edition.side, 32)
        || edition.chapters.is_empty()
        || edition.chapters.len() > 4096
    {
        anyhow::bail!("invalid_reader_document")
    }
    let mut offset = 0;
    let mut keys = BTreeSet::new();
    for ch in &edition.chapters {
        if ch.start != offset
            || ch.end <= ch.start
            || body.get(ch.start..ch.end).is_none()
            || !valid_text(&ch.key, 128)
            || !valid_text(&ch.title, 512)
            || !(1..=6).contains(&ch.level)
            || !keys.insert(&ch.key)
            || ch.counterpart.as_ref().is_some_and(|s| !valid_text(s, 128))
        {
            anyhow::bail!("invalid_reader_chapters")
        }
        offset = ch.end;
    }
    if offset != body.len() {
        anyhow::bail!("incomplete_reader_document")
    }
    Ok(())
}

/// All content is installed before the ingest journal can make it reachable.
/// Interrupted preparation leaves only immutable, unreferenced blobs; retry verifies them.
pub fn ingest_reader_book(
    library: &Path,
    mut capture: VerifiedCapture,
    draft: ItemDraft,
) -> Result<memex_core::IngestReceipt> {
    let manifest_blob = capture
        .blobs
        .iter()
        .find(|b| b.descriptor.media_type == BOOK_MANIFEST)
        .context("missing_book_manifest")?;
    let manifest: BookManifest = serde_json::from_slice(&manifest_blob.bytes)?;
    validate_book(&manifest, &draft.content_markdown, |sha| {
        capture
            .blobs
            .iter()
            .find(|b| b.descriptor.sha256 == sha && b.descriptor.media_type == "text/markdown")
            .map(|b| b.bytes.clone())
            .context("missing_book_edition")
    })?;
    if capture.blobs.len() > 16
        || capture.blobs.iter().map(|b| b.bytes.len()).sum::<usize>() > 256 * 1024 * 1024
    {
        anyhow::bail!("book_evidence_limit")
    }
    let root = canonical_root(library)?;
    {
        let lock = ingest_lock(&root)?;
        recover_if_pending_locked(&root)?;
        ensure_contained_dir(&root, Path::new("reader-blobs"))?;
        for blob in &mut capture.blobs {
            let sha = reader_sha256(&blob.bytes);
            if sha != blob.descriptor.sha256
                || blob.blob_id != format!("sha256:{sha}")
                || blob.descriptor.byte_size != blob.bytes.len() as u64
            {
                anyhow::bail!("invalid_book_evidence")
            }
            let relative = format!("reader-blobs/{sha}");
            let path = root.join(&relative);
            match fs::symlink_metadata(&path) {
                Ok(meta) => {
                    if !meta.is_file() || meta.file_type().is_symlink() {
                        anyhow::bail!("unsafe_book_evidence")
                    }
                    verify_binary(&contained_existing(&root, &path)?, &blob.bytes)?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    durable_replace(&root, &path, &blob.bytes)?
                }
                Err(e) => return Err(e.into()),
            }
            blob.source_backend_ref = Some(format!("memex-store:{relative}"));
        }
        lock.unlock()?;
    }
    ingest(root, &capture, draft)
}

fn validate_book(
    manifest: &BookManifest,
    canonical: &str,
    mut bytes: impl FnMut(&str) -> Result<Vec<u8>>,
) -> Result<()> {
    if manifest.schema_version != 1
        || manifest.alignment != "declared_chapter_order_not_paragraph_alignment"
        || manifest.editions.len() != 2
        || manifest.editions[0].side != "original"
        || manifest.editions[1].side != "translation"
    {
        anyhow::bail!("invalid_book_manifest")
    }
    for edition in &manifest.editions {
        let body = String::from_utf8(bytes(&edition.sha256)?)?;
        validate_edition(edition, &body)?;
        if edition.side == "translation" && body != canonical {
            anyhow::bail!("stale_book_manifest")
        }
    }
    for (i, edition) in manifest.editions.iter().enumerate() {
        for chapter in &edition.chapters {
            if let Some(key) = &chapter.counterpart {
                if !manifest.editions[1 - i]
                    .chapters
                    .iter()
                    .any(|c| &c.key == key && c.counterpart.as_ref() == Some(&chapter.key))
                {
                    anyhow::bail!("invalid_book_alignment")
                }
            }
        }
    }
    Ok(())
}

/// The reader-blob evidence for one Markdown edition. The same bytes may also be
/// recorded elsewhere (a reconversion keeps its own `evidence/<id>/content.md`
/// copy); only the reader-blob copy is readable by [`blob`], so pick that one
/// instead of whichever duplicate happens to come first.
fn reader_markdown<'a>(evidence: &'a [Evidence], sha: &str) -> Result<&'a Evidence> {
    let backend_ref = format!("memex-store:reader-blobs/{sha}");
    evidence
        .iter()
        .find(|e| {
            e.backend_ref == backend_ref
                && e.descriptor
                    .as_ref()
                    .is_some_and(|d| d.sha256 == sha && d.media_type == "text/markdown")
        })
        .context("missing_book_edition")
}

fn blob(root: &Path, evidence: &Evidence, limit: usize) -> Result<Vec<u8>> {
    let d = evidence
        .descriptor
        .as_ref()
        .context("missing_book_descriptor")?;
    if !valid_lower_hex_64(&d.sha256)
        || d.byte_size > limit as u64
        || evidence.blob_id != format!("sha256:{}", d.sha256)
        || evidence.backend_ref != format!("memex-store:reader-blobs/{}", d.sha256)
    {
        anyhow::bail!("invalid_book_evidence")
    }
    let path = contained_existing(root, &root.join("reader-blobs").join(&d.sha256))?;
    let mut bytes = Vec::new();
    open_regular_nofollow(&path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() != d.byte_size as usize || reader_sha256(&bytes) != d.sha256 {
        anyhow::bail!("invalid_book_evidence")
    }
    Ok(bytes)
}

fn heading_edition(body: &str) -> Edition {
    let mut chapters: Vec<Chapter> = Vec::new();
    let mut offset = 0;
    let mut fenced = false;
    for line in body.split_inclusive('\n') {
        let s = line.trim_end();
        if s.starts_with("```") || s.starts_with("~~~") {
            fenced = !fenced;
        }
        let level = s.bytes().take_while(|b| *b == b'#').count();
        if !fenced
            && (1..=6).contains(&level)
            && s.as_bytes().get(level) == Some(&b' ')
            && !s[level..].trim().is_empty()
        {
            if chapters.is_empty() && offset > 0 {
                chapters.push(Chapter {
                    key: "section-0".into(),
                    title: "开篇".into(),
                    level: 1,
                    start: 0,
                    end: offset,
                    counterpart: None,
                });
            }
            if let Some(last) = chapters.last_mut() {
                last.end = offset;
            }
            chapters.push(Chapter {
                key: format!("section-{}", chapters.len()),
                title: s[level..].trim().chars().take(128).collect(),
                level: level as u8,
                start: offset,
                end: body.len(),
                counterpart: None,
            });
        }
        offset += line.len();
    }
    if chapters.is_empty() {
        chapters.push(Chapter {
            key: "section-0".into(),
            title: "全文".into(),
            level: 1,
            start: 0,
            end: body.len(),
            counterpart: None,
        });
    }
    Edition {
        side: "source".into(),
        language: "und".into(),
        sha256: reader_sha256(body.as_bytes()),
        bytes: body.len(),
        chapters,
    }
}

fn document_locked(
    root: &Path,
    item: &str,
    revision: &str,
    ceiling: Sensitivity,
) -> Result<(ReaderDocument, BTreeMap<String, (String, Option<String>)>)> {
    if !valid_item_id(item) || !valid_revision_id(revision) {
        anyhow::bail!("invalid_reader_request")
    }
    let (_, current) =
        read_item_at_sensitivity(root, item, ceiling)?.context("library_resource_not_found")?;
    if current.revision_id != revision {
        anyhow::bail!("stale_item_revision")
    }
    document_revision_locked(root, &current, ceiling)
}

type ReaderBodies = BTreeMap<String, (String, Option<String>)>;

/// Projections of revisions already validated in this process. A revision and the
/// content-addressed Evidence it names are immutable, so the projection is a pure
/// function of the key below; recomputing it re-read and re-hashed both editions on
/// every chunk. The ceiling is still enforced on every call against the most
/// sensitive Evidence recorded with the entry. Failures are never cached.
struct CachedProjection {
    key: (PathBuf, String, String),
    reads: VerifiedReads,
    max_sensitivity: Sensitivity,
    bytes: usize,
    doc: ReaderDocument,
    bodies: ReaderBodies,
}
static PROJECTIONS: std::sync::OnceLock<std::sync::Mutex<std::collections::VecDeque<CachedProjection>>> =
    std::sync::OnceLock::new();
const PROJECTION_CACHE_BYTES: usize = 64 * 1024 * 1024;

// Caller validates the immutable revision chain before projecting its evidence.
fn document_revision_locked(
    root: &Path,
    current: &ItemRevision,
    ceiling: Sensitivity,
) -> Result<(ReaderDocument, ReaderBodies)> {
    let mut digest = Sha256::new();
    digest.update(serde_json::to_vec(&current.evidence_ids)?);
    digest.update(serde_json::to_vec(&current.provenance)?);
    digest.update(current.content_markdown.as_bytes());
    let key = (root.to_path_buf(), current.revision_id.clone(), format!("{:x}", digest.finalize()));
    let cache = PROJECTIONS.get_or_init(Default::default);
    {
        let mut cache = cache.lock().map_err(|_| anyhow::anyhow!("library_query_failed"))?;
        if let Some(i) = cache.iter().position(|c| c.key == key) {
            if !cache[i].reads.unchanged() {
                cache.remove(i);
            } else if cache[i].max_sensitivity > ceiling {
                anyhow::bail!("library_resource_not_found")
            } else {
                let hit = cache.remove(i).unwrap();
                let value = (hit.doc.clone(), hit.bodies.clone());
                cache.push_back(hit);
                return Ok(value);
            }
        }
    }
    let recording = VerifyReads::begin(root);
    let (doc, bodies, max_sensitivity) = project_revision(root, current, ceiling)?;
    let reads = recording.finish();
    // Account for retained document/paths as well as body bytes. These estimates
    // include per-entry overhead; the budget is not a process RSS guarantee.
    let bytes = std::mem::size_of::<CachedProjection>() + reads.bytes()
        + key.0.as_os_str().len() + key.1.len() + key.2.len()
        + bodies.iter().map(|(side, (body, evidence))| side.len() + body.len()
            + evidence.as_ref().map_or(0, String::len) + 128).sum::<usize>()
        + doc.item_id.len() + doc.revision_id.len() + doc.canonical_sha256.len()
        + doc.default_side.len() + doc.alignment.len()
        + doc.editions.iter().map(|edition| std::mem::size_of::<Edition>()
            + edition.side.len() + edition.language.len() + edition.sha256.len()
            + edition.chapters.iter().map(|chapter| std::mem::size_of::<Chapter>()
                + chapter.key.len() + chapter.title.len()
                + chapter.counterpart.as_ref().map_or(0, String::len)).sum::<usize>()).sum::<usize>();
    if reads.complete && bytes <= PROJECTION_CACHE_BYTES / 4 {
        let mut cache = cache.lock().map_err(|_| anyhow::anyhow!("library_query_failed"))?;
        while cache.len() >= 16 || cache.iter().map(|c| c.bytes).sum::<usize>() + bytes > PROJECTION_CACHE_BYTES {
            cache.pop_front();
        }
        cache.retain(|c| c.key != key);
        cache.push_back(CachedProjection { key, reads, max_sensitivity, bytes, doc: doc.clone(), bodies: bodies.clone() });
    }
    Ok((doc, bodies))
}

fn project_revision(
    root: &Path,
    current: &ItemRevision,
    ceiling: Sensitivity,
) -> Result<(ReaderDocument, ReaderBodies, Sensitivity)> {
    let mut evidence = Vec::new();
    let mut max_sensitivity = Sensitivity::Public;
    for id in &current.evidence_ids {
        let e: Evidence = load(root, root.join("evidence").join(id).join("evidence.json"))?;
        if e.evidence_id != *id || e.sensitivity > ceiling {
            anyhow::bail!("library_resource_not_found")
        }
        max_sensitivity = max_sensitivity.max(e.sensitivity);
        evidence.push(e);
    }
    let mut bodies = BTreeMap::new();
    // Only a validated chapter mutation may supersede the import's first manifest.
    // Extra imported evidence is not an instruction to reinterpret chapter IDs.
    let repaired = current.provenance.iter().rev().find(|a|
        matches!(a.activity_type.as_str(), repair::ACTIVITY | chapter_edit::ACTIVITY)
        || (matches!(a.activity_type.as_str(), content_format::ACTIVITY | content_format::RESTORE_ACTIVITY) && a.input_ids.len() == 5));
    let manifest_evidence = if let Some(activity) = repaired {
        Some(evidence.iter().find(|e| activity.input_ids.get(3) == Some(&e.evidence_id))
            .filter(|e| e.descriptor.as_ref().is_some_and(|d| d.media_type == BOOK_MANIFEST))
            .context("missing_book_manifest")?)
    } else { evidence.iter().find(|e| {
        e.descriptor
            .as_ref()
            .is_some_and(|d| d.media_type == BOOK_MANIFEST)
    }) };
    let (editions, default_side, alignment) = if let Some(e) = manifest_evidence {
        let mut manifest: BookManifest = serde_json::from_slice(&blob(root, e, 2 * 1024 * 1024)?)?;
        let reconverted = current.provenance.iter().rev().find(|a|
            matches!(a.activity_type.as_str(), reconversion::ACTIVITY | repair::ACTIVITY | chapter_edit::ACTIVITY | content_format::ACTIVITY | content_format::RESTORE_ACTIVITY))
            .filter(|a| a.activity_type == reconversion::ACTIVITY)
            .filter(|a| a.input_checksums.get(2) == Some(&format!("sha256:{}", reader_sha256(current.content_markdown.as_bytes()))));
        let canonical = if reconverted.is_some() {
            let translation = manifest.editions.get(1).context("invalid_book_manifest")?;
            let old = reader_markdown(&evidence, &translation.sha256)?;
            String::from_utf8(blob(root, old, MAX_TEXT)?)?
        } else {
            current.content_markdown.clone()
        };
        validate_book(&manifest, &canonical, |sha| {
            blob(root, reader_markdown(&evidence, sha)?, MAX_TEXT)
        })?;
        if let Some(activity) = reconverted {
            // The immutable import manifest describes the original extraction.
            // A verified PDF reconversion supplies new Chinese evidence; retain
            // chapter identities only when every heading still matches exactly.
            let pdf = evidence.iter().find(|e| activity.input_ids.get(1) == Some(&e.evidence_id))
                .context("missing_book_evidence")?;
            if !pdf.descriptor.as_ref().is_some_and(|d| d.locator == "translation.pdf") {
                anyhow::bail!("stale_book_manifest")
            }
            let revised = refreshed_translation(&manifest.editions[1], &current.content_markdown)?;
            bodies.insert("translation".into(), (current.content_markdown.clone(), activity.input_ids.get(2).cloned()));
            manifest.editions[1] = revised;
        }
        for edition in &manifest.editions {
            if bodies.contains_key(&edition.side) { continue; }
            let e = reader_markdown(&evidence, &edition.sha256)?;
            bodies.insert(
                edition.side.clone(),
                (
                    String::from_utf8(blob(root, e, MAX_TEXT)?)?,
                    Some(e.evidence_id.clone()),
                ),
            );
        }
        (manifest.editions, "translation".into(), manifest.alignment)
    } else {
        let edition = heading_edition(&current.content_markdown);
        validate_edition(&edition, &current.content_markdown)?;
        bodies.insert("source".into(), (current.content_markdown.clone(), None));
        (vec![edition], "source".into(), "none".into())
    };
    Ok((
        ReaderDocument {
            item_id: current.item_id.clone(),
            revision_id: current.revision_id.clone(),
            canonical_sha256: reader_sha256(current.content_markdown.as_bytes()),
            default_side,
            alignment,
            editions,
        },
        bodies,
        max_sensitivity,
    ))
}

fn refreshed_translation(old: &Edition, body: &str) -> Result<Edition> {
    let parsed = heading_edition(body);
    if parsed.chapters.len() != old.chapters.len() || parsed.chapters.iter().zip(&old.chapters)
        .any(|(a, b)| a.title != b.title || a.level != b.level) {
        anyhow::bail!("stale_book_manifest")
    }
    let mut edition = old.clone();
    edition.sha256 = reader_sha256(body.as_bytes());
    edition.bytes = body.len();
    for (chapter, new) in edition.chapters.iter_mut().zip(parsed.chapters) {
        chapter.start = new.start;
        chapter.end = new.end;
    }
    validate_edition(&edition, body)?;
    Ok(edition)
}

/// Correct one book's translation without changing its item, memberships,
/// original edition or immutable import evidence. Existing reconversion journal
/// semantics keep this compatible with previously installed Store readers.
pub fn refresh_book_translation(
    library: &Path, item: &str, expected_revision: &str,
    capture: &VerifiedCapture, draft: &ItemDraft,
) -> Result<ItemMetadataUpdateReceipt> {
    let manifest_blob = capture.blobs.iter().find(|b| b.descriptor.media_type == BOOK_MANIFEST)
        .context("missing_book_manifest")?;
    let manifest: BookManifest = serde_json::from_slice(&manifest_blob.bytes)?;
    validate_book(&manifest, &draft.content_markdown, |sha| capture.blobs.iter()
        .find(|b| b.descriptor.sha256 == sha).map(|b| b.bytes.clone()).context("missing_book_edition"))?;
    let old = reader_document(library, item, expected_revision, Sensitivity::Private)?;
    let (_, revision) = read_item_at_sensitivity(library, item, Sensitivity::Private)?
        .context("library_resource_not_found")?;
    if old.editions.len() != 2 || old.editions[0] != manifest.editions[0]
        || revision.source_url != draft.source_url
        || refreshed_translation(&old.editions[1], &draft.content_markdown)? != manifest.editions[1] {
        anyhow::bail!("book_refresh_mismatch")
    }
    let pdf = capture.blobs.iter().find(|b| b.descriptor.locator == "translation.pdf")
        .context("missing_book_evidence")?;
    reconvert_file_item(library, item, FileReconversion {
        expected_revision_id: expected_revision.into(),
        original_pdf_sha256: reader_sha256(&pdf.bytes),
        markdown: draft.content_markdown.clone(),
    }, Sensitivity::Private)
}

pub fn reader_document(
    library: &Path,
    item: &str,
    revision: &str,
    ceiling: Sensitivity,
) -> Result<ReaderDocument> {
    with_consistent_snapshot(library, |root| {
        Ok(document_locked(root, item, revision, ceiling)?.0)
    })
}

pub fn reader_chunk(
    library: &Path,
    item: &str,
    revision: &str,
    side: &str,
    chapter: &str,
    offset: usize,
    limit: usize,
    ceiling: Sensitivity,
) -> Result<ReaderChunk> {
    if !(4..=READER_CHUNK_BYTES).contains(&limit) {
        anyhow::bail!("invalid_reader_limit")
    }
    with_consistent_snapshot(library, |root| {
        let (doc, bodies) = document_locked(root, item, revision, ceiling)?;
        let edition = doc
            .editions
            .iter()
            .find(|e| e.side == side)
            .context("reader_side_not_found")?;
        let ch = edition
            .chapters
            .iter()
            .find(|c| c.key == chapter)
            .context("reader_chapter_not_found")?;
        let (body, evidence_id) = &bodies[side];
        let start = ch
            .start
            .checked_add(offset)
            .context("invalid_reader_offset")?;
        if start > ch.end || !body.is_char_boundary(start) {
            anyhow::bail!("invalid_reader_offset")
        }
        let mut end = start.saturating_add(limit).min(ch.end);
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        Ok(ReaderChunk {
            item_id: item.into(),
            revision_id: revision.into(),
            side: side.into(),
            sha256: edition.sha256.clone(),
            evidence_id: evidence_id.clone(),
            chapter: chapter.into(),
            start,
            end,
            next_offset: (end < ch.end).then_some(end - ch.start),
            markdown: body[start..end].into(),
        })
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReaderAnchor {
    pub revision_id: String,
    pub side: String,
    pub sha256: String,
    pub chapter: String,
    /// UTF-8 byte offset within this chapter, never a fraction or translated original offset.
    pub offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bookmark {
    pub id: String,
    pub label: String,
    pub anchor: ReaderAnchor,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReaderState {
    /// Fresh identity for each deliberate open; retries keep it unchanged.
    pub visit_id: String,
    pub position: Option<ReaderAnchor>,
    pub bookmarks: Vec<Bookmark>,
}
impl ReaderState {
    pub fn version(&self) -> Result<String> {
        Ok(reader_sha256(&serde_json::to_vec(self)?))
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    latest_item: Option<String>,
    items: BTreeMap<String, ReaderState>,
}

fn ledger(root: &Path) -> Result<Ledger> {
    let path = root.join("catalog/reader-state.json");
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Ledger::default()),
        Err(e) => Err(e.into()),
        Ok(_) => load(root, path),
    }
}

pub fn reader_state(library: &Path, item: &str, ceiling: Sensitivity) -> Result<ReaderState> {
    with_consistent_snapshot(library, |root| {
        read_item_at_sensitivity(root, item, ceiling)?.context("library_resource_not_found")?;
        Ok(ledger(root)?.items.remove(item).unwrap_or_default())
    })
}

/// One atomic file commits position, bookmarks and last-opened item together.
/// Per-item CAS prevents a stale device from overwriting a newer position/draft.
pub fn save_reader_state(
    library: &Path,
    item: &str,
    expected: &str,
    next: ReaderState,
    ceiling: Sensitivity,
) -> Result<ReaderState> {
    if !valid_item_id(item)
        || !valid_request_id(&next.visit_id)
        || next.bookmarks.len() > 4096
        || serde_json::to_vec(&next)?.len() > 128 * 1024
    {
        anyhow::bail!("invalid_reader_state")
    }
    let root = existing_canonical_root(library)?;
    let lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    let (_, current) =
        read_item_at_sensitivity(&root, item, ceiling)?.context("library_resource_not_found")?;
    let mut all = ledger(&root)?;
    let previous = all.items.get(item).cloned().unwrap_or_default();
    let (doc, bodies) = document_locked(&root, item, &current.revision_id, ceiling)?;
    let mut ids = BTreeSet::new();
    for b in &next.bookmarks {
        if !valid_request_id(&b.id)
            || b.label.len() > 512
            || b.label.contains('\0')
            || !ids.insert(&b.id)
        {
            anyhow::bail!("invalid_reader_state")
        }
    }
    // Retain historical bookmarks unchanged; they remain explicitly revision-bound.
    // New or changed anchors, and the current reading position, must resolve now.
    for anchor in next.position.iter().chain(
        next.bookmarks
            .iter()
            .filter(|b| !previous.bookmarks.contains(b))
            .map(|b| &b.anchor),
    ) {
        if anchor.revision_id != doc.revision_id {
            anyhow::bail!("stale_item_revision")
        }
        let edition = doc
            .editions
            .iter()
            .find(|e| e.side == anchor.side && e.sha256 == anchor.sha256)
            .context("invalid_reader_anchor")?;
        let ch = edition
            .chapters
            .iter()
            .find(|c| c.key == anchor.chapter)
            .context("invalid_reader_anchor")?;
        if anchor.offset > ch.end - ch.start
            || !bodies[&anchor.side]
                .0
                .is_char_boundary(ch.start + anchor.offset)
        {
            anyhow::bail!("invalid_reader_anchor")
        }
    }
    // A retry of an acknowledged/unknown identical save must not change latest_item
    // after another document has been opened. A new open supplies a new visit_id.
    if previous == next && all.items.contains_key(item) {
        return Ok(previous);
    }
    if previous.version()? != expected {
        anyhow::bail!("stale_reader_state")
    }
    if previous.visit_id != next.visit_id {
        all.latest_item = Some(item.into());
    }
    all.items.insert(item.into(), next.clone());
    ensure_contained_dir(&root, Path::new("catalog"))?;
    durable_replace(
        &root,
        &root.join("catalog/reader-state.json"),
        &serde_json::to_vec(&all)?,
    )?;
    lock.unlock()?;
    Ok(next)
}

/// Never reveals the identity of a now-hidden/trashed item. Old revision anchors
/// are returned only to the item's authorized owner and must be resolved or reset.
pub fn reader_resume(
    library: &Path,
    ceiling: Sensitivity,
) -> Result<Option<(String, ReaderState)>> {
    with_consistent_snapshot(library, |root| {
        let mut all = ledger(root)?;
        let Some(id) = all.latest_item else {
            return Ok(None);
        };
        if read_item_at_sensitivity(root, &id, ceiling)?.is_none() {
            return Ok(None);
        }
        Ok(all.items.remove(&id).map(|s| (id, s)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use memex_core::{BlobDescriptor, VerifiedBlob};

    fn book() -> (VerifiedCapture, ItemDraft) {
        let mut capture = crate::tests::capture();
        capture.capture.sensitivity = Sensitivity::Private;
        let mut draft = crate::tests::draft();
        draft.sensitivity = Sensitivity::Private;
        draft.content_markdown = "甲🌹乙\n".into();
        let mut original = heading_edition("First chapter.\n");
        original.side = "original".into();
        let mut translated = heading_edition(&draft.content_markdown);
        translated.side = "translation".into();
        let manifest = BookManifest {
            schema_version: 1,
            alignment: "declared_chapter_order_not_paragraph_alignment".into(),
            editions: vec![original, translated],
        };
        capture.blobs = [
            ("original.md", "text/markdown", b"First chapter.\n".to_vec()),
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
            (
                "original.pdf",
                "application/pdf",
                b"%PDF-synthetic".to_vec(),
            ),
            (
                "translation.pdf",
                "application/pdf",
                b"%PDF-synthetic-translation".to_vec(),
            ),
        ]
        .into_iter()
        .map(|(name, media, bytes)| {
            let sha = reader_sha256(&bytes);
            VerifiedBlob {
                descriptor: BlobDescriptor {
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
        capture.capture.artifacts = capture.blobs.iter().map(|b| b.descriptor.clone()).collect();
        (capture, draft)
    }

    #[test]
    fn content_format_source_keeps_book_sides_separate() {
        use content_format::{source, Target};
        let dir = tempfile::tempdir().unwrap();
        let (capture, draft) = book();
        let receipt = ingest_reader_book(dir.path(), capture, draft).unwrap();
        let item = &receipt.item_ids[0];
        let revision = &receipt.revision_ids[0];
        let read = |target, ceiling| source(dir.path(), item, revision, target, ceiling);
        assert_eq!(read(Target::Body {}, Sensitivity::Private).err().unwrap().to_string(), "chapter_required");
        let target = |side: &str, chapter: &str| Target::Chapter { side: side.into(), chapter: chapter.into() };
        let original = read(target("original", "section-0"), Sensitivity::Private).unwrap();
        let translation = read(target("translation", "section-0"), Sensitivity::Private).unwrap();
        assert_eq!(original.markdown, "First chapter.\n");
        assert_eq!(translation.markdown, "甲🌹乙\n");
        assert_eq!(original.identity.sensitivity, Sensitivity::Private);
        assert_ne!(original.identity.source_sha256, translation.identity.source_sha256);
        assert_ne!(original.identity.target, translation.identity.target);
        assert!(read(target("original", "section-0"), Sensitivity::Public).is_err());
        assert!(read(target("source", "section-0"), Sensitivity::Private).is_err());
        assert!(read(target("original", "missing"), Sensitivity::Private).is_err());
    }

    #[test]
    fn chapter_translation_is_immutable_revision_bound_and_separate_from_editions() {
        use chapter_translation as tr;
        for (language, paired) in [("en-GB", false), ("fr", false), ("en", true)] {
            let dir = tempfile::tempdir().unwrap();
            let (mut capture, draft) = book();
            let manifest_blob = &mut capture.blobs[2];
            let mut manifest: BookManifest = serde_json::from_slice(&manifest_blob.bytes).unwrap();
            manifest.editions[0].language = language.into();
            manifest.editions[0].chapters[0].counterpart = paired.then(|| "section-0".into());
            manifest.editions[1].chapters[0].counterpart = paired.then(|| "section-0".into());
            manifest_blob.bytes = serde_json::to_vec(&manifest).unwrap();
            manifest_blob.descriptor.sha256 = reader_sha256(&manifest_blob.bytes);
            manifest_blob.descriptor.byte_size = manifest_blob.bytes.len() as u64;
            manifest_blob.blob_id = format!("sha256:{}", manifest_blob.descriptor.sha256);
            capture.capture.artifacts = capture.blobs.iter().map(|b| b.descriptor.clone()).collect();
            let mut other_capture = capture.clone();
            other_capture.capture.external_capture_id = "different-synthetic-item".into();
            let other_draft = draft.clone();
            let receipt = ingest_reader_book(dir.path(), capture, draft).unwrap();
            let item = &receipt.item_ids[0];
            let revision = &receipt.revision_ids[0];
            let before = read_item(dir.path(), item).unwrap().1;
            let source = tr::source(dir.path(), item, revision, "section-0", Sensitivity::Private);
            if paired || language == "fr" {
                assert!(source.is_err());
                assert!(!dir.path().join("catalog/chapter-translations").exists());
                continue;
            }
            let source = source.unwrap();
            assert_eq!(source.markdown, "First chapter.\n");
            assert!(tr::metadata(dir.path(), item, revision, "section-0", Sensitivity::Private).unwrap().is_none());
            let provenance = tr::Provenance { route: tr::Route { provider_id: "synthetic".into(), model_id: "test".into(), reasoning_effort: Some("low".into()) },
                prompt_version: "test-v1".into(), prompt_sha256: reader_sha256(b"prompt"), plan_sha256: reader_sha256(b"plan") };
            assert!(tr::publish(dir.path(), source.identity.clone(), provenance.clone(), "译🌹文".into(), Sensitivity::Public).is_err());
            let mut forged = source.identity.clone(); forged.source_sha256 = "0".repeat(64);
            assert!(tr::publish(dir.path(), forged, provenance.clone(), "译🌹文".into(), Sensitivity::Private).is_err());
            let output = "译🌹文\n\n[^1]: 注释。";
            let saved = tr::publish(dir.path(), source.identity.clone(), provenance.clone(), output.into(), Sensitivity::Private).unwrap();
            assert_eq!(saved.provenance, "machine_translation");
            assert_eq!(saved.sha256, reader_sha256(output.as_bytes()));
            assert_eq!(read_item(dir.path(), item).unwrap().1, before);
            recover_pending_ingests(dir.path()).unwrap();
            assert_eq!(tr::publish(dir.path(), source.identity.clone(), provenance.clone(), "Must not replace".into(), Sensitivity::Private).unwrap(), saved);
            assert!(tr::chunk(dir.path(), item, revision, "section-0", &saved.version, 1, 4, Sensitivity::Private).is_err());
            assert!(tr::chunk(dir.path(), item, revision, "section-0", "wrong", 0, 4, Sensitivity::Private).is_err());
            assert!(tr::metadata(dir.path(), item, revision, "section-0", Sensitivity::Public).is_err());
            let mut offset = 0;
            let mut restored = String::new();
            loop {
                let part = tr::chunk(dir.path(), item, revision, "section-0", &saved.version, offset, 4, Sensitivity::Private).unwrap();
                assert_eq!(part.start, offset);
                restored.push_str(&part.markdown);
                let Some(next) = part.next_offset else { break };
                offset = next;
            }
            assert_eq!(restored, output);
            let renamed = revise_item_metadata(dir.path(), item, ItemMetadataUpdate {
                expected_revision_id: revision.clone(), title: "Renamed synthetic book".into(), manual_tags: vec![],
            }).unwrap();
            let reused = tr::metadata(dir.path(), item, &renamed.revision_id, "section-0", Sensitivity::Private).unwrap()
                .expect("title-only revision must retain generated translation");
            assert_eq!(reused.sha256, saved.sha256);
            assert_eq!(reused.revision_id, renamed.revision_id);
            let reused_text = tr::chunk(dir.path(), item, &renamed.revision_id, "section-0", &reused.version, 0, 8192, Sensitivity::Private).unwrap();
            assert_eq!(reused_text.markdown, output);
            assert!(tr::chunk(dir.path(), item, &renamed.revision_id, "section-0", &saved.version, 0, 8192, Sensitivity::Private).is_err());
            assert!(tr::metadata(dir.path(), item, &renamed.revision_id, "section-0", Sensitivity::Public).is_err());
            let other = ingest_reader_book(dir.path(), other_capture, other_draft).unwrap();
            assert_ne!(other.item_ids[0], *item);
            assert!(tr::metadata(dir.path(), &other.item_ids[0], &other.revision_ids[0], "section-0", Sensitivity::Private).unwrap().is_none());
            let renamed = revise_item_metadata(dir.path(), item, ItemMetadataUpdate {
                expected_revision_id: renamed.revision_id, title: "Second synthetic title".into(), manual_tags: vec![],
            }).unwrap();
            let second = tr::metadata(dir.path(), item, &renamed.revision_id, "section-0", Sensitivity::Private).unwrap().unwrap();
            assert_eq!(second.sha256, saved.sha256);
            assert_ne!(second.version, reused.version);
            assert_eq!(second.version, reader_sha256(&serde_json::to_vec(&["memex.reader.metadata-view.v1", "chapter_translation", item, &renamed.revision_id, revision, &saved.version]).unwrap()));
            let current_file = dir.path().join("catalog/chapter-translations").join(item).join(&renamed.revision_id).join(format!("{}.json", reader_sha256(b"section-0")));
            fs::create_dir_all(current_file.parent().unwrap()).unwrap();
            fs::write(&current_file, b"{}").unwrap();
            assert!(tr::metadata(dir.path(), item, &renamed.revision_id, "section-0", Sensitivity::Private).is_err());
            fs::remove_file(current_file).unwrap(); // disposable synthetic corruption only
            let next_text = "Owner changed the source.\n";
            let begin = chapter_edit::Begin { item_id: item.clone(), revision_id: renamed.revision_id, side: "original".into(), chapter: "section-0".into(),
                expected_chapter_sha256: source.identity.source_sha256.clone(), request_id: "invalidate-translation".into(), bytes: next_text.len(), sha256: reader_sha256(next_text.as_bytes()) };
            let stage = chapter_edit::begin(dir.path(), begin, "local_owner", Sensitivity::Private).unwrap();
            chapter_edit::chunk(dir.path(), item, &stage.edit_id, 0, next_text, "local_owner", Sensitivity::Private).unwrap();
            let next = chapter_edit::commit(dir.path(), item, &stage.edit_id, "local_owner", Sensitivity::Private).unwrap();
            assert!(tr::metadata(dir.path(), item, revision, "section-0", Sensitivity::Private).is_err());
            assert!(tr::metadata(dir.path(), item, &next.revision_id, "section-0", Sensitivity::Private).unwrap().is_none());
            let original_text = "First chapter.\n";
            let restore = chapter_edit::Begin { item_id: item.clone(), revision_id: next.revision_id, side: "original".into(), chapter: "section-0".into(),
                expected_chapter_sha256: reader_sha256(next_text.as_bytes()), request_id: "restore-same-source".into(), bytes: original_text.len(), sha256: reader_sha256(original_text.as_bytes()) };
            let stage = chapter_edit::begin(dir.path(), restore, "local_owner", Sensitivity::Private).unwrap();
            chapter_edit::chunk(dir.path(), item, &stage.edit_id, 0, original_text, "local_owner", Sensitivity::Private).unwrap();
            let restored = chapter_edit::commit(dir.path(), item, &stage.edit_id, "local_owner", Sensitivity::Private).unwrap();
            let renamed_restored = revise_item_metadata(dir.path(), item, ItemMetadataUpdate { expected_revision_id: restored.revision_id, title: "Restored then renamed".into(), manual_tags: vec![] }).unwrap();
            assert!(tr::metadata(dir.path(), item, &renamed_restored.revision_id, "section-0", Sensitivity::Private).unwrap().is_none());
            assert!(tr::publish(dir.path(), source.identity, provenance, output.into(), Sensitivity::Private).is_err());
            let old_projection = dir.path().join("catalog/chapter-translations").join(item).join(revision).join(format!("{}.json", reader_sha256(b"section-0")));
            assert!(old_projection.is_file());
        }
    }

    #[test]
    fn unbound_trailing_manifest_never_overrides_import_authority() {
        for malformed in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let (mut capture, draft) = book();
            let mut trailing = capture.blobs[2].clone();
            trailing.descriptor.locator = "unbound-manifest.json".into();
            trailing.bytes = if malformed { b"{}".to_vec() } else {
                let mut manifest: BookManifest = serde_json::from_slice(&trailing.bytes).unwrap();
                manifest.editions[0].chapters[0].key = "untrusted-key".into();
                serde_json::to_vec(&manifest).unwrap()
            };
            trailing.descriptor.sha256 = reader_sha256(&trailing.bytes);
            trailing.descriptor.byte_size = trailing.bytes.len() as u64;
            trailing.blob_id = format!("sha256:{}", trailing.descriptor.sha256);
            capture.blobs.push(trailing);
            capture.capture.artifacts = capture.blobs.iter().map(|b| b.descriptor.clone()).collect();
            let receipt = ingest_reader_book(dir.path(), capture, draft).unwrap();
            let doc = reader_document(dir.path(), &receipt.item_ids[0], &receipt.revision_ids[0], Sensitivity::Private).unwrap();
            assert_eq!(doc.editions[0].chapters[0].key, "section-0");
        }
    }

    #[test]
    fn chapter_edit_upload_cas_replay_and_recovery_preserve_other_chapters() {
        use chapter_edit::{begin, chunk, commit, Begin};
        for side in ["original", "translation"] {
            for interruption in [None, Some(0), Some(1), Some(2), Some(3), Some(4)] {
                let dir = tempfile::tempdir().unwrap();
                let (mut capture, mut draft) = book();
                let original = "# One\n\nAlpha.\n\n# Two\n\nKeep.\n";
                let translation = "# 一\n\n甲。\n\n# 二\n\n不变。\n";
                draft.content_markdown = translation.into();
                let mut editions = vec![heading_edition(original), heading_edition(translation)];
                for (edition, name) in editions.iter_mut().zip(["original", "translation"]) {
                    edition.side = name.into();
                    for chapter in &mut edition.chapters { chapter.counterpart = Some(chapter.key.clone()); }
                }
                let manifest = BookManifest { schema_version: 1, alignment: "declared_chapter_order_not_paragraph_alignment".into(), editions };
                for (i, bytes) in [(0, original.as_bytes().to_vec()), (1, translation.as_bytes().to_vec()),
                    (2, serde_json::to_vec(&manifest).unwrap())] {
                    let b = &mut capture.blobs[i];
                    b.descriptor.sha256 = reader_sha256(&bytes);
                    b.descriptor.byte_size = bytes.len() as u64;
                    b.blob_id = format!("sha256:{}", b.descriptor.sha256);
                    b.bytes = bytes;
                }
                capture.capture.artifacts = capture.blobs.iter().map(|b| b.descriptor.clone()).collect();
                let imported = ingest_reader_book(dir.path(), capture, draft).unwrap();
                let item = &imported.item_ids[0];
                let old = read_item(dir.path(), item).unwrap().1;
                let root = existing_canonical_root(dir.path()).unwrap();
                let (doc, bodies) = document_revision_locked(&root, &old, Sensitivity::Private).unwrap();
                let ch = &doc.editions.iter().find(|e| e.side == side).unwrap().chapters[0];
                let markdown = "# Owner heading\n\nChanged 🌹 prose.\n\n";
                let request = Begin { item_id: item.clone(), revision_id: old.revision_id.clone(),
                    side: side.into(), chapter: ch.key.clone(), request_id: "owner-edit".into(),
                    expected_chapter_sha256: reader_sha256(bodies[side].0[ch.start..ch.end].as_bytes()),
                    bytes: markdown.len(), sha256: reader_sha256(markdown.as_bytes()) };
                assert!(begin(dir.path(), request.clone(), "local_owner", Sensitivity::Public).is_err());
                let upload = begin(dir.path(), request.clone(), "local_owner", Sensitivity::Private).unwrap();
                let id = &upload.edit_id;
                assert!(chunk(dir.path(), item, id, 0, "x", &"a".repeat(64), Sensitivity::Private).is_err());
                assert!(chunk(dir.path(), item, id, 1, "x", "local_owner", Sensitivity::Private).is_err());
                assert_eq!(chunk(dir.path(), item, id, 0, &markdown[..17], "local_owner", Sensitivity::Private).unwrap(), 17);
                assert_eq!(begin(dir.path(), request.clone(), "local_owner", Sensitivity::Private).unwrap().received_bytes, 17);
                assert_eq!(chunk(dir.path(), item, id, 0, &markdown[..17], "local_owner", Sensitivity::Private).unwrap(), 17);
                assert!(chunk(dir.path(), item, id, 0, "different", "local_owner", Sensitivity::Private).is_err());
                assert!(commit(dir.path(), item, id, "local_owner", Sensitivity::Private).is_err());
                chunk(dir.path(), item, id, 17, &markdown[17..], "local_owner", Sensitivity::Private).unwrap();
                let mut conflict = request.clone(); conflict.sha256 = "0".repeat(64);
                assert!(begin(dir.path(), conflict, "local_owner", Sensitivity::Private).is_err());
                let mut competing = request.clone(); competing.request_id = "other-editor".into();
                let other = begin(dir.path(), competing, "local_owner", Sensitivity::Private).unwrap();
                chunk(dir.path(), item, &other.edit_id, 0, markdown, "local_owner", Sensitivity::Private).unwrap();
                FAIL_ITEM_UPDATE_STEP.with(|s| s.set(interruption));
                let result = commit(dir.path(), item, id, "local_owner", Sensitivity::Private);
                FAIL_ITEM_UPDATE_STEP.with(|s| s.set(None));
                assert_eq!(result.is_err(), interruption.is_some(), "{result:?}");
                recover_pending_ingests(dir.path()).unwrap();
                if interruption == Some(0) { assert_eq!(read_item(dir.path(), item).unwrap().1, old); }
                let receipt = commit(dir.path(), item, id, "local_owner", Sensitivity::Private).unwrap();
                assert!(receipt.changed);
                assert_eq!(commit(dir.path(), item, id, "local_owner", Sensitivity::Private).unwrap(), receipt);
                assert!(commit(dir.path(), item, &other.edit_id, "local_owner", Sensitivity::Private).is_err());
                let next = read_item(dir.path(), item).unwrap().1;
                assert_eq!(next.revision_number, old.revision_number + 1);
                assert_eq!(next.title, old.title);
                assert_eq!(next.metadata, old.metadata);
                assert_eq!(&next.evidence_ids[..old.evidence_ids.len()], old.evidence_ids);
                let (new_doc, new_bodies) = document_revision_locked(&root, &next, Sensitivity::Private).unwrap();
                for (old_e, new_e) in doc.editions.iter().zip(&new_doc.editions) {
                    if new_e.side != side { assert_eq!(new_e, old_e); assert_eq!(new_bodies[&new_e.side].0, bodies[&new_e.side].0); }
                    for (i, (old_ch, new_ch)) in old_e.chapters.iter().zip(&new_e.chapters).enumerate() {
                        assert_eq!((&old_ch.key, &old_ch.title, old_ch.level, &old_ch.counterpart), (&new_ch.key, &new_ch.title, new_ch.level, &new_ch.counterpart));
                        let text = &new_bodies[&new_e.side].0[new_ch.start..new_ch.end];
                        if new_e.side == side && i == 0 { assert_eq!(text, markdown); }
                        else { assert_eq!(text, &bodies[&old_e.side].0[old_ch.start..old_ch.end]); }
                    }
                }
                assert_eq!(paragraphs::read(dir.path(), item, &next.revision_id, "section-0", 0, 256, "any", Sensitivity::Private).unwrap().version, "missing");
                let noop = Begin { revision_id: next.revision_id.clone(), request_id: "no-change".into(), expected_chapter_sha256: request.sha256.clone(), ..request.clone() };
                let no_upload = begin(dir.path(), noop, "local_owner", Sensitivity::Private).unwrap();
                chunk(dir.path(), item, &no_upload.edit_id, 0, markdown, "local_owner", Sensitivity::Private).unwrap();
                assert!(!commit(dir.path(), item, &no_upload.edit_id, "local_owner", Sensitivity::Private).unwrap().changed);
                let mut later = request.clone();
                later.request_id = "later-edit".into(); later.revision_id = next.revision_id.clone();
                later.expected_chapter_sha256 = request.sha256.clone(); later.bytes = 6; later.sha256 = reader_sha256(b"Later.");
                let later_upload = begin(dir.path(), later, "local_owner", Sensitivity::Private).unwrap();
                chunk(dir.path(), item, &later_upload.edit_id, 0, "Later.", "local_owner", Sensitivity::Private).unwrap();
                commit(dir.path(), item, &later_upload.edit_id, "local_owner", Sensitivity::Private).unwrap();
                assert_eq!(begin(dir.path(), request, "local_owner", Sensitivity::Private).unwrap().receipt, Some(receipt.clone()));
                assert_eq!(commit(dir.path(), item, id, "local_owner", Sensitivity::Private).unwrap(), receipt);
                assert!(commit(dir.path(), item, id, "local_owner", Sensitivity::Public).is_err());
            }
        }
    }

    #[test]
    fn chapter_repair_recovers_both_editions_and_index_without_touching_other_chapters() {
        use paragraphs::{Kind, Pair, Relation};
        use repair::{Edit, Plan};
        for interruption in [None, Some(0), Some(1), Some(2), Some(3), Some(4)] {
            let dir = tempfile::tempdir().unwrap();
            let (mut capture, mut draft) = book();
            let original = "# One\n\nAlpha\n\nbeta.\n\n# Two\n\nKeep.\n";
            let translation = "# 一\n\n甲\n\n乙。\n\n# 二\n\n不变。\n";
            draft.content_markdown = translation.into();
            let mut editions = vec![heading_edition(original), heading_edition(translation)];
            for (edition, side) in editions.iter_mut().zip(["original", "translation"]) {
                edition.side = side.into();
                for chapter in &mut edition.chapters { chapter.counterpart = Some(chapter.key.clone()); }
            }
            let manifest = BookManifest { schema_version: 1,
                alignment: "declared_chapter_order_not_paragraph_alignment".into(), editions };
            for (i, bytes) in [(0, original.as_bytes().to_vec()), (1, translation.as_bytes().to_vec()),
                (2, serde_json::to_vec(&manifest).unwrap())] {
                let b = &mut capture.blobs[i];
                b.descriptor.sha256 = reader_sha256(&bytes);
                b.descriptor.byte_size = bytes.len() as u64;
                b.blob_id = format!("sha256:{}", b.descriptor.sha256);
                b.bytes = bytes;
            }
            capture.capture.artifacts = capture.blobs.iter().map(|b| b.descriptor.clone()).collect();
            let imported = ingest_reader_book(dir.path(), capture, draft).unwrap();
            let item = &imported.item_ids[0];
            let revision = &imported.revision_ids[0];
            let old = read_item(dir.path(), item).unwrap().1;
            let plan = Plan {
                schema_version: 1,
                identity: paragraphs::identity(dir.path(), item, revision, "section-0", Sensitivity::Private).unwrap(),
                expected_alignment_version: "missing".into(), input_profile_sha256: reader_sha256(b"offline-owner-plan"),
                original_edits: vec![Edit { range: [12, 14], replacement: " ".into() }],
                translation_edits: vec![Edit { range: [10, 12], replacement: " ".into() }],
                pairs: vec![
                    Pair { o: Some([0, 7]), t: Some([0, 7]), kind: Kind::Heading, relation: Relation::Matched },
                    Pair { o: Some([7, 20]), t: Some([7, 19]), kind: Kind::Paragraph, relation: Relation::Matched },
                ],
            };
            assert!(repair::apply(dir.path(), plan.clone(), Sensitivity::Public).is_err());
            for variant in 0..4 {
                let mut bad = plan.clone();
                match variant {
                    0 => bad.original_edits[0].replacement = "rewritten".into(),
                    1 => bad.translation_edits[0].range = [8, 12], // split UTF-8
                    2 => bad.original_edits[0].range = [21, 23], // outside selected chapter
                    _ => bad.expected_alignment_version = "0".repeat(64),
                }
                assert!(repair::apply(dir.path(), bad, Sensitivity::Private).is_err());
                assert_eq!(read_item(dir.path(), item).unwrap().1, old);
            }
            FAIL_ITEM_UPDATE_STEP.with(|s| s.set(interruption));
            let result = repair::apply(dir.path(), plan.clone(), Sensitivity::Private);
            FAIL_ITEM_UPDATE_STEP.with(|s| s.set(None));
            assert_eq!(result.is_err(), interruption.is_some(), "{result:?}");
            recover_pending_ingests(dir.path()).unwrap();
            let next = read_item(dir.path(), item).unwrap().1;
            if interruption == Some(0) {
                assert_eq!(next, old);
                assert_eq!(paragraphs::read(dir.path(), item, revision, "section-0", 0, 256, "any", Sensitivity::Private).unwrap().version, "missing");
                continue;
            }
            assert_eq!(next.content_markdown, "# 一\n\n甲 乙。\n\n# 二\n\n不变。\n");
            assert_eq!(&next.evidence_ids[..old.evidence_ids.len()], old.evidence_ids);
            let root = existing_canonical_root(dir.path()).unwrap();
            let (doc, bodies) = document_revision_locked(&root, &next, Sensitivity::Private).unwrap();
            assert_eq!(bodies["original"].0, "# One\n\nAlpha beta.\n\n# Two\n\nKeep.\n");
            for edition in &doc.editions {
                let ch = &edition.chapters[1];
                assert_eq!(ch.key, "section-1");
                assert_eq!(ch.counterpart.as_deref(), Some("section-1"));
                assert_eq!(&bodies[&edition.side].0[ch.start..ch.end],
                    if edition.side == "original" { "# Two\n\nKeep.\n" } else { "# 二\n\n不变。\n" });
            }
            let page = paragraphs::read(dir.path(), item, &next.revision_id, "section-0", 0, 256, "any", Sensitivity::Private).unwrap();
            assert_eq!(page.status, "aligned");
            assert_eq!(page.pairs, plan.pairs);
            let replay = repair::apply(dir.path(), plan.clone(), Sensitivity::Private).unwrap();
            assert_eq!(replay.revision_id, next.revision_id);
            let mut conflicting = plan.clone();
            conflicting.input_profile_sha256 = "0".repeat(64);
            assert!(repair::apply(dir.path(), conflicting, Sensitivity::Private).is_err());
            // Updating only the derived index keeps the content revision and history.
            let no_edit = Plan { identity: paragraphs::identity(dir.path(), item, &next.revision_id, "section-0", Sensitivity::Private).unwrap(),
                expected_alignment_version: page.version, original_edits: vec![], translation_edits: vec![], ..plan };
            let receipt = repair::apply(dir.path(), no_edit.clone(), Sensitivity::Private).unwrap();
            assert_eq!(receipt.revision_id, next.revision_id);
            assert_eq!(read_item(dir.path(), item).unwrap().1, next);
            if interruption.is_none() {
                // Distinct plans racing the same revision must have one winner.
                // Only original changes here: the Chinese canonical hash stays fixed.
                let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
                let handles: Vec<_> = (0..2).map(|i| {
                    let mut plan = no_edit.clone();
                    plan.original_edits = vec![Edit { range: [12, 13], replacement: "\n".into() }];
                    plan.input_profile_sha256 = reader_sha256(format!("racer-{i}").as_bytes());
                    let root = dir.path().to_owned();
                    let barrier = barrier.clone();
                    std::thread::spawn(move || {
                        barrier.wait();
                        repair::apply(&root, plan, Sensitivity::Private)
                    })
                }).collect();
                let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
                assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
                assert_eq!(results.iter().find_map(|r| r.as_ref().err()).unwrap().to_string(), "stale_item_revision");
                let latest = read_item(dir.path(), item).unwrap().1;
                assert_eq!(latest.content_markdown, next.content_markdown);
                assert_eq!(latest.revision_number, next.revision_number + 1);
                let (_, bodies) = document_revision_locked(&root, &latest, Sensitivity::Private).unwrap();
                assert_eq!(bodies["original"].0, "# One\n\nAlpha\nbeta.\n\n# Two\n\nKeep.\n");
            }
        }
    }

    #[test]
    fn paragraph_index_partitions_both_sides_and_paginates_with_cas() {
        use paragraphs::{Index, Kind, Pair, Relation};
        let dir = tempfile::tempdir().unwrap();
        let (mut capture, mut draft) = book();
        let original = "One.\n\nTwo.\n\nThree.\n\nUncertain.\n";
        let translation = "甲🌹。\n\n乙。\n\n丙。\n\n[^1]: 注释。\ncontinued\n\n    continuation.\n";
        draft.content_markdown = translation.into();
        let mut left = heading_edition(original);
        left.side = "original".into();
        left.chapters[0].counterpart = Some("section-0".into());
        let mut right = heading_edition(translation);
        right.side = "translation".into();
        right.chapters[0].counterpart = Some("section-0".into());
        let manifest = BookManifest { schema_version: 1,
            alignment: "declared_chapter_order_not_paragraph_alignment".into(), editions: vec![left, right] };
        for (i, bytes) in [(0, original.as_bytes().to_vec()), (1, translation.as_bytes().to_vec()),
            (2, serde_json::to_vec(&manifest).unwrap())] {
            let b = &mut capture.blobs[i];
            b.descriptor.sha256 = reader_sha256(&bytes);
            b.descriptor.byte_size = bytes.len() as u64;
            b.blob_id = format!("sha256:{}", b.descriptor.sha256);
            b.bytes = bytes;
        }
        capture.capture.artifacts = capture.blobs.iter().map(|b| b.descriptor.clone()).collect();
        let mut other_capture = capture.clone();
        other_capture.capture.external_capture_id = "alignment-other-item".into();
        let other_draft = draft.clone();
        let receipt = ingest_reader_book(dir.path(), capture, draft).unwrap();
        let item = &receipt.item_ids[0];
        let rev = &receipt.revision_ids[0];
        let missing = paragraphs::read(dir.path(), item, rev, "section-0", 0, 2, "any", Sensitivity::Private).unwrap();
        assert_eq!((missing.version.as_str(), missing.status.as_str(), missing.total), ("missing", "none", 0));
        let original_revision = read_item(dir.path(), item).unwrap().1;
        let mut index = Index { schema_version: 1,
            identity: paragraphs::identity(dir.path(), item, rev, "section-0", Sensitivity::Private).unwrap(),
            algorithm: "chapter-paragraphs-v1".into(), input_profile_sha256: reader_sha256(b"synthetic"),
            pairs: vec![
                Pair { o: Some([0, 6]), t: Some([0, 20]), kind: Kind::Paragraph, relation: Relation::Matched },
                Pair { o: Some([6, 20]), t: Some([20, 28]), kind: Kind::Paragraph, relation: Relation::Matched },
                Pair { o: None, t: Some([28, translation.len()]), kind: Kind::Note, relation: Relation::TranslationOnly },
                Pair { o: Some([20, original.len()]), t: None, kind: Kind::Paragraph, relation: Relation::Unresolved },
            ] };
        // Exact byte counts above are independently stated, not taken from the block parser.
        // Chinese first block is 12 bytes, second and third 8 each.
        paragraphs::save(dir.path(), "missing", index.clone(), Sensitivity::Private).unwrap();
        let first = paragraphs::read(dir.path(), item, rev, "section-0", 0, 2, "any", Sensitivity::Private).unwrap();
        assert_eq!(first.status, "partial");
        assert_eq!(first.total, 4);
        assert_eq!(first.next_offset, Some(2));
        assert_eq!(first.pairs, index.pairs[..2]);
        let tail = paragraphs::read(dir.path(), item, rev, "section-0", 2, 2, &first.version, Sensitivity::Private).unwrap();
        assert_eq!(tail.pairs, index.pairs[2..]);
        assert_eq!(tail.next_offset, None);
        assert!(paragraphs::read(dir.path(), item, rev, "section-0", 2, 2, "any", Sensitivity::Private).is_err());
        assert!(paragraphs::read(dir.path(), item, rev, "section-0", 0, 2, "any", Sensitivity::Public).is_err());
        for case in 0..9 {
            let mut bad = index.clone();
            match case {
                0 => bad.pairs[0].t = Some([0, 4]), // split emoji
                1 => bad.pairs[1].o = Some([7, 20]), // gap / partial block
                2 => bad.pairs[1].o = Some([0, 20]), // overlap
                3 => { bad.pairs.pop(); }, // omission
                4 => bad.pairs[2].kind = Kind::Paragraph, // note swallowed
                5 => bad.pairs[0].relation = Relation::Unresolved, // false pair
                6 => bad.identity.original.sha256 = "0".repeat(64),
                7 => bad.identity.original.chapter_start = 1,
                _ => {
                    // A note's indented continuation must never match body text.
                    bad.pairs[2].t = Some([28, 55]);
                    bad.pairs[3].t = Some([55, translation.len()]);
                    bad.pairs[3].relation = Relation::Matched;
                }
            }
            assert!(paragraphs::save(dir.path(), &first.version, bad, Sensitivity::Private).is_err(), "case {case}");
        }
        index.pairs[3].relation = Relation::OriginalOnly;
        assert!(paragraphs::save(dir.path(), "missing", index.clone(), Sensitivity::Private).is_err());
        paragraphs::save(dir.path(), &first.version, index.clone(), Sensitivity::Private).unwrap();
        assert!(paragraphs::read(dir.path(), item, rev, "section-0", 2, 2, &first.version, Sensitivity::Private).is_err());
        assert_eq!(paragraphs::read(dir.path(), item, rev, "section-0", 0, 256, "any", Sensitivity::Private).unwrap().status, "aligned");
        assert_eq!(paragraphs::save(dir.path(), "missing", index.clone(), Sensitivity::Private).unwrap(), index);
        assert_eq!(read_item(dir.path(), item).unwrap().1, original_revision);
        let renamed = revise_item_metadata(dir.path(), item, ItemMetadataUpdate {
            expected_revision_id: rev.clone(), title: "《Synthetic》Book".into(), manual_tags: vec![],
        }).unwrap();
        let after = paragraphs::read(dir.path(), item, &renamed.revision_id, "section-0", 0, 256, "any", Sensitivity::Private).unwrap();
        assert_eq!(after.pairs, index.pairs, "title-only revision must retain paragraph alignment");
        assert_eq!(after.revision_id, renamed.revision_id);
        assert!(paragraphs::path(dir.path(), &index.identity).is_file());
        let other = ingest_reader_book(dir.path(), other_capture, other_draft).unwrap();
        assert_ne!(other.item_ids[0], *item);
        assert_eq!(paragraphs::read(dir.path(), &other.item_ids[0], &other.revision_ids[0], "section-0", 0, 256, "any", Sensitivity::Private).unwrap().version, "missing");
        let expected = reader_sha256(serde_json::to_string(&["memex.reader.metadata-view.v1", "paragraph_alignment", item, &renamed.revision_id, rev, &index.version().unwrap()]).unwrap().as_bytes());
        assert_eq!(after.version, expected);
        let renamed_again = revise_item_metadata(dir.path(), item, ItemMetadataUpdate {
            expected_revision_id: renamed.revision_id, title: "Second title".into(), manual_tags: vec!["tag".into()],
        }).unwrap();
        let current = &renamed_again.revision_id;
        assert!(paragraphs::read(dir.path(), item, current, "section-0", 2, 2, &after.version, Sensitivity::Private).is_err());
        let page = paragraphs::read(dir.path(), item, current, "section-0", 0, 256, "any", Sensitivity::Private).unwrap();
        let plan = repair::plan(dir.path(), item, current, "section-0", Sensitivity::Private).unwrap();
        assert_eq!(plan.expected_alignment_version, page.version);
        assert_eq!(plan.pairs, index.pairs);
        let mut stale = plan.clone(); stale.expected_alignment_version = "missing".into();
        assert!(repair::apply(dir.path(), stale, Sensitivity::Private).is_err());
        let committed = repair::apply(dir.path(), plan, Sensitivity::Private).unwrap();
        assert_eq!(committed.revision_id, *current);
        index.identity.revision_id = current.clone();
        assert_eq!(committed.alignment_version, index.version().unwrap());
        let mut none = index.clone();
        none.pairs = index.pairs.iter().flat_map(|p| [
            p.o.map(|o| Pair { o: Some(o), t: None, kind: p.kind, relation: Relation::OriginalOnly }),
            p.t.map(|t| Pair { o: None, t: Some(t), kind: p.kind, relation: Relation::TranslationOnly }),
        ].into_iter().flatten()).collect();
        assert!(paragraphs::save(dir.path(), &page.version, none.clone(), Sensitivity::Private).is_err());
        paragraphs::save(dir.path(), &committed.alignment_version, none.clone(), Sensitivity::Private).unwrap();
        assert_eq!(paragraphs::read(dir.path(), item, current, "section-0", 0, 256, "any", Sensitivity::Private).unwrap().status, "none");
        fs::write(paragraphs::path(dir.path(), &none.identity), b"{}").unwrap();
        assert!(paragraphs::read(dir.path(), item, current, "section-0", 0, 256, "any", Sensitivity::Private).is_err());
        fs::write(paragraphs::path(dir.path(), &none.identity), serde_json::to_vec(&none).unwrap()).unwrap();
        let renamed_third = revise_item_metadata(dir.path(), item, ItemMetadataUpdate { expected_revision_id: current.clone(), title: "Third title".into(), manual_tags: vec![] }).unwrap();
        let inherited_none = paragraphs::read(dir.path(), item, &renamed_third.revision_id, "section-0", 0, 256, "any", Sensitivity::Private).unwrap();
        assert_eq!(inherited_none.status, "none");
        none.identity.revision_id = renamed_third.revision_id.clone();
        assert!(paragraphs::save(dir.path(), "missing", none.clone(), Sensitivity::Private).is_err());
        paragraphs::save(dir.path(), &inherited_none.version, none.clone(), Sensitivity::Private).unwrap();
        let mut current = renamed_third.revision_id;
        let mut old_text = original;
        for (request, text) in [("change-aligned-source", "Changed source.\n"), ("restore-aligned-source", original)] {
            let begin = chapter_edit::Begin { item_id: item.clone(), revision_id: current, side: "original".into(), chapter: "section-0".into(),
                expected_chapter_sha256: reader_sha256(old_text.as_bytes()), request_id: request.into(), bytes: text.len(), sha256: reader_sha256(text.as_bytes()) };
            let stage = chapter_edit::begin(dir.path(), begin, "local_owner", Sensitivity::Private).unwrap();
            chapter_edit::chunk(dir.path(), item, &stage.edit_id, 0, text, "local_owner", Sensitivity::Private).unwrap();
            current = chapter_edit::commit(dir.path(), item, &stage.edit_id, "local_owner", Sensitivity::Private).unwrap().revision_id;
            old_text = text;
        }
        let renamed_restored = revise_item_metadata(dir.path(), item, ItemMetadataUpdate { expected_revision_id: current, title: "Same bytes renamed".into(), manual_tags: vec![] }).unwrap();
        assert_eq!(paragraphs::read(dir.path(), item, &renamed_restored.revision_id, "section-0", 0, 256, "any", Sensitivity::Private).unwrap().version, "missing");
    }

    #[test]
    fn content_edits_keep_only_byte_identical_chapter_alignment_and_translation() {
        use chapter_translation as tr;
        use content_format::edits as fe;
        use paragraphs::{Index, Pair, Relation};
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        let (mut capture, mut draft) = book();
        // original section-0 is untranslated English (AI target); section-1 <-> section-0
        // is the aligned pair; section-2 <-> section-1 is where other edits happen.
        let (o0, o1, o2) = ("# A\n\nOne.\n\n", "# B\n\nTwo.\n\n", "# C\n\nThree  more.\n");
        let (t0, t1) = ("# 乙\n\n二。\n\n", "# 丙\n\n三。\n");
        let (original, translation) = ([o0, o1, o2].concat(), [t0, t1].concat());
        draft.content_markdown = translation.clone();
        let mut left = heading_edition(&original);
        left.side = "original".into();
        left.language = "en".into();
        left.chapters[1].counterpart = Some("section-0".into());
        left.chapters[2].counterpart = Some("section-1".into());
        let mut right = heading_edition(&translation);
        right.side = "translation".into();
        right.language = "zh".into();
        right.chapters[0].counterpart = Some("section-1".into());
        right.chapters[1].counterpart = Some("section-2".into());
        let manifest = BookManifest { schema_version: 1,
            alignment: "declared_chapter_order_not_paragraph_alignment".into(), editions: vec![left, right] };
        for (i, bytes) in [(0, original.as_bytes().to_vec()), (1, translation.as_bytes().to_vec()),
            (2, serde_json::to_vec(&manifest).unwrap())] {
            let b = &mut capture.blobs[i];
            b.descriptor.sha256 = reader_sha256(&bytes);
            b.descriptor.byte_size = bytes.len() as u64;
            b.blob_id = format!("sha256:{}", b.descriptor.sha256);
            b.bytes = bytes;
        }
        capture.capture.artifacts = capture.blobs.iter().map(|b| b.descriptor.clone()).collect();
        let receipt = ingest_reader_book(p, capture, draft).unwrap();
        let item = &receipt.item_ids[0];
        let rev0 = receipt.revision_ids[0].clone();
        let id = paragraphs::identity(p, item, &rev0, "section-1", Sensitivity::Private).unwrap();
        let pairs: Vec<Pair> = paragraphs::blocks(o1).into_iter().zip(paragraphs::blocks(t0))
            .map(|(a, b)| Pair { o: Some(a.0), t: Some(b.0), kind: a.1, relation: Relation::Matched }).collect();
        assert_eq!(pairs.len(), 2);
        let index = Index { schema_version: 1, identity: id, algorithm: "chapter-paragraphs-v1".into(),
            input_profile_sha256: reader_sha256(b"profile"), pairs: pairs.clone() };
        paragraphs::save(p, "missing", index.clone(), Sensitivity::Private).unwrap();
        let source = tr::source(p, item, &rev0, "section-0", Sensitivity::Private).unwrap();
        let provenance = tr::Provenance { route: tr::Route { provider_id: "synthetic".into(), model_id: "test".into(), reasoning_effort: None },
            prompt_version: "test-v1".into(), prompt_sha256: reader_sha256(b"prompt"), plan_sha256: reader_sha256(b"plan") };
        let ai = "# 甲\n\n一。\n\n";
        let saved = tr::publish(p, source.identity.clone(), provenance, ai.into(), Sensitivity::Private).unwrap();
        let files = || walkdir_count(&p.join("catalog/chapter-translations")) + walkdir_count(&p.join("catalog/chapter-alignments"));
        let edit = |rev: &str, side: &str, chapter: &str, old: &str, text: &str, request: &str| -> String {
            let begin = chapter_edit::Begin { item_id: item.clone(), revision_id: rev.into(), side: side.into(), chapter: chapter.into(),
                expected_chapter_sha256: reader_sha256(old.as_bytes()), request_id: request.into(), bytes: text.len(), sha256: reader_sha256(text.as_bytes()) };
            let stage = chapter_edit::begin(p, begin, "local_owner", Sensitivity::Private).unwrap();
            chapter_edit::chunk(p, item, &stage.edit_id, 0, text, "local_owner", Sensitivity::Private).unwrap();
            chapter_edit::commit(p, item, &stage.edit_id, "local_owner", Sensitivity::Private).unwrap().revision_id
        };
        let kept = |rev: &str, what: &str| {
            assert_eq!(read_item(p, item).unwrap().1.revision_id, rev);
            let page = paragraphs::read(p, item, rev, "section-1", 0, 256, "any", Sensitivity::Private).unwrap();
            assert_eq!((page.status.as_str(), &page.pairs, page.revision_id.as_str()), ("aligned", &pairs, rev), "{what}");
            assert_ne!(page.version, index.version().unwrap(), "{what}");
            let m = tr::metadata(p, item, rev, "section-0", Sensitivity::Private).unwrap().unwrap_or_else(|| panic!("{what}: translation lost"));
            assert_eq!((m.sha256.as_str(), m.revision_id.as_str()), (saved.sha256.as_str(), rev), "{what}");
            assert_eq!(tr::chunk(p, item, rev, "section-0", &m.version, 0, 4096, Sensitivity::Private).unwrap().markdown, ai, "{what}");
            assert!(tr::chunk(p, item, rev, "section-0", &saved.version, 0, 4096, Sensitivity::Private).is_err(), "{what}");
            assert!(tr::metadata(p, item, rev, "section-0", Sensitivity::Public).is_err(), "{what}");
            page.version
        };
        // Each newly crossable transition, on another chapter, keeps both artifacts.
        let rev1 = edit(&rev0, "translation", "section-1", t1, "# 丙\n\n三改。\n", "edit-unrelated");
        kept(&rev1, "chapter edit");
        let s = content_format::source(p, item, &rev1, content_format::Target::Chapter { side: "original".into(), chapter: "section-2".into() }, Sensitivity::Private).unwrap();
        let start = s.markdown.find("  ").unwrap();
        let classified: Vec<_> = fe::blocks(&s.markdown).unwrap().into_iter()
            .map(|block| fe::ClassifiedBlock { block, role: fe::Role::Body, source_evidence_ids: vec![] }).collect();
        let applied = fe::apply(&s.markdown, &s.identity.source_sha256, &classified, &s.identity.source_evidence_ids, &[fe::ProposedEdit {
            old_range: [start, start + 2], old_sha256: reader_sha256(b"  "), replacement: " ".into(), kind: fe::Kind::Whitespace,
            basis: fe::Basis::Deterministic, reason_code: fe::Reason::ParagraphLayout, source_evidence_ids: vec![], block_id: None }]).unwrap();
        let formatted = content_format::publish(p, s.identity.clone(), "format-other", "local_owner", &applied,
            serde_json::json!({"policy":"synthetic-v1"}), Sensitivity::Private).unwrap();
        kept(&formatted.result_revision_id, "content format");
        let restored = content_format::restore(p, "format-other", "local_owner", &formatted.changes_version,
            &formatted.result_revision_id, "restore-other", Sensitivity::Private).unwrap();
        kept(&restored.revision_id, "format restore");
        let mut plan = repair::plan(p, item, &restored.revision_id, "section-2", Sensitivity::Private).unwrap();
        let repaired = "# C\n\nThree more.\n";
        plan.original_edits = vec![repair::Edit { range: [10, 12], replacement: " ".into() }];
        plan.pairs = paragraphs::blocks(repaired).into_iter().map(|(r, kind)| Pair { o: Some(r), t: None, kind, relation: Relation::Unresolved })
            .chain(paragraphs::blocks("# 丙\n\n三改。\n").into_iter().map(|(r, kind)| Pair { o: None, t: Some(r), kind, relation: Relation::Unresolved })).collect();
        let rev4 = repair::apply(p, plan, Sensitivity::Private).unwrap().revision_id;
        assert_ne!(rev4, restored.revision_id);
        let artifacts = files();
        let inherited = kept(&rev4, "repair");
        assert_eq!(files(), artifacts, "inherited reads write nothing");
        // A corrupt or unreadable historical proof is an error, never missing.
        let old_tr = p.join("catalog/chapter-translations").join(item).join(&rev0).join(format!("{}.json", reader_sha256(b"section-0")));
        let old_al = paragraphs::path(p, &index.identity);
        for file in [&old_tr, &old_al] {
            let good = fs::read(file).unwrap();
            fs::write(file, b"{}").unwrap(); // disposable synthetic corruption only
            assert!(tr::metadata(p, item, &rev4, "section-0", Sensitivity::Private).is_err() || file == &old_al);
            assert!(paragraphs::read(p, item, &rev4, "section-1", 0, 256, "any", Sensitivity::Private).is_err() || file == &old_tr);
            fs::write(file, good).unwrap();
        }
        // CAS on the inherited version: repair plans against it, save needs it.
        assert_eq!(repair::plan(p, item, &rev4, "section-1", Sensitivity::Private).unwrap().expected_alignment_version, inherited);
        let mut next = index.clone();
        next.identity = paragraphs::identity(p, item, &rev4, "section-1", Sensitivity::Private).unwrap();
        assert!(paragraphs::save(p, "missing", next.clone(), Sensitivity::Private).is_err());
        assert!(paragraphs::save(p, &index.version().unwrap(), next.clone(), Sensitivity::Private).is_err());
        paragraphs::save(p, &inherited, next.clone(), Sensitivity::Private).unwrap();
        // A longer earlier chapter shifts the aligned chapter's start: still kept.
        let rev5 = edit(&rev4, "original", "section-0", o0, "# A\n\nOne, now longer.\n\n", "lengthen-first");
        let id5 = paragraphs::identity(p, item, &rev5, "section-1", Sensitivity::Private).unwrap();
        assert_ne!(id5.original.chapter_start, index.identity.original.chapter_start);
        let page = paragraphs::read(p, item, &rev5, "section-1", 0, 256, "any", Sensitivity::Private).unwrap();
        assert_eq!((page.status.as_str(), &page.pairs), ("aligned", &pairs));
        // Same key, other bytes: nothing; restoring the bytes later does not cross the change.
        assert!(tr::metadata(p, item, &rev5, "section-0", Sensitivity::Private).unwrap().is_none());
        let rev6 = edit(&rev5, "original", "section-0", "# A\n\nOne, now longer.\n\n", o0, "restore-first");
        assert!(tr::metadata(p, item, &rev6, "section-0", Sensitivity::Private).unwrap().is_none());
        // One side of the aligned pair changes: its index is gone.
        let rev7 = edit(&rev6, "translation", "section-0", t0, "# 乙\n\n二改。\n\n", "edit-paired-side");
        assert_eq!(paragraphs::read(p, item, &rev7, "section-1", 0, 256, "any", Sensitivity::Private).unwrap().version, "missing");
        assert!(old_tr.is_file() && old_al.is_file());
    }

    fn walkdir_count(dir: &Path) -> usize {
        fs::read_dir(dir).map(|d| d.flatten().map(|e| if e.path().is_dir() { walkdir_count(&e.path()) } else { 1 }).sum()).unwrap_or(0)
    }

    #[test]
    fn stored_alignment_rejects_split_utf8_crossing_and_stale_writes() {
        use alignment::*;
        let dir = tempfile::tempdir().unwrap();
        let (mut capture, draft) = book();
        let manifest = &mut capture.blobs[2];
        let mut value: BookManifest = serde_json::from_slice(&manifest.bytes).unwrap();
        value.editions[0].chapters[0].counterpart = Some(value.editions[1].chapters[0].key.clone());
        value.editions[1].chapters[0].counterpart = Some(value.editions[0].chapters[0].key.clone());
        manifest.bytes = serde_json::to_vec(&value).unwrap();
        manifest.descriptor.sha256 = reader_sha256(&manifest.bytes);
        manifest.descriptor.byte_size = manifest.bytes.len() as u64;
        manifest.blob_id = format!("sha256:{}", manifest.descriptor.sha256);
        capture.capture.artifacts = capture.blobs.iter().map(|b| b.descriptor.clone()).collect();
        let saved = ingest_reader_book(dir.path(), capture, draft).unwrap();
        let item = &saved.item_ids[0];
        let revision = &saved.revision_ids[0];
        let doc = reader_document(dir.path(), item, revision, Sensitivity::Private).unwrap();
        let next: BookAlignment = serde_json::from_value(serde_json::json!({
            "schema_version":1,"item_id":item,"revision_id":revision,"chapters":[{
                "schema_version":1,"alignment":"sparse_navigation_not_citations","quality":"not_certified",
                "identity":{"sampler_version":1,"sample_count":3,
                    "original_sha256":reader_sha256(b"First chapter.\n"),"translation_sha256":reader_sha256("甲🌹乙\n".as_bytes()),
                    "binding":{"item_id":item,"revision_id":revision,"canonical_sha256":doc.canonical_sha256,
                        "original":{"chapter":doc.editions[0].chapters[0].key,"chapter_start":0,"sha256":doc.editions[0].sha256},
                        "translation":{"chapter":doc.editions[1].chapters[0].key,"chapter_start":0,"sha256":doc.editions[1].sha256}}},
                "anchors":[{"original":[0,5],"translation":[0,3],"basis":"model_sample_not_full_span_verification"}]}]
        })).unwrap();
        alignment::save(dir.path(), "missing", next.clone(), Sensitivity::Private).unwrap();
        for range in [[1,3], [0,4], [0,usize::MAX]] {
            let mut bad = next.clone();
            bad.chapters[0].anchors[0].translation = range;
            assert!(alignment::save(dir.path(), &next.version().unwrap(), bad, Sensitivity::Private).is_err());
        }
        let mut crossing = next.clone();
        crossing.chapters[0].anchors.push(NavigationAnchor { original:[3,6], translation:[3,7], basis:AnchorBasis::ModelSampleNotFullSpanVerification });
        assert!(alignment::save(dir.path(), &next.version().unwrap(), crossing, Sensitivity::Private).is_err());
        assert_eq!(alignment::read(dir.path(), item, revision, Sensitivity::Private).unwrap(), Some(next.clone()));
        assert!(alignment::read(dir.path(), item, revision, Sensitivity::Public).is_err());
        let mut changed = next.clone();
        changed.chapters[0].anchors[0].translation = [0,7];
        assert!(alignment::save(dir.path(), "missing", changed.clone(), Sensitivity::Private).is_err());
        alignment::save(dir.path(), &next.version().unwrap(), changed.clone(), Sensitivity::Private).unwrap();
        assert_eq!(alignment::save(dir.path(), "missing", changed.clone(), Sensitivity::Private).unwrap(), changed);
        assert_eq!(reader_document(dir.path(), item, revision, Sensitivity::Private).unwrap().canonical_sha256, doc.canonical_sha256);
    }

    #[test]
    fn paired_book_is_one_replayable_item_with_independent_bounded_coordinates() {
        let dir = tempfile::tempdir().unwrap();
        let (capture, draft) = book();
        let saved = ingest_reader_book(dir.path(), capture.clone(), draft.clone()).unwrap();
        let replay = ingest_reader_book(dir.path(), capture, draft.clone()).unwrap();
        assert!(replay.idempotent_replay);
        assert_eq!(saved.item_ids, replay.item_ids);
        let id = &saved.item_ids[0];
        let rev = &saved.revision_ids[0];
        assert_eq!(
            read_item(dir.path(), id).unwrap().1.content_markdown,
            draft.content_markdown
        );
        let doc = reader_document(dir.path(), id, rev, Sensitivity::Private).unwrap();
        assert_eq!(doc.default_side, "translation");
        let first = reader_chunk(
            dir.path(),
            id,
            rev,
            "translation",
            "section-0",
            0,
            5,
            Sensitivity::Private,
        )
        .unwrap();
        assert_eq!(first.markdown, "甲");
        assert_eq!(first.next_offset, Some(3));
        let second = reader_chunk(
            dir.path(),
            id,
            rev,
            "translation",
            "section-0",
            3,
            4,
            Sensitivity::Private,
        )
        .unwrap();
        assert_eq!(second.markdown, "🌹");
        assert_eq!(second.start, 3);
        assert_eq!(second.end, 7);
        assert!(reader_chunk(
            dir.path(),
            id,
            rev,
            "translation",
            "section-0",
            1,
            5,
            Sensitivity::Private
        )
        .is_err());
        let en = reader_chunk(
            dir.path(),
            id,
            rev,
            "original",
            "section-0",
            0,
            20,
            Sensitivity::Private,
        )
        .unwrap();
        assert_eq!(en.markdown, "First chapter.\n");
        assert_ne!(en.evidence_id, first.evidence_id);
        assert_ne!(en.sha256, first.sha256);
        assert!(reader_document(dir.path(), id, rev, Sensitivity::Public).is_err());
        fs::write(dir.path().join("reader-blobs").join(&en.sha256), "corrupt").unwrap();
        assert!(reader_document(dir.path(), id, rev, Sensitivity::Private).is_err());
    }

    #[test]
    fn corrected_translation_keeps_identity_history_and_chapters_after_recovery() {
        for interruption in [None, Some(0), Some(1), Some(2), Some(3), Some(4)] {
            let dir = tempfile::tempdir().unwrap();
            let (mut capture, mut draft) = book();
            let receipt = ingest_reader_book(dir.path(), capture.clone(), draft.clone()).unwrap();
            let id = &receipt.item_ids[0];
            let rev = &receipt.revision_ids[0];
            let (old_item, old_revision) = read_item(dir.path(), id).unwrap();
            let old_doc = reader_document(dir.path(), id, rev, Sensitivity::Private).unwrap();
            draft.content_markdown = "正确中文🌹，完整段落。\n".into();
            let mut manifest: BookManifest = serde_json::from_slice(&capture.blobs[2].bytes).unwrap();
            manifest.editions[1] = refreshed_translation(&manifest.editions[1], &draft.content_markdown).unwrap();
            for (i, bytes) in [(1, draft.content_markdown.as_bytes().to_vec()), (2, serde_json::to_vec(&manifest).unwrap())] {
                capture.blobs[i].descriptor.sha256 = reader_sha256(&bytes);
                capture.blobs[i].descriptor.byte_size = bytes.len() as u64;
                capture.blobs[i].bytes = bytes;
            }
            FAIL_ITEM_UPDATE_STEP.with(|s| s.set(interruption));
            let result = refresh_book_translation(dir.path(), id, rev, &capture, &draft);
            FAIL_ITEM_UPDATE_STEP.with(|s| s.set(None));
            assert_eq!(result.is_err(), interruption.is_some());
            recover_pending_ingests(dir.path()).unwrap();
            let (item, next) = read_item(dir.path(), id).unwrap();
            if interruption == Some(0) {
                assert_eq!(next, old_revision);
                continue;
            }
            assert_eq!(item.item_id, old_item.item_id);
            assert_eq!(item.added_at, old_item.added_at);
            assert_eq!(next.content_markdown, draft.content_markdown);
            assert_eq!(next.evidence_ids[..old_revision.evidence_ids.len()], old_revision.evidence_ids);
            let doc = reader_document(dir.path(), id, &next.revision_id, Sensitivity::Private).unwrap();
            assert_eq!(doc.editions[0], old_doc.editions[0]);
            assert_eq!(doc.editions[1], manifest.editions[1]);
            let chunk = reader_chunk(dir.path(), id, &next.revision_id, "translation", "section-0", 0, 32768, Sensitivity::Private).unwrap();
            assert_eq!(chunk.markdown, draft.content_markdown);
            assert_eq!(chunk.evidence_id.as_ref(), next.evidence_ids.last());
            assert!(reader_document(dir.path(), id, &next.revision_id, Sensitivity::Public).is_err());
            assert!(refresh_book_translation(dir.path(), id, rev, &capture, &draft).is_err());
            assert_eq!(fs::read_dir(dir.path().join("items")).unwrap().count(), 1);
        }
    }

    #[test]
    fn format_after_translation_reconversion_still_opens_the_book() {
        // A reconversion keeps its translation in evidence/<id>/content.md; a later
        // original-side format records the same translation bytes again as a reader
        // blob. Opening the book must pick the reader-blob copy (2026-11-05 regression).
        let dir = tempfile::tempdir().unwrap();
        let (mut capture, mut draft) = book();
        let original = "First  chapter.\n";
        let mut manifest: BookManifest = serde_json::from_slice(&capture.blobs[2].bytes).unwrap();
        manifest.editions[0] = Edition { side: "original".into(), ..heading_edition(original) };
        for (i, bytes) in [(0, original.as_bytes().to_vec()), (2, serde_json::to_vec(&manifest).unwrap())] {
            capture.blobs[i].descriptor.sha256 = reader_sha256(&bytes);
            capture.blobs[i].descriptor.byte_size = bytes.len() as u64;
            capture.blobs[i].blob_id = format!("sha256:{}", reader_sha256(&bytes));
            capture.blobs[i].bytes = bytes;
        }
        capture.capture.artifacts = capture.blobs.iter().map(|b| b.descriptor.clone()).collect();
        let receipt = ingest_reader_book(dir.path(), capture.clone(), draft.clone()).unwrap();
        let id = &receipt.item_ids[0];
        draft.content_markdown = "正确中文🌹，完整段落。\n".into();
        manifest.editions[1] = refreshed_translation(&manifest.editions[1], &draft.content_markdown).unwrap();
        for (i, bytes) in [(1, draft.content_markdown.as_bytes().to_vec()), (2, serde_json::to_vec(&manifest).unwrap())] {
            capture.blobs[i].descriptor.sha256 = reader_sha256(&bytes);
            capture.blobs[i].descriptor.byte_size = bytes.len() as u64;
            capture.blobs[i].bytes = bytes;
        }
        refresh_book_translation(dir.path(), id, &receipt.revision_ids[0], &capture, &draft).unwrap();
        let reconverted = read_item(dir.path(), id).unwrap().1;
        let doc = reader_document(dir.path(), id, &reconverted.revision_id, Sensitivity::Private).unwrap();
        let s = content_format::source(
            dir.path(), id, &reconverted.revision_id,
            content_format::Target::Chapter { side: "original".into(), chapter: doc.editions[0].chapters[0].key.clone() },
            Sensitivity::Private,
        )
        .unwrap();
        let start = s.markdown.find("  ").unwrap();
        let classified: Vec<_> = content_format::edits::blocks(&s.markdown).unwrap().into_iter()
            .map(|block| content_format::edits::ClassifiedBlock { block, role: content_format::edits::Role::Body, source_evidence_ids: vec![] })
            .collect();
        let applied = content_format::edits::apply(&s.markdown, &s.identity.source_sha256, &classified, &s.identity.source_evidence_ids,
            &[content_format::edits::ProposedEdit {
                old_range: [start, start + 2], old_sha256: reader_sha256(b"  "), replacement: " ".into(),
                kind: content_format::edits::Kind::Whitespace, basis: content_format::edits::Basis::Deterministic,
                reason_code: content_format::edits::Reason::ParagraphLayout, source_evidence_ids: vec![], block_id: None,
            }]).unwrap();
        content_format::publish(dir.path(), s.identity.clone(), "format-after-reconversion", "local_owner", &applied,
            serde_json::json!({"policy":"synthetic-v1"}), Sensitivity::Private).unwrap();
        let formatted = read_item(dir.path(), id).unwrap().1;
        assert_ne!(formatted.revision_id, reconverted.revision_id);
        let doc = reader_document(dir.path(), id, &formatted.revision_id, Sensitivity::Private).unwrap();
        assert_eq!(doc.editions[1].sha256, reader_sha256(draft.content_markdown.as_bytes()));
        let zh = reader_chunk(dir.path(), id, &formatted.revision_id, "translation", "section-0", 0, 32768, Sensitivity::Private).unwrap();
        assert_eq!(zh.markdown, draft.content_markdown);
        let en = reader_chunk(dir.path(), id, &formatted.revision_id, "original", "section-0", 0, 32768, Sensitivity::Private).unwrap();
        assert_eq!(en.markdown, "First chapter.\n");
        let read = || reader_document(dir.path(), id, &formatted.revision_id, Sensitivity::Private);
        let mut rejected = Vec::new();
        for path in [dir.path().join("items").join(id).join("item.json"),
            dir.path().join("integrity/items").join(format!("{id}.json"))] {
            read().unwrap();
            let bytes = fs::read(&path).unwrap();
            fs::remove_file(&path).unwrap();
            rejected.push(("missing baseline", read().is_err()));
            fs::write(path, bytes).unwrap();
        }
        let e: Evidence = load(dir.path(), dir.path().join("evidence")
            .join(formatted.evidence_ids.last().unwrap()).join("evidence.json")).unwrap();
        let capture_path = dir.path().join("captures").join(e.capture_id).join("capture.json");
        read().unwrap();
        let bytes = fs::read(&capture_path).unwrap();
        let mut capture: CaptureRecord = serde_json::from_slice(&bytes).unwrap();
        capture.sensitivity = Sensitivity::Restricted;
        fs::write(&capture_path, serde_json::to_vec(&capture).unwrap()).unwrap();
        rejected.push(("changed capture", read().is_err()));
        fs::write(capture_path, bytes).unwrap();

        read().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let blobs = dir.path().join("reader-blobs");
        let moved = outside.path().join("blobs");
        fs::rename(&blobs, &moved).unwrap();
        std::os::unix::fs::symlink(&moved, &blobs).unwrap();
        rejected.push(("escaped blob directory", read().is_err()));
        fs::remove_file(&blobs).unwrap();
        fs::rename(moved, &blobs).unwrap();

        let blob_path = blobs.join(&zh.sha256);
        let target = dir.path().join("linked-body.md");
        fs::rename(&blob_path, &target).unwrap();
        std::os::unix::fs::symlink(&target, &blob_path).unwrap();
        read().unwrap(); // Existing contained links remain supported.
        let bytes = fs::read(&target).unwrap();
        fs::write(&target, "corrupt linked body").unwrap();
        rejected.push(("changed linked target", read().is_err()));
        fs::remove_file(&blob_path).unwrap();
        fs::write(&blob_path, bytes).unwrap();
        assert!(rejected.iter().all(|(_, rejected)| *rejected), "{rejected:?}");
        // The cached projection still notices a damaged reader blob.
        fs::write(dir.path().join("reader-blobs").join(&zh.sha256), "corrupt").unwrap();
        assert!(reader_document(dir.path(), id, &formatted.revision_id, Sensitivity::Private).is_err());
    }

    #[test]
    fn interrupted_import_recovers_one_item_and_all_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let (capture, draft) = book();
        FAIL_APPLY_STEP.with(|v| v.set(Some(0)));
        assert!(ingest_reader_book(dir.path(), capture.clone(), draft.clone()).is_err());
        FAIL_APPLY_STEP.with(|v| v.set(None));
        recover_pending_ingests(dir.path()).unwrap();
        let receipt = ingest_reader_book(dir.path(), capture, draft).unwrap();
        assert_eq!(fs::read_dir(dir.path().join("items")).unwrap().count(), 1);
        assert_eq!(
            reader_document(
                dir.path(),
                &receipt.item_ids[0],
                &receipt.revision_ids[0],
                Sensitivity::Private
            )
            .unwrap()
            .editions
            .len(),
            2
        );
    }

    #[test]
    fn ordinary_document_navigation_and_cross_device_state_preserve_cas_and_history() {
        let dir = tempfile::tempdir().unwrap();
        let mut draft = crate::tests::draft();
        draft.content_markdown = "开篇\n# 一\n甲🌹乙\n## 二\n尾声\n".into();
        let capture = crate::tests::capture();
        let receipt = ingest(dir.path(), &capture, draft).unwrap();
        let id = &receipt.item_ids[0];
        let rev = &receipt.revision_ids[0];
        let doc = reader_document(dir.path(), id, rev, Sensitivity::Public).unwrap();
        assert_eq!(doc.editions[0].chapters.len(), 3);
        let ch = &doc.editions[0].chapters[1];
        let anchor = ReaderAnchor {
            revision_id: rev.clone(),
            side: "source".into(),
            sha256: doc.editions[0].sha256.clone(),
            chapter: ch.key.clone(),
            offset: 0,
        };
        let empty = reader_state(dir.path(), id, Sensitivity::Public).unwrap();
        let next = ReaderState {
            visit_id: "mac-open-1".into(),
            position: Some(anchor.clone()),
            bookmarks: vec![Bookmark {
                id: "bookmark-1".into(),
                label: "中文书签".into(),
                anchor,
            }],
        };
        save_reader_state(
            dir.path(),
            id,
            &empty.version().unwrap(),
            next.clone(),
            Sensitivity::Public,
        )
        .unwrap();
        assert_eq!(
            reader_resume(dir.path(), Sensitivity::Public)
                .unwrap()
                .unwrap(),
            (id.clone(), next.clone())
        );
        assert_eq!(
            save_reader_state(
                dir.path(),
                id,
                &empty.version().unwrap(),
                next.clone(),
                Sensitivity::Public
            )
            .unwrap(),
            next
        );
        let mut mobile = next.clone();
        mobile.visit_id = "mobile-open-2".into();
        mobile.position.as_mut().unwrap().offset = 2;
        assert!(save_reader_state(
            dir.path(),
            id,
            &empty.version().unwrap(),
            mobile.clone(),
            Sensitivity::Public
        )
        .is_err());
        save_reader_state(
            dir.path(),
            id,
            &next.version().unwrap(),
            mobile.clone(),
            Sensitivity::Public,
        )
        .unwrap();
        assert_eq!(
            reader_state(dir.path(), id, Sensitivity::Public).unwrap(),
            mobile
        );
        let edit = MarkdownEdit {
            item_id: id.clone(),
            expected_revision_id: rev.clone(),
            request_id: "reader-edit".into(),
            markdown: "# 新版\n正文\n".into(),
        };
        let newer = edit_markdown(dir.path(), edit, Sensitivity::Public).unwrap();
        assert!(reader_document(dir.path(), id, rev, Sensitivity::Public).is_err());
        let mut stale = mobile.clone();
        stale.visit_id = "new-open".into();
        assert!(save_reader_state(
            dir.path(),
            id,
            &mobile.version().unwrap(),
            stale.clone(),
            Sensitivity::Public
        )
        .is_err());
        stale.position = None;
        save_reader_state(
            dir.path(),
            id,
            &mobile.version().unwrap(),
            stale.clone(),
            Sensitivity::Public,
        )
        .unwrap();
        assert_eq!(
            reader_state(dir.path(), id, Sensitivity::Public)
                .unwrap()
                .bookmarks[0]
                .anchor
                .revision_id,
            *rev
        );
        trash_item(dir.path(), id, &newer.revision_id, "owner", None, None).unwrap();
        assert!(reader_resume(dir.path(), Sensitivity::Public)
            .unwrap()
            .is_none());
        assert!(reader_state(dir.path(), id, Sensitivity::Public).is_err());
        assert!(save_reader_state(
            dir.path(),
            id,
            &stale.version().unwrap(),
            stale,
            Sensitivity::Public
        )
        .is_err());
    }
}
