//! Five-style marks on AI chapter translations
//! (`reader_annotations_ai_translation_v1`): an independent per-item ledger in
//! `catalog/reader-ai-annotations/<item_id>.json`. The edition ledger
//! (`reader_annotations_v1`) is never read, written or flagged from here.
//!
//! Each segment binds the effective projection identity the chapter-translation
//! metadata/chunk reads return (`translation_version` includes metadata-view
//! inheritance) plus its body hash; offsets are UTF-8 byte ranges of that chapter's
//! complete translation Markdown, never original-text or chunk-local coordinates.
use super::annotations::STYLES;
use super::chapter_translation::{Metadata, Projection};
use super::*;

pub const SPACE: &str = "ai_translation";
const MAX_MARKS: usize = 4096;
const MAX_SEGMENTS: usize = 64;
const MAX_LEDGER_BYTES: usize = 4 * 1024 * 1024;
const MAX_REQUEST_BYTES: usize = 32 * 1024;
const MAX_PAGE_BYTES: usize = 48 * 1024;
const SCHEMA: u32 = 1;
const DOMAIN: &str = "reader-ai-annotations-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Segment {
    /// The original chapter key the translation projects (`original_chapter`).
    pub chapter: String,
    pub translation_version: String,
    pub sha256: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Anchor {
    pub revision_id: String,
    /// Always `ai_translation`; never an edition side.
    pub side: String,
    pub segments: Vec<Segment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Annotation {
    pub id: String,
    pub style: String,
    pub anchor: Anchor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Change {
    Put { annotation: Annotation },
    Delete { id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditResult {
    pub item_id: String,
    pub annotation_space: String,
    pub version: String,
    pub id: String,
    /// `created`, `style_changed` or `deleted`.
    pub outcome: String,
}

/// A complete projection identity; a lone `translation_version` is never a filter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionFilter {
    pub chapter: String,
    pub translation_version: String,
    pub sha256: String,
}

/// The bound identity of one segment and whether it is still the effective one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentProjection {
    pub chapter: String,
    pub translation_version: String,
    pub sha256: String,
    pub status: String,
}

/// `status` is `stale` if the revision or ANY segment's projection changed; such a
/// mark is never drawn on any new translation segment and is delete-only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListedAnnotation {
    pub id: String,
    pub style: String,
    pub anchor: Anchor,
    pub status: String,
    pub order: u64,
    pub projections: Vec<SegmentProjection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotationPage {
    pub item_id: String,
    pub annotation_space: String,
    pub version: String,
    /// Binds the ledger version, filter, current revision, the effective projection
    /// identities referenced and the visible mark set; later pages must match it.
    pub view_version: String,
    pub annotations: Vec<ListedAnnotation>,
    pub total: usize,
    pub next_offset: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    id: String,
    style: String,
    anchor: Anchor,
    order: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    principal: String,
    expected_version: String,
    change_sha256: String,
    result: EditResult,
    /// Revision the affected mark is bound to; a replay re-authorizes it so a
    /// receipt never outlives read access to that history. Receipts written
    /// before this field (unpublished synthetic ledgers only) carry `None` and are
    /// never replayed: the ledger stays readable and the client reconciles by list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bound_revision_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    schema_version: u32,
    annotation_space: String,
    item_id: String,
    /// Strictly increasing per item, so a version never repeats (no ABA).
    sequence: u64,
    marks: Vec<Stored>,
    last: Option<Receipt>,
}

impl Ledger {
    fn empty(item: &str) -> Self {
        Self {
            schema_version: SCHEMA,
            annotation_space: SPACE.into(),
            item_id: item.into(),
            sequence: 0,
            marks: vec![],
            last: None,
        }
    }

    /// Namespace-, sequence- and content-bound: never equal to an edition-ledger
    /// version, and a pre-restore version cannot pass CAS on different marks.
    fn version(&self) -> String {
        let marks = serde_json::to_string(&self.marks).expect("marks serialise");
        reader_sha256(format!("{DOMAIN}\0{SPACE}\0{}\0{}\0{}", self.item_id, self.sequence, marks).as_bytes())
    }
}

fn path(root: &Path, item: &str) -> PathBuf {
    root.join("catalog/reader-ai-annotations").join(format!("{item}.json"))
}

fn change_sha256(change: &Change) -> Result<String> {
    let mut bytes = format!("{DOMAIN}\0{SPACE}\0").into_bytes();
    bytes.extend(serde_json::to_vec(change)?);
    Ok(reader_sha256(&bytes))
}

/// Missing file is the empty ledger; anything unreadable or unsupported fails closed.
fn ledger(root: &Path, item: &str) -> Result<Ledger> {
    let p = path(root, item);
    match fs::symlink_metadata(&p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Ledger::empty(item)),
        Err(e) => return Err(e.into()),
        Ok(m) if m.len() as usize > MAX_LEDGER_BYTES + 64 * 1024 => anyhow::bail!("reader_annotations_unavailable"),
        Ok(_) => {}
    }
    let l: Ledger = load(root, p).map_err(|_| anyhow::anyhow!("reader_annotations_unavailable"))?;
    anyhow::ensure!(
        l.schema_version == SCHEMA && l.annotation_space == SPACE && l.item_id == item,
        "reader_annotations_unavailable"
    );
    Ok(l)
}

/// Item visible at `ceiling` and its current reader projection, or not found.
fn current(root: &Path, item: &str, ceiling: Sensitivity) -> Result<(ReaderDocument, ReaderBodies)> {
    let (_, revision) = read_item_at_sensitivity(root, item, ceiling)?.context("library_resource_not_found")?;
    document_revision_locked(root, &revision, ceiling)
}

/// Stale-mark access needs the current item AND its bound historical source to be
/// readable at this ceiling; never weaker than existing history access.
pub(super) fn authorize_bound(root: &Path, item: &str, revision: &str, current_revision: &str, ceiling: Sensitivity) -> Result<()> {
    if revision == current_revision {
        return Ok(());
    }
    // The Store cannot narrow history access while the current revision stays
    // readable (current evidence always includes historical evidence), so tests
    // simulate a grant that narrows history only.
    #[cfg(test)]
    anyhow::ensure!(!tests::hidden_history(revision), "library_resource_not_found");
    let old = revision_text::historical_revision(root, item, revision, ceiling)
        .map_err(|_| anyhow::anyhow!("library_resource_not_found"))?;
    document_revision_locked(root, &old, ceiling).map_err(|_| anyhow::anyhow!("library_resource_not_found"))?;
    Ok(())
}

enum Resolved {
    /// The chapter is not an AI-translatable original chapter of this revision.
    NotSurface,
    /// No translation is published or inheritable.
    Missing,
    Present(Box<(Projection, Metadata)>),
}

/// Effective projections of the CURRENT revision, resolved once per chapter through
/// the chapter-translation resolver (access, source integrity, full projection
/// validation, metadata-view inheritance). Corruption propagates; it is never
/// reported as "no translation".
struct Projections<'a> {
    root: &'a Path,
    doc: &'a ReaderDocument,
    bodies: &'a ReaderBodies,
    ceiling: Sensitivity,
    cache: BTreeMap<String, Resolved>,
}

