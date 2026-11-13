//! Manual five-style reader marks (`reader_annotations_v1`): a per-item ledger in
//! `catalog/reader-annotations/<item_id>.json`, separate from reading position,
//! bookmarks, the legacy `reading_annotations_v1` underlines and the item body.
use super::*;

pub const STYLES: [&str; 5] = ["bold", "red", "blue", "underline", "yellow"];
const MAX_MARKS: usize = 4096;
const MAX_SEGMENTS: usize = 64;
const MAX_LEDGER_BYTES: usize = 4 * 1024 * 1024;
const MAX_PAGE_BYTES: usize = 48 * 1024;
const SCHEMA: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Segment {
    pub chapter: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnnotationAnchor {
    pub revision_id: String,
    pub side: String,
    pub sha256: String,
    pub segments: Vec<Segment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Annotation {
    pub id: String,
    pub style: String,
    pub anchor: AnnotationAnchor,
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
    pub version: String,
    pub id: String,
    /// `created`, `style_changed` or `deleted`.
    pub outcome: String,
}

/// A returned mark. `status` (`current`/`stale`) and `order` (latest server-accepted
/// mutation order, used for red/blue precedence) are derived, never client input.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListedAnnotation {
    pub id: String,
    pub style: String,
    pub anchor: AnnotationAnchor,
    pub status: String,
    pub order: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotationPage {
    pub item_id: String,
    pub version: String,
    pub annotations: Vec<ListedAnnotation>,
    pub total: usize,
    pub next_offset: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    id: String,
    style: String,
    anchor: AnnotationAnchor,
    order: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    principal: String,
    expected_version: String,
    change_sha256: String,
    result: EditResult,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    schema_version: u32,
    item_id: String,
    /// Strictly increasing per item, so a version never repeats (no ABA) even when a
    /// put/delete returns the mark set to an earlier content.
    sequence: u64,
    marks: Vec<Stored>,
    last: Option<Receipt>,
}

impl Ledger {
    /// The sequence alone repeats after a library is restored from an older backup;
    /// binding the mark set too keeps a pre-restore version from passing CAS on
    /// different content.
    fn version(&self) -> String {
        let marks = serde_json::to_string(&self.marks).expect("marks serialise");
        reader_sha256(format!("reader-annotations-v1\0{}\0{}\0{}", self.item_id, self.sequence, marks).as_bytes())
    }
}

fn path(root: &Path, item: &str) -> PathBuf {
    root.join("catalog/reader-annotations").join(format!("{item}.json"))
}

/// Missing file is the empty ledger; anything unreadable or unsupported fails closed.
fn ledger(root: &Path, item: &str) -> Result<Ledger> {
    let p = path(root, item);
    match fs::symlink_metadata(&p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Ledger { schema_version: SCHEMA, item_id: item.into(), ..Default::default() })
        }
        Err(e) => return Err(e.into()),
        Ok(m) if m.len() as usize > MAX_LEDGER_BYTES + 64 * 1024 => anyhow::bail!("reader_annotations_unavailable"),
        Ok(_) => {}
    }
    let l: Ledger = load(root, p).map_err(|_| anyhow::anyhow!("reader_annotations_unavailable"))?;
    anyhow::ensure!(l.schema_version == SCHEMA && l.item_id == item, "reader_annotations_unavailable");
    Ok(l)
}

/// Item visible at `ceiling` and current reader projection, or not found.
fn current(root: &Path, item: &str, ceiling: Sensitivity) -> Result<(ItemRevision, ReaderDocument, ReaderBodies)> {
    let (_, revision) =
        read_item_at_sensitivity(root, item, ceiling)?.context("library_resource_not_found")?;
    let (doc, bodies) = document_revision_locked(root, &revision, ceiling)?;
    Ok((revision, doc, bodies))
}

/// A mark of a revision whose body is the current body (only metadata edited since) is
/// current: its edition hash and offsets are those of the current edition.
fn is_current(root: &Path, anchor: &AnnotationAnchor, doc: &ReaderDocument) -> bool {
    doc.editions.iter().any(|e| e.side == anchor.side && e.sha256 == anchor.sha256)
        && crate::body::same_body(root, &doc.item_id, &doc.revision_id, &anchor.revision_id)
}

