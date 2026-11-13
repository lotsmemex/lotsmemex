//! Five-style marks on a completed whole-document AI translation of an ordinary item
//! (`reader_annotations_ai_document_v1`; `annotation_space=ai_translation`,
//! `projection_kind=async_document`). Independent per-item ledger in
//! `catalog/reader-ai-document-annotations/<item_id>.json`; the chapter AI ledger and
//! the edition ledger are never read, written or flagged from here.
//!
//! Dependency direction: `memex-analysis` depends on this crate, so the Store never
//! calls Analysis. The caller supplies a [`Resolver`] implemented with the Analysis
//! validator; the Store invokes it INSIDE its own consistent boundary (shared lock
//! for list, exclusive writer lock for edit, together with item/history checks and
//! ledger CAS) and independently re-checks that the returned record binds this exact
//! item revision and body. Validation results are never accepted from a client.
use super::annotations::STYLES;
use super::annotations_ai::{authorize_bound, SPACE};
use super::*;

pub const KIND: &str = "async_document";
const VERSION_DOMAIN: &str = "memex.async-document-annotation.v1";
const DOMAIN: &str = "reader-ai-document-annotations-v1";
const MAX_MARKS: usize = 4096;
const MAX_SEGMENTS: usize = 64;
const MAX_LEDGER_BYTES: usize = 4 * 1024 * 1024;
const MAX_REQUEST_BYTES: usize = 32 * 1024;
const MAX_PAGE_BYTES: usize = 48 * 1024;
const SCHEMA: u32 = 1;

/// A completed, validated `DocumentTranslation` Analysis record for one item revision.
/// `record_json` must be exactly `serde_json::to_vec(&record)` of the validated record.
pub struct VerifiedDocument {
    pub analysis_id: String,
    pub record_json: Vec<u8>,
    pub body_markdown: String,
}

/// Implemented by the caller over the Analysis validator. Called under a Store lock,
/// so implementations must not take Store locks themselves (no
/// `with_consistent_snapshot`/`read_translation_projection`), only lock-free reads.
pub trait Resolver {
    /// The effective completed document translation of `revision` (the current
    /// revision) visible at `ceiling`; `None` when absent/incomplete/not visible.
    /// Corruption is an error, never `None`.
    fn current(&self, root: &Path, item: &str, revision: &str, ceiling: Sensitivity) -> Result<Option<VerifiedDocument>>;
    /// Whether the named Analysis record exists and is readable at `ceiling`.
    fn record_visible(&self, root: &Path, analysis_id: &str, ceiling: Sensitivity) -> Result<bool>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Segment {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Anchor {
    pub revision_id: String,
    pub side: String,
    pub projection_kind: String,
    pub projection_id: String,
    pub translation_version: String,
    pub sha256: String,
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
    pub projection_kind: String,
    pub version: String,
    pub id: String,
    /// `created`, `style_changed` or `deleted`.
    pub outcome: String,
}

/// Exposed beside the existing translation page; clients echo it, never rebuild it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub projection_kind: String,
    pub projection_id: String,
    pub translation_version: String,
    pub sha256: String,
}

/// A complete identity; there is no filter by version or hash alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionFilter {
    pub projection_id: String,
    pub translation_version: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListedAnnotation {
    pub id: String,
    pub style: String,
    pub anchor: Anchor,
    /// `stale` when the revision or the effective document identity changed.
    pub status: String,
    pub order: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotationPage {
    pub item_id: String,
    pub annotation_space: String,
    pub projection_kind: String,
    pub version: String,
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
    /// Source revision and Analysis record the affected mark is bound to; replay
    /// re-authorizes both (also after deletion).
    bound_revision_id: String,
    bound_projection_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    schema_version: u32,
    annotation_space: String,
    projection_kind: String,
    item_id: String,
    sequence: u64,
    marks: Vec<Stored>,
    last: Option<Receipt>,
}

impl Ledger {
    fn empty(item: &str) -> Self {
        Self {
            schema_version: SCHEMA,
            annotation_space: SPACE.into(),
            projection_kind: KIND.into(),
            item_id: item.into(),
            sequence: 0,
            marks: vec![],
            last: None,
        }
    }
    fn version(&self) -> String {
        let marks = serde_json::to_string(&self.marks).expect("marks serialise");
        reader_sha256(format!("{DOMAIN}\0{SPACE}\0{KIND}\0{}\0{}\0{}", self.item_id, self.sequence, marks).as_bytes())
    }
}

fn path(root: &Path, item: &str) -> PathBuf {
    root.join("catalog/reader-ai-document-annotations").join(format!("{item}.json"))
}

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
        l.schema_version == SCHEMA && l.annotation_space == SPACE && l.projection_kind == KIND && l.item_id == item,
        "reader_annotations_unavailable"
    );
    Ok(l)
}