impl<'a> Projections<'a> {
    fn new(root: &'a Path, doc: &'a ReaderDocument, bodies: &'a ReaderBodies, ceiling: Sensitivity) -> Self {
        Self { root, doc, bodies, ceiling, cache: BTreeMap::new() }
    }

    fn get(&mut self, chapter: &str) -> Result<&Resolved> {
        if !self.cache.contains_key(chapter) {
            let doc = self.doc;
            let resolved = match chapter_translation::source_from(doc, self.bodies, &doc.item_id, &doc.revision_id, chapter) {
                None => Resolved::NotSurface,
                Some(source) => match chapter_translation::effective(self.root, &source?, self.ceiling)? {
                    None => Resolved::Missing,
                    Some((p, m)) => {
                        anyhow::ensure!(
                            m.bytes == p.markdown.len() && m.sha256 == reader_sha256(p.markdown.as_bytes()),
                            "invalid_reader_translation_request"
                        );
                        Resolved::Present(Box::new((p, m)))
                    }
                },
            };
            self.cache.insert(chapter.into(), resolved);
        }
        Ok(&self.cache[chapter])
    }

    /// The segment binds the current effective projection: the same body hash and either the
    /// current view version, or the view version the same projection had on a revision whose
    /// body is the current body (a mark made before a title or other metadata edit).
    fn segment_current(&mut self, s: &Segment) -> Result<bool> {
        let (root, item, current) = (self.root, self.doc.item_id.clone(), self.doc.revision_id.clone());
        let Resolved::Present(pm) = self.get(&s.chapter)? else { return Ok(false) };
        if pm.1.sha256 != s.sha256 {
            return Ok(false);
        }
        if pm.1.version == s.translation_version {
            return Ok(true);
        }
        let base = pm.0.metadata()?.version;
        let source = pm.0.identity.revision_id.clone();
        let mut views = vec![source.clone()];
        views.extend(crate::body::same_body_revisions(root, &item, &current)?.into_iter().map(|r| r.revision_id));
        for view in views {
            if super::metadata_view::version("chapter_translation", &item, &view, &source, &base)? == s.translation_version {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Per-segment status; a revision with another body makes every segment stale unresolved.
    fn statuses(&mut self, anchor: &Anchor) -> Result<Vec<bool>> {
        if !crate::body::same_body(self.root, &self.doc.item_id, &self.doc.revision_id, &anchor.revision_id) {
            return Ok(vec![false; anchor.segments.len()]);
        }
        anchor.segments.iter().map(|s| self.segment_current(s)).collect()
    }

    /// Effective identities of every chapter resolved so far, for the view version.
    fn identities(&self) -> BTreeMap<&str, Option<(&str, &str)>> {
        self.cache.iter().map(|(k, r)| (k.as_str(), match r {
            Resolved::Present(pm) => Some((pm.1.version.as_str(), pm.1.sha256.as_str())),
            _ => None,
        })).collect()
    }

    /// Every segment binds the current effective version and hash of its chapter,
    /// lies on UTF-8 boundaries inside the complete translation, toc-ordered and
    /// disjoint. Version/hash drift is the determinate `stale_reader_translation`.
    fn validate(&mut self, anchor: &Anchor) -> Result<()> {
        anyhow::ensure!(anchor.revision_id == self.doc.revision_id, "stale_item_revision");
        let toc = &self
            .doc
            .editions
            .iter()
            .find(|e| e.side == "original")
            .context("invalid_reader_anchor")?
            .chapters;
        let mut last: Option<(usize, usize)> = None;
        for s in &anchor.segments {
            let index = toc.iter().position(|c| c.key == s.chapter).context("invalid_reader_anchor")?;
            let (p, m) = match self.get(&s.chapter)? {
                Resolved::NotSurface => anyhow::bail!("invalid_reader_anchor"),
                Resolved::Missing => anyhow::bail!("stale_reader_translation"),
                Resolved::Present(pm) => (&pm.0, &pm.1),
            };
            anyhow::ensure!(m.version == s.translation_version && m.sha256 == s.sha256, "stale_reader_translation");
            anyhow::ensure!(
                s.end <= p.markdown.len() && p.markdown.is_char_boundary(s.start) && p.markdown.is_char_boundary(s.end),
                "invalid_reader_anchor"
            );
            if let Some((li, lend)) = last {
                anyhow::ensure!(index > li || (index == li && s.start >= lend), "invalid_reader_anchor");
            }
            last = Some((index, s.end));
        }
        Ok(())
    }
}

fn valid_chapter(chapter: &str) -> bool {
    !chapter.is_empty() && chapter.len() <= 128
}

fn valid_shape(a: &Annotation) -> bool {
    valid_request_id(&a.id)
        && STYLES.contains(&a.style.as_str())
        && valid_revision_id(&a.anchor.revision_id)
        && a.anchor.side == SPACE
        && (1..=MAX_SEGMENTS).contains(&a.anchor.segments.len())
        && a.anchor.segments.iter().all(|s| {
            s.start < s.end
                && valid_chapter(&s.chapter)
                && valid_lower_hex_64(&s.translation_version)
                && valid_lower_hex_64(&s.sha256)
        })
}

#[allow(clippy::too_many_arguments)]
pub fn list(
    library: &Path,
    item: &str,
    filter: Option<&ProjectionFilter>,
    offset: usize,
    limit: usize,
    expected_version: Option<&str>,
    expected_view_version: Option<&str>,
    ceiling: Sensitivity,
) -> Result<AnnotationPage> {
    anyhow::ensure!(
        valid_item_id(item)
            && (1..=100).contains(&limit)
            && (offset == 0 || (expected_version.is_some() && expected_view_version.is_some()))
            && expected_version.is_none_or(valid_lower_hex_64)
            && expected_view_version.is_none_or(valid_lower_hex_64)
            && filter.is_none_or(|f| valid_chapter(&f.chapter)
                && valid_lower_hex_64(&f.translation_version)
                && valid_lower_hex_64(&f.sha256)),
        "invalid_reader_annotation"
    );
    with_consistent_snapshot(library, |root| {
        let (doc, bodies) = current(root, item, ceiling)?;
        let l = ledger(root, item)?;
        let version = l.version();
        if let Some(expected) = expected_version {
            anyhow::ensure!(expected == version, "stale_reader_annotations");
        }
        let mut projections = Projections::new(root, &doc, &bodies, ceiling);
        // Stale marks bound to history this ceiling cannot read are not disclosed.
        let mut readable: BTreeMap<&str, bool> = BTreeMap::new();
        let mut visible = Vec::new();
        for m in &l.marks {
            let rev = m.anchor.revision_id.as_str();
            let ok = match readable.get(rev) {
                Some(ok) => *ok,
                None => {
                    let ok = authorize_bound(root, item, rev, &doc.revision_id, ceiling).is_ok();
                    readable.insert(rev, ok);
                    ok
                }
            };
            let matches = filter.is_none_or(|f| m.anchor.segments.iter().any(|s| {
                s.chapter == f.chapter && s.translation_version == f.translation_version && s.sha256 == f.sha256
            }));
            if !(ok && matches) {
                continue;
            }
            let statuses = projections.statuses(&m.anchor)?;
            visible.push(ListedAnnotation {
                id: m.id.clone(),
                style: m.style.clone(),
                anchor: m.anchor.clone(),
                status: if statuses.iter().all(|c| *c) { "current" } else { "stale" }.into(),
                order: m.order,
                projections: m.anchor.segments.iter().zip(&statuses).map(|(s, c)| SegmentProjection {
                    chapter: s.chapter.clone(),
                    translation_version: s.translation_version.clone(),
                    sha256: s.sha256.clone(),
                    status: if *c { "current" } else { "stale" }.into(),
                }).collect(),
            });
        }
        let view_version = reader_sha256(&serde_json::to_vec(&(
            "reader-ai-annotations-view-v1",
            item,
            &doc.revision_id,
            &version,
            filter,
            projections.identities(),
            &visible,
        ))?);
        if let Some(expected) = expected_view_version {
            anyhow::ensure!(expected == view_version, "stale_reader_annotations");
        }
        let total = visible.len();
        let mut annotations = Vec::new();
        let mut bytes = 512;
        let mut next = offset;
        for listed in visible.into_iter().skip(offset).take(limit) {
            let size = serde_json::to_vec(&listed)?.len() + 1;
            if !annotations.is_empty() && bytes + size > MAX_PAGE_BYTES {
                break;
            }
            bytes += size;
            annotations.push(listed);
            next += 1;
        }
        Ok(AnnotationPage {
            item_id: item.into(),
            annotation_space: SPACE.into(),
            version,
            view_version,
            annotations,
            total,
            next_offset: (next < total).then_some(next),
        })
    })
}

pub fn edit(
    library: &Path,
    item: &str,
    expected_version: &str,
    change: Change,
    principal: &str,
    ceiling: Sensitivity,
) -> Result<EditResult> {
    let shape_ok = match &change {
        Change::Put { annotation } => valid_shape(annotation),
        Change::Delete { id } => valid_request_id(id),
    };
    anyhow::ensure!(
        valid_item_id(item)
            && valid_lower_hex_64(expected_version)
            && shape_ok
            && !principal.is_empty()
            && serde_json::to_vec(&change)?.len() <= MAX_REQUEST_BYTES,
        "invalid_reader_annotation"
    );
    let change_sha256 = change_sha256(&change)?;
    let root = existing_canonical_root(library)?;
    let lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    let (doc, bodies) = current(&root, item, ceiling)?;
    let mut l = ledger(&root, item)?;
    if l.version() != expected_version {
        // Only the immediately preceding exact attempt is replayed; anything older is stale.
        if let Some(r) = &l.last {
            // A replay discloses the receipt only while the current item (above) AND the
            // mark's bound history are readable; otherwise it is indistinguishable from
            // any other stale attempt. No body is needed and nothing is written.
            if r.principal == principal && r.expected_version == expected_version && r.change_sha256 == change_sha256
                && r.bound_revision_id.as_deref().is_some_and(|rev| authorize_bound(&root, item, rev, &doc.revision_id, ceiling).is_ok())
            {
                return Ok(r.result.clone());
            }
        }
        anyhow::bail!("stale_reader_annotations")
    }
    let mut projections = Projections::new(&root, &doc, &bodies, ceiling);
    let order = l.sequence + 1;
    let (id, outcome, bound) = match change {
        Change::Put { annotation } => match l.marks.iter().position(|m| m.id == annotation.id) {
            Some(i) => {
                let existing = &l.marks[i];
                anyhow::ensure!(existing.anchor == annotation.anchor, "reader_annotation_conflict");
                anyhow::ensure!(existing.style != annotation.style, "reader_annotation_conflict");
                // Stale AI marks are delete-only in this increment.
                projections.validate(&existing.anchor)?;
                anyhow::ensure!(
                    !l.marks.iter().any(|m| m.anchor == annotation.anchor && m.style == annotation.style),
                    "reader_annotation_conflict"
                );
                l.marks[i].style = annotation.style;
                l.marks[i].order = order;
                (annotation.id, "style_changed", annotation.anchor.revision_id)
            }
            None => {
                projections.validate(&annotation.anchor)?;
                anyhow::ensure!(
                    !l.marks.iter().any(|m| m.anchor == annotation.anchor && m.style == annotation.style),
                    "reader_annotation_conflict"
                );
                anyhow::ensure!(l.marks.len() < MAX_MARKS, "reader_annotation_limit_exceeded");
                let (id, bound) = (annotation.id.clone(), annotation.anchor.revision_id.clone());
                l.marks.push(Stored { id: annotation.id, style: annotation.style, anchor: annotation.anchor, order });
                l.marks.sort_by(|a, b| a.id.cmp(&b.id));
                (id, "created", bound)
            }
        },
        Change::Delete { id } => {
            // Deletion needs no translation body: a missing old projection is fine,
            // but the current item and bound history must still be authorized.
            let i = l.marks.iter().position(|m| m.id == id).context("reader_annotation_not_found")?;
            authorize_bound(&root, item, &l.marks[i].anchor.revision_id, &doc.revision_id, ceiling)?;
            let removed = l.marks.remove(i);
            (id, "deleted", removed.anchor.revision_id)
        }
    };
    l.sequence = order;
    let result = EditResult {
        item_id: item.into(),
        annotation_space: SPACE.into(),
        version: l.version(),
        id,
        outcome: outcome.into(),
    };
    l.last = Some(Receipt {
        principal: principal.into(),
        expected_version: expected_version.into(),
        change_sha256,
        result: result.clone(),
        bound_revision_id: Some(bound),
    });
    let bytes = serde_json::to_vec(&l)?;
    anyhow::ensure!(bytes.len() <= MAX_LEDGER_BYTES || outcome == "deleted", "reader_annotation_limit_exceeded");
    ensure_contained_dir(&root, Path::new("catalog/reader-ai-annotations"))?;
    durable_replace(&root, &path(&root, item), &bytes)?;
    lock.unlock()?;
    Ok(result)
}

#[cfg(test)]
pub(crate) mod tests;