fn valid_shape(a: &Annotation) -> bool {
    valid_request_id(&a.id)
        && STYLES.contains(&a.style.as_str())
        && valid_revision_id(&a.anchor.revision_id)
        && valid_lower_hex_64(&a.anchor.sha256)
        && !a.anchor.side.is_empty()
        && a.anchor.side.len() <= 32
        && (1..=MAX_SEGMENTS).contains(&a.anchor.segments.len())
        && a.anchor.segments.iter().all(|s| s.start < s.end && !s.chapter.is_empty() && s.chapter.len() <= 128)
}

/// Every segment resolves in the bound edition, on UTF-8 boundaries, ordered and disjoint.
fn validate_anchor(anchor: &AnnotationAnchor, doc: &ReaderDocument, bodies: &ReaderBodies) -> Result<()> {
    anyhow::ensure!(anchor.revision_id == doc.revision_id, "stale_item_revision");
    let edition = doc
        .editions
        .iter()
        .find(|e| e.side == anchor.side && e.sha256 == anchor.sha256)
        .context("invalid_reader_anchor")?;
    let body = &bodies.get(&anchor.side).context("invalid_reader_anchor")?.0;
    let mut last: Option<(usize, usize)> = None;
    for s in &anchor.segments {
        let index = edition.chapters.iter().position(|c| c.key == s.chapter).context("invalid_reader_anchor")?;
        let ch = &edition.chapters[index];
        anyhow::ensure!(
            s.end <= ch.end - ch.start
                && body.is_char_boundary(ch.start + s.start)
                && body.is_char_boundary(ch.start + s.end),
            "invalid_reader_anchor"
        );
        if let Some((li, lend)) = last {
            anyhow::ensure!(index > li || (index == li && s.start >= lend), "invalid_reader_anchor");
        }
        last = Some((index, s.end));
    }
    Ok(())
}

/// Editing a stale mark needs the current item AND its bound historical source to be
/// readable at this ceiling; never weaker than existing history access.
fn authorize_bound(root: &Path, item: &str, anchor: &AnnotationAnchor, doc: &ReaderDocument, ceiling: Sensitivity) -> Result<()> {
    if anchor.revision_id == doc.revision_id {
        return Ok(());
    }
    let old = revision_text::historical_revision(root, item, &anchor.revision_id, ceiling)
        .map_err(|_| anyhow::anyhow!("library_resource_not_found"))?;
    document_revision_locked(root, &old, ceiling).map_err(|_| anyhow::anyhow!("library_resource_not_found"))?;
    Ok(())
}