/// The version formula frozen in `memex.async_translation.md`: lowercase SHA256 of
/// the compact JSON string array (serde_json emits compact output).
pub fn translation_version(item: &str, revision: &str, projection_id: &str, record_json: &[u8], body: &str) -> Result<String> {
    Ok(reader_sha256(&serde_json::to_vec(&[
        VERSION_DOMAIN,
        item,
        revision,
        projection_id,
        &reader_sha256(record_json),
        &reader_sha256(body.as_bytes()),
    ])?))
}

/// Independent Store-side check that a resolver result is a DocumentTranslation
/// record bound to exactly this item revision and body; then its identity.
fn identity_of(d: &VerifiedDocument, revision: &ItemRevision) -> Result<Identity> {
    let v: serde_json::Value =
        serde_json::from_slice(&d.record_json).map_err(|_| anyhow::anyhow!("invalid_reader_translation_request"))?;
    let revision_sha256 = format!("sha256:{}", reader_sha256(&serde_json::to_vec(revision)?));
    let bound = v["inputs"].as_array().is_some_and(|inputs| {
        inputs.iter().any(|i| {
            i["item_id"] == revision.item_id.as_str()
                && i["revision_id"] == revision.revision_id.as_str()
                && i["revision_sha256"] == revision_sha256.as_str()
                && i["evidence_ids"] == serde_json::json!(revision.evidence_ids)
        })
    });
    anyhow::ensure!(
        valid_projection_id(&d.analysis_id)
            && v["analysis_id"] == d.analysis_id.as_str()
            && v["body_markdown"] == d.body_markdown.as_str()
            && v["generated_text"]["purpose"] == "document_translation"
            && bound
            && !d.body_markdown.is_empty()
            && d.body_markdown.len() <= MAX_TEXT,
        "invalid_reader_translation_request"
    );
    Ok(Identity {
        projection_kind: KIND.into(),
        projection_id: d.analysis_id.clone(),
        translation_version: translation_version(&revision.item_id, &revision.revision_id, &d.analysis_id, &d.record_json, &d.body_markdown)?,
        sha256: reader_sha256(d.body_markdown.as_bytes()),
    })
}

/// The current effective identity of `item` for the translation page (read path).
pub fn identity(library: &Path, item: &str, ceiling: Sensitivity, resolver: &dyn Resolver) -> Result<Option<Identity>> {
    anyhow::ensure!(valid_item_id(item), "invalid_reader_annotation");
    with_consistent_snapshot(library, |root| {
        let revision = current(root, item, ceiling)?;
        Ok(match resolver.current(root, item, &revision.revision_id, ceiling)? {
            Some(d) => Some(identity_of(&d, &revision)?),
            None => None,
        })
    })
}

fn current(root: &Path, item: &str, ceiling: Sensitivity) -> Result<ItemRevision> {
    let (item_header, revision) =
        read_item_at_sensitivity(root, item, ceiling)?.context("library_resource_not_found")?;
    anyhow::ensure!(item_header.sensitivity.max(revision.sensitivity) <= ceiling, "library_resource_not_found");
    Ok(revision)
}

fn valid_projection_id(id: &str) -> bool {
    id.len() == 73 && id.starts_with("analysis_") && id[9..].bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn valid_shape(a: &Annotation) -> bool {
    valid_request_id(&a.id)
        && STYLES.contains(&a.style.as_str())
        && valid_revision_id(&a.anchor.revision_id)
        && a.anchor.side == SPACE
        && a.anchor.projection_kind == KIND
        && valid_projection_id(&a.anchor.projection_id)
        && valid_lower_hex_64(&a.anchor.translation_version)
        && valid_lower_hex_64(&a.anchor.sha256)
        && (1..=MAX_SEGMENTS).contains(&a.anchor.segments.len())
        && a.anchor.segments.iter().all(|s| s.start < s.end)
}

/// Current effective document of the current revision, resolved once per call.
struct Effective {
    revision: ItemRevision,
    doc: Option<(Identity, String)>,
}

impl Effective {
    fn resolve(root: &Path, item: &str, ceiling: Sensitivity, resolver: &dyn Resolver) -> Result<Self> {
        let revision = current(root, item, ceiling)?;
        let doc = match resolver.current(root, item, &revision.revision_id, ceiling)? {
            Some(d) => Some((identity_of(&d, &revision)?, d.body_markdown)),
            None => None,
        };
        Ok(Self { revision, doc })
    }
    fn is_current(&self, a: &Anchor) -> bool {
        a.revision_id == self.revision.revision_id
            && self.doc.as_ref().is_some_and(|(id, _)| {
                id.projection_id == a.projection_id && id.translation_version == a.translation_version && id.sha256 == a.sha256
            })
    }
    /// Identity drift is the determinate `stale_reader_translation`; ranges are
    /// nonempty, ordered, disjoint UTF-8 ranges of the complete body.
    fn validate(&self, a: &Anchor) -> Result<()> {
        anyhow::ensure!(a.revision_id == self.revision.revision_id, "stale_item_revision");
        anyhow::ensure!(self.is_current(a), "stale_reader_translation");
        let body = &self.doc.as_ref().unwrap().1;
        let mut last = 0;
        for s in &a.segments {
            anyhow::ensure!(
                s.start >= last && s.end <= body.len() && body.is_char_boundary(s.start) && body.is_char_boundary(s.end),
                "invalid_reader_anchor"
            );
            last = s.end;
        }
        Ok(())
    }
}

/// Bound source history and bound Analysis record are both readable at `ceiling`.
fn authorized(root: &Path, item: &str, a_rev: &str, a_pid: &str, current_rev: &str, ceiling: Sensitivity, resolver: &dyn Resolver) -> Result<bool> {
    if authorize_bound(root, item, a_rev, current_rev, ceiling).is_err() {
        return Ok(false);
    }
    resolver.record_visible(root, a_pid, ceiling)
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
    resolver: &dyn Resolver,
) -> Result<AnnotationPage> {
    anyhow::ensure!(
        valid_item_id(item)
            && (1..=100).contains(&limit)
            && expected_version.is_some() == expected_view_version.is_some()
            && (offset == 0 || expected_version.is_some())
            && expected_version.is_none_or(valid_lower_hex_64)
            && expected_view_version.is_none_or(valid_lower_hex_64)
            && filter.is_none_or(|f| valid_projection_id(&f.projection_id)
                && valid_lower_hex_64(&f.translation_version)
                && valid_lower_hex_64(&f.sha256)),
        "invalid_reader_annotation"
    );
    with_consistent_snapshot(library, |root| {
        let eff = Effective::resolve(root, item, ceiling, resolver)?;
        let l = ledger(root, item)?;
        let version = l.version();
        if let Some(expected) = expected_version {
            anyhow::ensure!(expected == version, "stale_reader_annotations");
        }
        // Hidden history or Analysis records contribute neither marks nor counts.
        let mut readable: BTreeMap<(&str, &str), bool> = BTreeMap::new();
        let mut visible = Vec::new();
        for m in &l.marks {
            let key = (m.anchor.revision_id.as_str(), m.anchor.projection_id.as_str());
            let ok = match readable.get(&key) {
                Some(ok) => *ok,
                None => {
                    let ok = authorized(root, item, key.0, key.1, &eff.revision.revision_id, ceiling, resolver)?;
                    readable.insert(key, ok);
                    ok
                }
            };
            let matches = filter.is_none_or(|f| {
                m.anchor.projection_id == f.projection_id && m.anchor.translation_version == f.translation_version && m.anchor.sha256 == f.sha256
            });
            if ok && matches {
                visible.push(ListedAnnotation {
                    id: m.id.clone(),
                    style: m.style.clone(),
                    anchor: m.anchor.clone(),
                    status: if eff.is_current(&m.anchor) { "current" } else { "stale" }.into(),
                    order: m.order,
                });
            }
        }
        let view_version = reader_sha256(&serde_json::to_vec(&(
            "reader-ai-document-annotations-view-v1",
            item,
            &eff.revision.revision_id,
            &version,
            filter,
            eff.doc.as_ref().map(|(id, _)| id),
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
            projection_kind: KIND.into(),
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
    resolver: &dyn Resolver,
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
    let mut hashed = format!("{DOMAIN}\0{SPACE}\0{KIND}\0").into_bytes();
    hashed.extend(serde_json::to_vec(&change)?);
    let change_sha256 = reader_sha256(&hashed);
    let root = existing_canonical_root(library)?;
    let lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    // Item, history, effective Analysis identity and CAS: one writer boundary.
    let eff = Effective::resolve(&root, item, ceiling, resolver)?;
    let current_rev = eff.revision.revision_id.clone();
    let mut l = ledger(&root, item)?;
    if l.version() != expected_version {
        if let Some(r) = &l.last {
            if r.principal == principal
                && r.expected_version == expected_version
                && r.change_sha256 == change_sha256
                && authorized(&root, item, &r.bound_revision_id, &r.bound_projection_id, &current_rev, ceiling, resolver)?
            {
                return Ok(r.result.clone());
            }
        }
        anyhow::bail!("stale_reader_annotations")
    }
    let order = l.sequence + 1;
    let (id, outcome, bound) = match change {
        Change::Put { annotation } => match l.marks.iter().position(|m| m.id == annotation.id) {
            Some(i) => {
                let existing = &l.marks[i];
                anyhow::ensure!(existing.anchor == annotation.anchor, "reader_annotation_conflict");
                anyhow::ensure!(existing.style != annotation.style, "reader_annotation_conflict");
                // Stale marks are delete-only.
                eff.validate(&existing.anchor)?;
                anyhow::ensure!(
                    !l.marks.iter().any(|m| m.anchor == annotation.anchor && m.style == annotation.style),
                    "reader_annotation_conflict"
                );
                l.marks[i].style = annotation.style;
                l.marks[i].order = order;
                let a = &l.marks[i].anchor;
                (annotation.id, "style_changed", (a.revision_id.clone(), a.projection_id.clone()))
            }
            None => {
                eff.validate(&annotation.anchor)?;
                anyhow::ensure!(
                    !l.marks.iter().any(|m| m.anchor == annotation.anchor && m.style == annotation.style),
                    "reader_annotation_conflict"
                );
                anyhow::ensure!(l.marks.len() < MAX_MARKS, "reader_annotation_limit_exceeded");
                let bound = (annotation.anchor.revision_id.clone(), annotation.anchor.projection_id.clone());
                let id = annotation.id.clone();
                l.marks.push(Stored { id: annotation.id, style: annotation.style, anchor: annotation.anchor, order });
                l.marks.sort_by(|a, b| a.id.cmp(&b.id));
                (id, "created", bound)
            }
        },
        Change::Delete { id } => {
            let i = l.marks.iter().position(|m| m.id == id).context("reader_annotation_not_found")?;
            let a = &l.marks[i].anchor;
            anyhow::ensure!(
                authorized(&root, item, &a.revision_id, &a.projection_id, &current_rev, ceiling, resolver)?,
                "library_resource_not_found"
            );
            let removed = l.marks.remove(i);
            (id, "deleted", (removed.anchor.revision_id, removed.anchor.projection_id))
        }
    };
    l.sequence = order;
    let result = EditResult {
        item_id: item.into(),
        annotation_space: SPACE.into(),
        projection_kind: KIND.into(),
        version: l.version(),
        id,
        outcome: outcome.into(),
    };
    l.last = Some(Receipt {
        principal: principal.into(),
        expected_version: expected_version.into(),
        change_sha256,
        result: result.clone(),
        bound_revision_id: bound.0,
        bound_projection_id: bound.1,
    });
    let bytes = serde_json::to_vec(&l)?;
    anyhow::ensure!(bytes.len() <= MAX_LEDGER_BYTES || outcome == "deleted", "reader_annotation_limit_exceeded");
    ensure_contained_dir(&root, Path::new("catalog/reader-ai-document-annotations"))?;
    durable_replace(&root, &path(&root, item), &bytes)?;
    lock.unlock()?;
    Ok(result)
}

#[cfg(test)]
mod tests;