pub fn list(
    library: &Path,
    item: &str,
    offset: usize,
    limit: usize,
    expected_version: Option<&str>,
    ceiling: Sensitivity,
) -> Result<AnnotationPage> {
    anyhow::ensure!(
        valid_item_id(item) && (1..=100).contains(&limit) && (offset == 0 || expected_version.is_some()),
        "invalid_reader_annotation"
    );
    with_consistent_snapshot(library, |root| {
        let (_, doc, _) = current(root, item, ceiling)?;
        let l = ledger(root, item)?;
        let version = l.version();
        if let Some(expected) = expected_version {
            anyhow::ensure!(expected == version, "stale_reader_annotations");
        }
        // Stale marks bound to history this ceiling cannot read are not disclosed.
        let mut readable: BTreeMap<String, bool> = BTreeMap::new();
        let mut visible = Vec::new();
        for m in &l.marks {
            let ok = match readable.get(&m.anchor.revision_id) {
                Some(ok) => *ok,
                None => {
                    let ok = authorize_bound(root, item, &m.anchor, &doc, ceiling).is_ok();
                    readable.insert(m.anchor.revision_id.clone(), ok);
                    ok
                }
            };
            if ok {
                visible.push(m);
            }
        }
        let total = visible.len();
        let mut annotations = Vec::new();
        let mut bytes = 256;
        let mut next = offset;
        for m in visible.iter().skip(offset).take(limit) {
            let listed = ListedAnnotation {
                id: m.id.clone(),
                style: m.style.clone(),
                anchor: m.anchor.clone(),
                status: if is_current(root, &m.anchor, &doc) { "current" } else { "stale" }.into(),
                order: m.order,
            };
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
            version,
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
        valid_item_id(item) && valid_lower_hex_64(expected_version) && shape_ok && !principal.is_empty(),
        "invalid_reader_annotation"
    );
    let change_sha256 = reader_sha256(&serde_json::to_vec(&change)?);
    let root = existing_canonical_root(library)?;
    let lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    let (_, doc, bodies) = current(&root, item, ceiling)?;
    let mut l = ledger(&root, item)?;
    if l.version() != expected_version {
        // Only the immediately preceding exact attempt is replayed; anything older is stale.
        if let Some(r) = &l.last {
            if r.principal == principal && r.expected_version == expected_version && r.change_sha256 == change_sha256 {
                return Ok(r.result.clone());
            }
        }
        anyhow::bail!("stale_reader_annotations")
    }
    let order = l.sequence + 1;
    let (id, outcome) = match change {
        Change::Put { annotation } => {
            match l.marks.iter().position(|m| m.id == annotation.id) {
                Some(i) => {
                    let existing = &l.marks[i];
                    anyhow::ensure!(existing.anchor == annotation.anchor, "reader_annotation_conflict");
                    anyhow::ensure!(existing.style != annotation.style, "reader_annotation_conflict");
                    authorize_bound(&root, item, &existing.anchor, &doc, ceiling)?;
                    anyhow::ensure!(
                        !l.marks.iter().any(|m| m.anchor == annotation.anchor && m.style == annotation.style),
                        "reader_annotation_conflict"
                    );
                    l.marks[i].style = annotation.style;
                    l.marks[i].order = order;
                    (annotation.id, "style_changed")
                }
                None => {
                    validate_anchor(&annotation.anchor, &doc, &bodies)?;
                    anyhow::ensure!(
                        !l.marks.iter().any(|m| m.anchor == annotation.anchor && m.style == annotation.style),
                        "reader_annotation_conflict"
                    );
                    anyhow::ensure!(l.marks.len() < MAX_MARKS, "reader_annotation_limit_exceeded");
                    let id = annotation.id.clone();
                    l.marks.push(Stored { id: annotation.id, style: annotation.style, anchor: annotation.anchor, order });
                    l.marks.sort_by(|a, b| a.id.cmp(&b.id));
                    (id, "created")
                }
            }
        }
        Change::Delete { id } => {
            let i = l.marks.iter().position(|m| m.id == id).context("reader_annotation_not_found")?;
            authorize_bound(&root, item, &l.marks[i].anchor, &doc, ceiling)?;
            l.marks.remove(i);
            (id, "deleted")
        }
    };
    l.sequence = order;
    let result = EditResult { item_id: item.into(), version: l.version(), id, outcome: outcome.into() };
    l.last = Some(Receipt {
        principal: principal.into(),
        expected_version: expected_version.into(),
        change_sha256,
        result: result.clone(),
    });
    let bytes = serde_json::to_vec(&l)?;
    anyhow::ensure!(bytes.len() <= MAX_LEDGER_BYTES || outcome == "deleted", "reader_annotation_limit_exceeded");
    ensure_contained_dir(&root, Path::new("catalog/reader-annotations"))?;
    durable_replace(&root, &path(&root, item), &bytes)?;
    lock.unlock()?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, String, String, ReaderDocument) {
        let dir = tempfile::tempdir().unwrap();
        let mut draft = crate::tests::draft();
        draft.content_markdown = "开篇\n# 一\n甲🌹乙甲🌹乙\n## 二\n尾声\n".into();
        let receipt = ingest(dir.path(), &crate::tests::capture(), draft).unwrap();
        let (id, rev) = (receipt.item_ids[0].clone(), receipt.revision_ids[0].clone());
        let doc = reader_document(dir.path(), &id, &rev, Sensitivity::Public).unwrap();
        (dir, id, rev, doc)
    }
    fn mark(id: &str, style: &str, rev: &str, doc: &ReaderDocument, segs: &[(usize, usize, usize)]) -> Annotation {
        let e = &doc.editions[0];
        Annotation { id: id.into(), style: style.into(), anchor: AnnotationAnchor {
            revision_id: rev.into(), side: e.side.clone(), sha256: e.sha256.clone(),
            segments: segs.iter().map(|(c, s, t)| Segment { chapter: e.chapters[*c].key.clone(), start: *s, end: *t }).collect() } }
    }
    fn put(a: Annotation) -> Change { Change::Put { annotation: a } }

    #[test]
    fn five_styles_overlap_cross_chapter_cas_replay_aba_and_stale() {
        let (dir, id, rev, doc) = setup();
        let lib = dir.path();
        let pub_ = Sensitivity::Public;
        let v0 = list(lib, &id, 0, 100, None, pub_).unwrap().version;
        // Repeated text: second "甲🌹乙" in chapter 1 is a distinct byte range (7..14 after the heading line).
        let ch1 = &doc.editions[0].chapters[1];
        let body_len = ch1.end - ch1.start;
        let mut v = v0.clone();
        let mut ids = vec![];
        for (i, style) in STYLES.iter().enumerate() {
            let r = edit(lib, &id, &v, put(mark(&format!("m{i}"), style, &rev, &doc, &[(1, 0, body_len)])), "owner", pub_).unwrap();
            assert_eq!(r.outcome, "created");
            assert_ne!(r.version, v);
            v = r.version; ids.push(r.id);
        }
        // Cross-chapter mark is one atomic mark.
        let cross = mark("mx", "yellow", &rev, &doc, &[(1, 0, 2), (2, 0, 3)]);
        let r = edit(lib, &id, &v, put(cross.clone()), "owner", pub_).unwrap();
        // Exact replay returns the same result without a new version.
        assert_eq!(edit(lib, &id, &v, put(cross.clone()), "owner", pub_).unwrap(), r);
        v = r.version;
        let page = list(lib, &id, 0, 100, None, pub_).unwrap();
        assert_eq!((page.total, page.annotations.len()), (6, 6));
        assert!(page.annotations.iter().all(|a| a.status == "current"));
        // Exact same anchor/style is a conflict; a non-boundary emoji offset is invalid.
        assert_eq!(edit(lib, &id, &v, put(mark("dup", "red", &rev, &doc, &[(1, 0, body_len)])), "owner", pub_).unwrap_err().to_string(), "reader_annotation_conflict");
        assert_eq!(edit(lib, &id, &v, put(mark("bad", "red", &rev, &doc, &[(1, 0, 4)])), "owner", pub_).unwrap_err().to_string(), "invalid_reader_anchor");
        assert_eq!(edit(lib, &id, &v, put(mark("ord", "red", &rev, &doc, &[(2, 0, 3), (1, 0, 2)])), "owner", pub_).unwrap_err().to_string(), "invalid_reader_anchor");
        // Stale expected version is rejected, not replayed.
        assert_eq!(edit(lib, &id, &v0, put(mark("late", "red", &rev, &doc, &[(1, 0, 3)])), "owner", pub_).unwrap_err().to_string(), "stale_reader_annotations");
        // Anchor change under the same id rejects; style change keeps id and anchor.
        assert_eq!(edit(lib, &id, &v, put(mark("m0", "bold", &rev, &doc, &[(1, 0, 3)])), "owner", pub_).unwrap_err().to_string(), "reader_annotation_conflict");
        let mut restyled = cross.clone(); restyled.style = "blue".into();
        let r = edit(lib, &id, &v, put(restyled), "owner", pub_).unwrap();
        assert_eq!(r.outcome, "style_changed");
        // ABA: delete then re-put the same mark never repeats a version.
        let v1 = r.version;
        let d = edit(lib, &id, &v1, Change::Delete { id: "mx".into() }, "owner", pub_).unwrap();
        let mut back = cross.clone(); back.style = "blue".into();
        let p = edit(lib, &id, &d.version, put(back), "owner", pub_).unwrap();
        assert_ne!(p.version, v1);
        // The delete's old attempt cannot resurrect or replay after the intervening put.
        assert_eq!(edit(lib, &id, &v1, Change::Delete { id: "mx".into() }, "owner", pub_).unwrap_err().to_string(), "stale_reader_annotations");
        // Paging is version bound.
        let first = list(lib, &id, 0, 2, None, pub_).unwrap();
        assert_eq!(first.next_offset, Some(2));
        list(lib, &id, 2, 2, Some(&first.version), pub_).unwrap();
        assert!(list(lib, &id, 2, 2, None, pub_).is_err());
        // Reading state is untouched.
        assert!(!lib.join("catalog/reader-state.json").exists());
        // A title edit keeps every mark current (same body); a body change below stales them.
        let renamed = revise_item_metadata(lib, &id, ItemMetadataUpdate {
            expected_revision_id: rev.clone(), title: "改过的标题".into(), manual_tags: vec![] }).unwrap();
        let page = list(lib, &id, 0, 100, None, pub_).unwrap();
        assert_eq!(page.total, 6);
        assert!(page.annotations.iter().all(|a| a.status == "current"));
        // Revision change: marks are stale, retained, and still deletable.
        let newer = edit_markdown(lib, MarkdownEdit { item_id: id.clone(), expected_revision_id: renamed.revision_id.clone(),
            request_id: "ann-edit".into(), markdown: "# 新\n正文\n".into() }, pub_).unwrap();
        let page = list(lib, &id, 0, 100, None, pub_).unwrap();
        assert_eq!(page.total, 6);
        assert!(page.annotations.iter().all(|a| a.status == "stale"));
        let d = edit(lib, &id, &page.version, Change::Delete { id: "m1".into() }, "owner", pub_).unwrap();
        // New marks on the old revision are rejected.
        assert_eq!(edit(lib, &id, &d.version, put(mark("old", "red", &rev, &doc, &[(1, 0, 3)])), "owner", pub_).unwrap_err().to_string(), "stale_item_revision");
        // Trash hides everything; corrupt ledger fails closed.
        trash_item(lib, &id, &newer.revision_id, "owner", None, None).unwrap();
        assert_eq!(list(lib, &id, 0, 100, None, pub_).unwrap_err().to_string(), "library_resource_not_found");
    }

    #[test]
    fn restoring_an_older_backup_never_lets_a_later_version_pass_cas() {
        let (dir, id, rev, doc) = setup();
        let lib = dir.path();
        let pub_ = Sensitivity::Public;
        let ch1 = &doc.editions[0].chapters[1];
        let n = ch1.end - ch1.start;
        let v0 = list(lib, &id, 0, 100, None, pub_).unwrap().version;
        let a = edit(lib, &id, &v0, put(mark("a", "red", &rev, &doc, &[(1, 0, n)])), "owner", pub_).unwrap();
        let ledger = lib.join(format!("catalog/reader-annotations/{id}.json"));
        let backup = fs::read(&ledger).unwrap();
        // After the backup another device writes b; it holds the resulting version.
        let b = edit(lib, &id, &a.version, put(mark("b", "bold", &rev, &doc, &[(1, 0, n)])), "owner", pub_).unwrap();
        // The library is restored, and a different write takes the same sequence.
        fs::write(&ledger, &backup).unwrap();
        assert_eq!(list(lib, &id, 0, 100, None, pub_).unwrap().version, a.version);
        let c = edit(lib, &id, &a.version, put(mark("c", "yellow", &rev, &doc, &[(1, 0, n)])), "owner", pub_).unwrap();
        assert_ne!(c.version, b.version);
        // The pre-restore version is stale, not a match on different marks.
        assert_eq!(edit(lib, &id, &b.version, Change::Delete { id: "b".into() }, "owner", pub_).unwrap_err().to_string(), "stale_reader_annotations");
    }

    #[test]
    fn corrupt_ledger_fails_closed_not_empty() {
        let (dir, id, _, _) = setup();
        fs::create_dir_all(dir.path().join("catalog/reader-annotations")).unwrap();
        fs::write(dir.path().join(format!("catalog/reader-annotations/{id}.json")), b"{not json").unwrap();
        assert_eq!(list(dir.path(), &id, 0, 10, None, Sensitivity::Public).unwrap_err().to_string(), "reader_annotations_unavailable");
    }
}
