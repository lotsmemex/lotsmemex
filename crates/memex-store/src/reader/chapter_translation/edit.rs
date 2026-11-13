//! Owner edits of an effective AI chapter translation (`reader_translation_edit_v1`).
//!
//! An edit is a new immutable owner artifact bound to the exact effective version it
//! replaced; the AI projection it starts from is never rewritten. A per-revision/
//! chapter head file is the single effective pointer AND carries the bounded commit
//! receipts, so publication is one durable replacement after the artifact is
//! installed: a crash in between leaves an unreferenced immutable file only.
use super::*;

pub const MAX_CHUNK: usize = 8192;
/// Whole edit chain (across revisions) for one original chapter.
pub const MAX_DEPTH: u32 = 128;
const MAX_RECEIPTS: usize = 64;
const MAX_ARTIFACT: u64 = 6 * MAX_TEXT as u64 + 65536;
const KIND: &str = "owner_translation_edit";
const DOMAIN: &str = "memex.reader.translation-edit.v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Begin {
    pub item_id: String,
    pub revision_id: String,
    pub chapter: String,
    /// Opaque effective `translation.version` from translate_metadata/chunk.
    pub expected_translation_version: String,
    pub expected_translation_sha256: String,
    pub request_id: String,
    pub bytes: usize,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub item_id: String,
    pub request_id: String,
    pub revision_id: String,
    pub chapter: String,
    pub previous_version: String,
    pub previous_sha256: String,
    /// Effective version at `revision_id` after commit (unchanged on a no-op).
    pub translation_version: String,
    pub sha256: String,
    pub bytes: usize,
    pub changed: bool,
}

#[derive(Debug, Serialize)]
pub struct Upload {
    pub edit_id: String,
    pub received_bytes: usize,
    pub max_chunk_bytes: usize,
    pub state: String,
    pub receipt: Option<Receipt>,
}

#[derive(Debug, Serialize)]
pub struct Preview {
    pub translation: Metadata,
    /// Currently visible, currently effective, de-duplicated AI marks touching this
    /// chapter's effective version: every one becomes stale on a substantive edit.
    pub stale_marks: usize,
    pub annotations_version: String,
    pub max_bytes: usize,
    pub max_chunk_bytes: usize,
}

/// Physical stored artifact an edit was made from (verifiable by path + hash).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Physical {
    /// `machine_translation` or `owner_edit`.
    pub kind: String,
    pub identity: Identity,
    /// Hash of the complete stored artifact (projection or edit file).
    pub artifact_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditOf {
    /// The effective (view) version the editor saw and CAS-bound.
    pub version: String,
    pub sha256: String,
    pub bytes: usize,
    pub physical: Physical,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditedBlock {
    pub id: String,
    pub source_range: [usize; 2],
    pub kind: paragraphs::Kind,
    /// Present only while byte-identical to the base block (proven binding).
    pub target_range: Option<[usize; 2]>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerEdit {
    pub edit_of_version: String,
    pub edit_of_sha256: String,
    pub edit_of_kind: String,
    pub machine_origin_sha256: String,
    pub depth: u32,
    /// `aligned|partial|none|missing` for this version's recorded correspondence.
    pub alignment_status: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Artifact {
    schema_version: u32,
    kind: String,
    identity: Identity,
    owner_edit_of: EditOf,
    machine_origin: Physical,
    origin_provenance: Provenance,
    principal: String,
    request_id: String,
    depth: u32,
    sha256: String,
    pub(super) markdown: String,
    pub(super) blocks: Option<Vec<EditedBlock>>,
}

/// Session-only view attached to a resolved projection; never serialized.
pub(crate) struct View {
    pub(super) version: String,
    pub(super) metadata: Metadata,
    pub(super) blocks: Option<Vec<EditedBlock>>,
    machine_origin: Physical,
    origin_provenance: Provenance,
    depth: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredReceipt {
    principal: String,
    request_sha256: String,
    receipt: Receipt,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Head {
    schema_version: u32,
    identity: Identity,
    version: String,
    receipts: Vec<StoredReceipt>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stage {
    schema_version: u32,
    principal: String,
    begin: Begin,
    received_bytes: usize,
}

fn dir(root: &Path, id: &Identity) -> PathBuf {
    root.join("catalog/chapter-translation-edits")
        .join(&id.item_id)
        .join(&id.revision_id)
}

fn head_path(root: &Path, id: &Identity) -> PathBuf {
    dir(root, id).join(format!("{}.json", reader_sha256(id.original_chapter.as_bytes())))
}

fn artifact_path(root: &Path, id: &Identity, version: &str) -> PathBuf {
    dir(root, id)
        .join(reader_sha256(id.original_chapter.as_bytes()))
        .join(format!("{version}.json"))
}

fn read_bytes(root: &Path, path: &Path, max: u64) -> Result<Option<Vec<u8>>> {
    use std::io::Read;
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
        Ok(m) => {
            anyhow::ensure!(
                m.is_file() && !m.file_type().is_symlink() && m.len() <= max,
                "reader_translation_edit_unavailable"
            );
            let mut bytes = Vec::new();
            open_regular_nofollow(&contained_existing(root, path)?)?.read_to_end(&mut bytes)?;
            Ok(Some(bytes))
        }
    }
}

fn load_artifact(root: &Path, id: &Identity, version: &str) -> Result<Artifact> {
    anyhow::ensure!(valid_lower_hex_64(version), "reader_translation_edit_unavailable");
    let bytes = read_bytes(root, &artifact_path(root, id, version), MAX_ARTIFACT)?
        .context("reader_translation_edit_unavailable")?;
    anyhow::ensure!(reader_sha256(&bytes) == version, "reader_translation_edit_unavailable");
    let a: Artifact = serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("reader_translation_edit_unavailable"))?;
    anyhow::ensure!(
        a.schema_version == 1 && a.kind == KIND && a.identity == *id,
        "reader_translation_edit_unavailable"
    );
    Ok(a)
}

fn load_head(root: &Path, id: &Identity) -> Result<Option<Head>> {
    let Some(bytes) = read_bytes(root, &head_path(root, id), 1024 * 1024)? else {
        return Ok(None);
    };
    let h: Head = serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("reader_translation_edit_unavailable"))?;
    anyhow::ensure!(
        h.schema_version == 1 && h.identity == *id && valid_lower_hex_64(&h.version)
            && h.receipts.len() <= MAX_RECEIPTS,
        "reader_translation_edit_unavailable"
    );
    Ok(Some(h))
}

/// Base body and block correspondence of a physical artifact, hash-verified.
fn base_of(root: &Path, p: &Physical, source: &str) -> Result<(String, Option<Vec<EditedBlock>>, Physical, Provenance, u32)> {
    match p.kind.as_str() {
        "machine_translation" => {
            let proj = stored(root, &p.identity, source)?.context("reader_translation_edit_unavailable")?;
            anyhow::ensure!(proj.metadata()?.version == p.artifact_sha256, "reader_translation_edit_unavailable");
            let blocks = proj.blocks.as_ref().map(|b| {
                b.iter().map(|b| EditedBlock {
                    id: b.id.clone(),
                    source_range: b.source_range,
                    kind: b.kind,
                    target_range: Some(b.target_range),
                }).collect()
            });
            Ok((proj.markdown, blocks, p.clone(), proj.provenance, 0))
        }
        "owner_edit" => {
            let a = load_artifact(root, &p.identity, &p.artifact_sha256)?;
            Ok((a.markdown, a.blocks, a.machine_origin, a.origin_provenance, a.depth))
        }
        _ => anyhow::bail!("reader_translation_edit_unavailable"),
    }
}

/// Correspondence that is still provably bound: a block survives only while its
/// complete generated bytes lie wholly in the unchanged common prefix or suffix.
/// Nothing is re-derived from paragraph counts or substring search.
fn retained(base: &str, next: &str, blocks: &Option<Vec<EditedBlock>>) -> Option<Vec<EditedBlock>> {
    let (a, b) = (base.as_bytes(), next.as_bytes());
    let mut prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    while !base.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let max_suffix = a.len().min(b.len()) - prefix;
    let mut suffix = a.iter().rev().zip(b.iter().rev()).take(max_suffix).take_while(|(x, y)| x == y).count();
    while !base.is_char_boundary(a.len() - suffix) || !next.is_char_boundary(b.len() - suffix) {
        suffix -= 1;
    }
    blocks.as_ref().map(|blocks| {
        blocks.iter().map(|blk| EditedBlock {
            target_range: blk.target_range.and_then(|[s, e]| {
                if e <= prefix {
                    Some([s, e])
                } else if s >= a.len() - suffix {
                    Some([s + b.len() - a.len(), e + b.len() - a.len()])
                } else {
                    None
                }
            }),
            ..blk.clone()
        }).collect()
    })
}

fn alignment_status(blocks: &Option<Vec<EditedBlock>>) -> &'static str {
    match blocks {
        None => "missing",
        Some(b) if b.iter().all(|b| b.target_range.is_some()) => "aligned",
        Some(b) if b.iter().any(|b| b.target_range.is_some()) => "partial",
        Some(_) => "none",
    }
}

fn validate_artifact(a: &Artifact, id: &Identity, source: &str, base_md: &str, base_blocks: &Option<Vec<EditedBlock>>) -> Result<()> {
    let base = &a.owner_edit_of;
    anyhow::ensure!(
        a.identity == *id
            && id.source_sha256 == reader_sha256(source.as_bytes())
            && id.source_bytes == source.len()
            && !a.markdown.trim().is_empty()
            && !a.markdown.contains('\0')
            && a.markdown.len() <= MAX_TEXT
            && a.sha256 == reader_sha256(a.markdown.as_bytes())
            && base.sha256 == reader_sha256(base_md.as_bytes())
            && base.bytes == base_md.len()
            && a.markdown != base_md
            && (1..=MAX_DEPTH).contains(&a.depth)
            && valid_lower_hex_64(&base.version)
            && base.physical.identity.item_id == id.item_id
            && base.physical.identity.original_chapter == id.original_chapter
            && base.physical.identity.source_sha256 == id.source_sha256
            && a.machine_origin.kind == "machine_translation"
            && valid_text(&a.principal, 128)
            && valid_request_id(&a.request_id),
        "reader_translation_edit_unavailable"
    );
    match (&a.blocks, base_blocks) {
        (None, None) => {}
        (Some(next), Some(old)) => {
            anyhow::ensure!(next.len() == old.len(), "reader_translation_edit_unavailable");
            let mut end = 0;
            for (n, o) in next.iter().zip(old) {
                anyhow::ensure!(
                    n.id == o.id && n.source_range == o.source_range && n.kind == o.kind,
                    "reader_translation_edit_unavailable"
                );
                if let Some([s, e]) = n.target_range {
                    let [os, oe] = o.target_range.context("reader_translation_edit_unavailable")?;
                    anyhow::ensure!(
                        s >= end && s < e && a.markdown.get(s..e).is_some()
                            && a.markdown.get(s..e) == base_md.get(os..oe),
                        "reader_translation_edit_unavailable"
                    );
                    end = e;
                }
            }
        }
        _ => anyhow::bail!("reader_translation_edit_unavailable"),
    }
    Ok(())
}

/// The effective owner edit stored for exactly this revision's source identity.
/// Missing head is `None`; any unreadable/inconsistent head or artifact fails closed.
pub(super) fn head(root: &Path, id: &Identity, source: &str) -> Result<Option<Projection>> {
    let Some(h) = load_head(root, id)? else {
        return Ok(None);
    };
    let a = load_artifact(root, id, &h.version)?;
    let (base_md, base_blocks, origin, origin_provenance, base_depth) = base_of(root, &a.owner_edit_of.physical, source)?;
    validate_artifact(&a, id, source, &base_md, &base_blocks)?;
    anyhow::ensure!(
        origin == a.machine_origin && origin_provenance == a.origin_provenance && a.depth == base_depth + 1,
        "reader_translation_edit_unavailable"
    );
    let metadata = Metadata {
        version: h.version.clone(),
        item_id: id.item_id.clone(),
        revision_id: id.revision_id.clone(),
        original_chapter: id.original_chapter.clone(),
        source_sha256: id.source_sha256.clone(),
        target_language: "zh-CN".into(),
        sha256: a.sha256.clone(),
        bytes: a.markdown.len(),
        provenance: "owner_edit".into(),
        route: a.origin_provenance.route.clone(),
        owner_edit: Some(OwnerEdit {
            edit_of_version: a.owner_edit_of.version.clone(),
            edit_of_sha256: a.owner_edit_of.sha256.clone(),
            edit_of_kind: a.owner_edit_of.physical.kind.clone(),
            machine_origin_sha256: a.machine_origin.artifact_sha256.clone(),
            depth: a.depth,
            alignment_status: alignment_status(&a.blocks).into(),
        }),
    };
    let retained_blocks = a.blocks.as_ref().map(|b| {
        b.iter().filter_map(|b| b.target_range.map(|t| TranslationBlock {
            id: b.id.clone(),
            source_range: b.source_range,
            target_range: t,
            kind: b.kind,
        })).collect()
    });
    Ok(Some(Projection {
        schema_version: 0,
        identity: a.identity,
        provenance: a.origin_provenance.clone(),
        sha256: a.sha256,
        markdown: a.markdown,
        // Always present for an edit, so a retrospective map job can never bind an
        // owner-edited body (it rejects recorded maps); readers use `owner` below.
        blocks: Some(retained_blocks.unwrap_or_default()),
        owner: Some(Box::new(View {
            version: h.version,
            metadata,
            blocks: a.blocks,
            machine_origin: a.machine_origin,
            origin_provenance: a.origin_provenance,
            depth: a.depth,
        })),
    }))
}

/// Edited-block rows for the alignment page of an owner edit.
pub(super) fn pairs(view: &View) -> Option<Vec<paragraphs::Pair>> {
    view.blocks.as_ref().map(|b| b.iter().map(|b| paragraphs::Pair {
        o: Some(b.source_range),
        t: b.target_range,
        kind: b.kind,
        relation: if b.target_range.is_some() { paragraphs::Relation::Matched } else { paragraphs::Relation::Unresolved },
    }).collect())
}

pub(super) fn status(view: &View) -> &'static str {
    alignment_status(&view.blocks)
}

/// Any owner translation edit head in this library (deployment downgrade guard).
pub fn any_owner_edits(library: &Path) -> Result<bool> {
    let root = existing_canonical_root(library)?;
    let base = root.join("catalog/chapter-translation-edits");
    fn walk(p: &Path, depth: usize) -> Result<bool> {
        match fs::read_dir(p) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
            Ok(entries) => {
                for e in entries {
                    let e = e?;
                    let t = e.file_type()?;
                    if depth == 2 && t.is_file() && e.file_name().to_string_lossy().ends_with(".json") {
                        return Ok(true);
                    }
                    if depth < 2 && t.is_dir() && walk(&e.path(), depth + 1)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
        }
    }
    walk(&base, 0)
}

fn validate_begin(b: &Begin, principal: &str) -> Result<()> {
    anyhow::ensure!(
        valid_item_id(&b.item_id)
            && valid_revision_id(&b.revision_id)
            && !b.chapter.trim().is_empty()
            && b.chapter.len() <= 128
            && valid_request_id(&b.request_id)
            && valid_lower_hex_64(&b.expected_translation_version)
            && valid_lower_hex_64(&b.expected_translation_sha256)
            && valid_lower_hex_64(&b.sha256)
            && (principal == "local_owner" || valid_lower_hex_64(principal)),
        "invalid_reader_translation_edit"
    );
    anyhow::ensure!(b.bytes > 0 && b.bytes <= MAX_TEXT, "reader_translation_edit_too_large");
    Ok(())
}

fn edit_id(principal: &str, request: &str) -> String {
    format!("tedit_{}", reader_sha256(&serde_json::to_vec(&(DOMAIN, principal, request)).unwrap()))
}

fn request_sha256(b: &Begin, principal: &str) -> Result<String> {
    Ok(reader_sha256(&serde_json::to_vec(&(DOMAIN, principal, b))?))
}

fn stage_path(root: &Path, id: &str) -> Result<PathBuf> {
    anyhow::ensure!(
        id.strip_prefix("tedit_").is_some_and(valid_lower_hex_64),
        "invalid_reader_translation_edit"
    );
    Ok(root.join("catalog/reader-translation-edits").join(format!("{id}.json")))
}

fn stage(root: &Path, item: &str, id: &str, principal: &str, ceiling: Sensitivity) -> Result<Stage> {
    anyhow::ensure!(valid_item_id(item), "invalid_reader_translation_edit");
    read_item_at_sensitivity(root, item, ceiling)?.context("library_resource_not_found")?;
    let bytes = read_bytes(root, &stage_path(root, id)?, 64 * 1024)?.context("reader_translation_edit_not_found")?;
    let s: Stage = serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid_reader_translation_edit"))?;
    validate_begin(&s.begin, &s.principal)?;
    anyhow::ensure!(
        s.schema_version == 1
            && s.principal == principal
            && s.begin.item_id == item
            && edit_id(principal, &s.begin.request_id) == id,
        "reader_translation_edit_not_found"
    );
    anyhow::ensure!(s.received_bytes <= s.begin.bytes, "invalid_reader_translation_edit");
    Ok(s)
}

/// The original receipt for this principal/request, after re-authorizing the
/// current item AND the bound (possibly now historical) revision at `ceiling`.
fn replay(root: &Path, b: &Begin, principal: &str, ceiling: Sensitivity) -> Result<Option<Receipt>> {
    let (_, current) = read_item_at_sensitivity(root, &b.item_id, ceiling)?.context("library_resource_not_found")?;
    let rev = if current.revision_id == b.revision_id {
        current
    } else {
        match revision_text::historical_revision(root, &b.item_id, &b.revision_id, ceiling) {
            Ok(r) => r,
            Err(_) => return Ok(None),
        }
    };
    let (doc, bodies) = document_revision_locked(root, &rev, ceiling)?;
    let Some(Ok(src)) = source_from(&doc, &bodies, &b.item_id, &b.revision_id, &b.chapter) else {
        return Ok(None);
    };
    let Some(h) = load_head(root, &src.identity)? else {
        return Ok(None);
    };
    let hash = request_sha256(b, principal)?;
    match h.receipts.iter().find(|r| r.principal == principal && r.receipt.request_id == b.request_id) {
        Some(r) if r.request_sha256 == hash => Ok(Some(r.receipt.clone())),
        Some(_) => anyhow::bail!("request_id_conflict"),
        None => Ok(None),
    }
}

fn current_effective(root: &Path, b: &Begin, ceiling: Sensitivity) -> Result<(Source, Projection, Metadata)> {
    let source = source_locked(root, &b.item_id, &b.revision_id, &b.chapter, ceiling)?;
    let (p, m) = effective(root, &source, ceiling)?.context("reader_translation_unavailable")?;
    anyhow::ensure!(
        m.version == b.expected_translation_version && m.sha256 == b.expected_translation_sha256,
        "stale_reader_translation"
    );
    Ok((source, p, m))
}

/// Read-only: the effective version to CAS against and how many current AI marks
/// a substantive edit would make stale. No staging, no write.
pub fn preview(library: &Path, item: &str, revision: &str, chapter: &str, ceiling: Sensitivity) -> Result<Preview> {
    let translation = metadata(library, item, revision, chapter, ceiling)?.context("reader_translation_unavailable")?;
    let filter = annotations_ai::ProjectionFilter {
        chapter: chapter.into(),
        translation_version: translation.version.clone(),
        sha256: translation.sha256.clone(),
    };
    let (mut offset, mut count) = (0, 0);
    let (mut version, mut view) = (None::<String>, None::<String>);
    loop {
        let page = annotations_ai::list(library, item, Some(&filter), offset, 100, version.as_deref(), view.as_deref(), ceiling)?;
        count += page.annotations.iter().filter(|a| a.status == "current").count();
        version = Some(page.version);
        view = Some(page.view_version);
        match page.next_offset {
            Some(next) => offset = next,
            None => break,
        }
    }
    // The page set must still describe the same effective translation.
    let again = metadata(library, item, revision, chapter, ceiling)?.context("reader_translation_unavailable")?;
    anyhow::ensure!(again.version == translation.version, "stale_reader_translation");
    Ok(Preview {
        translation,
        stale_marks: count,
        annotations_version: version.unwrap_or_default(),
        max_bytes: MAX_TEXT,
        max_chunk_bytes: MAX_CHUNK,
    })
}

pub fn begin(library: &Path, request: Begin, principal: &str, ceiling: Sensitivity) -> Result<Upload> {
    validate_begin(&request, principal)?;
    let root = existing_canonical_root(library)?;
    let lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    let id = edit_id(principal, &request.request_id);
    if let Some(receipt) = replay(&root, &request, principal, ceiling)? {
        return Ok(Upload { edit_id: id, received_bytes: request.bytes, max_chunk_bytes: MAX_CHUNK,
            state: "committed".into(), receipt: Some(receipt) });
    }
    current_effective(&root, &request, ceiling)?;
    let path = stage_path(&root, &id)?;
    let received = match fs::symlink_metadata(&path) {
        Ok(_) => {
            let s = stage(&root, &request.item_id, &id, principal, ceiling)?;
            anyhow::ensure!(s.begin == request, "request_id_conflict");
            s.received_bytes
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let parent = ensure_contained_dir(&root, Path::new("catalog/reader-translation-edits"))?;
            #[cfg(unix)]
            fs::set_permissions(parent, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
            durable_replace(&root, &path, &serde_json::to_vec(&Stage {
                schema_version: 1, principal: principal.into(), begin: request, received_bytes: 0,
            })?)?;
            0
        }
        Err(e) => return Err(e.into()),
    };
    lock.unlock()?;
    Ok(Upload { edit_id: id, received_bytes: received, max_chunk_bytes: MAX_CHUNK, state: "uploading".into(), receipt: None })
}

pub fn chunk(library: &Path, item: &str, id: &str, offset: usize, markdown: &str, principal: &str, ceiling: Sensitivity) -> Result<usize> {
    use std::io::{Read, Seek, SeekFrom, Write};
    anyhow::ensure!(
        !markdown.is_empty() && markdown.len() <= MAX_CHUNK && !markdown.contains('\0'),
        "invalid_reader_translation_edit"
    );
    let root = existing_canonical_root(library)?;
    let lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    let mut s = stage(&root, item, id, principal, ceiling)?;
    let received = s.received_bytes;
    anyhow::ensure!(
        offset <= received && markdown.len() <= s.begin.bytes.saturating_sub(offset),
        "reader_translation_edit_upload_conflict"
    );
    let path = contained_output_path(&root, &stage_path(&root, id)?.with_extension("md"))?;
    if offset < received {
        anyhow::ensure!(offset + markdown.len() <= received, "reader_translation_edit_upload_conflict");
        let mut file = fs::File::open(&path)?;
        file.seek(SeekFrom::Start(offset as u64))?;
        let mut prior = vec![0; markdown.len()];
        file.read_exact(&mut prior)?;
        anyhow::ensure!(prior == markdown.as_bytes(), "reader_translation_edit_upload_conflict");
        return Ok(received);
    }
    // Every step re-admits the bound resource and version; a committed request
    // never accepts more bytes.
    current_effective(&root, &s.begin, ceiling)?;
    anyhow::ensure!(replay(&root, &s.begin, principal, ceiling)?.is_none(), "reader_translation_edit_upload_conflict");
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    anyhow::ensure!(file.metadata()?.len() >= received as u64, "invalid_reader_translation_edit");
    file.set_len(received as u64)?;
    file.seek(SeekFrom::Start(received as u64))?;
    file.write_all(markdown.as_bytes())?;
    file.sync_all()?;
    s.received_bytes += markdown.len();
    durable_replace(&root, &stage_path(&root, id)?, &serde_json::to_vec(&s)?)?;
    lock.unlock()?;
    Ok(received + markdown.len())
}

/// Replay-only lookup used before queue admission (no write, no lock upgrade).
pub fn committed(library: &Path, item: &str, id: &str, principal: &str, ceiling: Sensitivity) -> Result<Option<Receipt>> {
    with_consistent_snapshot(library, |root| {
        let s = stage(root, item, id, principal, ceiling)?;
        replay(root, &s.begin, principal, ceiling)
    })
}

pub fn commit(library: &Path, item: &str, id: &str, principal: &str, ceiling: Sensitivity) -> Result<Receipt> {
    commit_with_gate(library, item, id, principal, ceiling, |publish| publish())
}

/// `gate` wraps the single head replacement (tests inject crash/sync failures).
pub fn commit_with_gate<G>(library: &Path, item: &str, id: &str, principal: &str, ceiling: Sensitivity, gate: G) -> Result<Receipt>
where
    G: FnOnce(Box<dyn FnOnce() -> Result<()> + '_>) -> Result<()>,
{
    let root = existing_canonical_root(library)?;
    let lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    let s = stage(&root, item, id, principal, ceiling)?;
    if let Some(r) = replay(&root, &s.begin, principal, ceiling)? {
        return Ok(r);
    }
    let b = &s.begin;
    let bytes = read_bytes(&root, &stage_path(&root, id)?.with_extension("md"), MAX_TEXT as u64)?
        .context("reader_translation_edit_upload_conflict")?;
    anyhow::ensure!(
        s.received_bytes == b.bytes && bytes.len() == b.bytes && reader_sha256(&bytes) == b.sha256,
        "reader_translation_edit_upload_conflict"
    );
    let markdown = String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("invalid_reader_translation_edit"))?;
    anyhow::ensure!(!markdown.trim().is_empty() && !markdown.contains('\0'), "invalid_reader_translation_edit");
    let (source, base, m) = current_effective(&root, b, ceiling)?;
    let mut receipt = Receipt {
        item_id: b.item_id.clone(),
        request_id: b.request_id.clone(),
        revision_id: b.revision_id.clone(),
        chapter: b.chapter.clone(),
        previous_version: m.version.clone(),
        previous_sha256: m.sha256.clone(),
        translation_version: m.version.clone(),
        sha256: m.sha256.clone(),
        bytes: m.bytes,
        changed: false,
    };
    if markdown == base.markdown {
        // Byte-identical: no version, no receipt, no mark invalidation.
        lock.unlock()?;
        return Ok(receipt);
    }
    let (physical, origin, origin_provenance, depth, base_blocks) = match &base.owner {
        Some(v) => (
            Physical { kind: "owner_edit".into(), identity: base.identity.clone(), artifact_sha256: v.version.clone() },
            v.machine_origin.clone(), v.origin_provenance.clone(), v.depth, v.blocks.clone(),
        ),
        None => {
            let p = Physical { kind: "machine_translation".into(), identity: base.identity.clone(), artifact_sha256: base.metadata()?.version };
            let blocks = base.blocks.as_ref().map(|b| b.iter().map(|b| EditedBlock {
                id: b.id.clone(), source_range: b.source_range, kind: b.kind, target_range: Some(b.target_range),
            }).collect());
            (p.clone(), p, base.provenance.clone(), 0, blocks)
        }
    };
    anyhow::ensure!(depth < MAX_DEPTH, "reader_translation_edit_limit_exceeded");
    let a = Artifact {
        schema_version: 1,
        kind: KIND.into(),
        identity: source.identity.clone(),
        owner_edit_of: EditOf { version: m.version.clone(), sha256: m.sha256.clone(), bytes: m.bytes, physical },
        machine_origin: origin,
        origin_provenance,
        principal: principal.into(),
        request_id: b.request_id.clone(),
        depth: depth + 1,
        sha256: reader_sha256(markdown.as_bytes()),
        blocks: retained(&base.markdown, &markdown, &base_blocks),
        markdown,
    };
    validate_artifact(&a, &source.identity, &source.markdown, &base.markdown, &base_blocks)?;
    let artifact = serde_json::to_vec(&a)?;
    anyhow::ensure!(artifact.len() as u64 <= MAX_ARTIFACT, "reader_translation_edit_too_large");
    let version = reader_sha256(&artifact);
    let mut head = load_head(&root, &source.identity)?.unwrap_or(Head {
        schema_version: 1, identity: source.identity.clone(), version: String::new(), receipts: vec![],
    });
    receipt.translation_version = version.clone();
    receipt.sha256 = a.sha256.clone();
    receipt.bytes = a.markdown.len();
    receipt.changed = true;
    head.version = version.clone();
    head.receipts.push(StoredReceipt { principal: principal.into(), request_sha256: request_sha256(b, principal)?, receipt: receipt.clone() });
    if head.receipts.len() > MAX_RECEIPTS {
        head.receipts.remove(0);
    }
    // 1) immutable artifact (idempotent on retry), 2) one atomic pointer+receipt.
    let file = artifact_path(&root, &source.identity, &version);
    ensure_contained_dir(&root, file.parent().unwrap().strip_prefix(&root)?)?;
    install_binary(&root, &file, &artifact)?;
    durable_replace_with_gate(&root, &head_path(&root, &source.identity), &serde_json::to_vec(&head)?, gate)?;
    // Keep the small stage JSON so an exact commit retry still finds the receipt.
    let _ = fs::remove_file(stage_path(&root, id)?.with_extension("md"));
    lock.unlock()?;
    Ok(receipt)
}

#[derive(Debug, Serialize)]
pub struct HistoryEntry {
    pub kind: String,
    pub revision_id: String,
    pub artifact_sha256: String,
    pub sha256: String,
    pub bytes: usize,
    pub depth: u32,
}

#[derive(Debug, Serialize)]
pub struct History {
    pub translation_version: String,
    pub entries: Vec<HistoryEntry>,
    /// True when an older link exists but its revision is not readable here.
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct VersionChunk {
    pub artifact_sha256: String,
    pub kind: String,
    pub revision_id: String,
    pub sha256: String,
    pub bytes: usize,
    pub start: usize,
    pub end: usize,
    pub markdown: String,
    pub next_offset: Option<usize>,
}

/// Walk the effective edit chain back to its machine origin. Each older link is
/// authorized on its own revision; an unreadable link stops the walk (not disclosed).
fn chain(root: &Path, item: &str, revision: &str, chapter: &str, expected: &str, ceiling: Sensitivity,
    mut visit: impl FnMut(&Physical, &str) -> Result<bool>) -> Result<bool> {
    let source = source_locked(root, item, revision, chapter, ceiling)?;
    let (p, m) = effective(root, &source, ceiling)?.context("reader_translation_unavailable")?;
    anyhow::ensure!(m.version == expected, "stale_reader_translation");
    let mut physical = match &p.owner {
        Some(v) => Physical { kind: "owner_edit".into(), identity: p.identity.clone(), artifact_sha256: v.version.clone() },
        None => Physical { kind: "machine_translation".into(), identity: p.identity.clone(), artifact_sha256: p.metadata()?.version },
    };
    for _ in 0..=MAX_DEPTH {
        if physical.identity.revision_id != revision
            && revision_text::historical_revision(root, item, &physical.identity.revision_id, ceiling).is_err()
        {
            return Ok(true);
        }
        if physical.kind == "machine_translation" {
            let proj = stored(root, &physical.identity, &source.markdown)?.context("reader_translation_edit_unavailable")?;
            anyhow::ensure!(proj.metadata()?.version == physical.artifact_sha256, "reader_translation_edit_unavailable");
            visit(&physical, &proj.markdown)?;
            return Ok(false);
        }
        let a = load_artifact(root, &physical.identity, &physical.artifact_sha256)?;
        if !visit(&physical, &a.markdown)? {
            return Ok(false);
        }
        physical = a.owner_edit_of.physical;
    }
    anyhow::bail!("reader_translation_edit_unavailable")
}

pub fn history(library: &Path, item: &str, revision: &str, chapter: &str, expected_version: &str, ceiling: Sensitivity) -> Result<History> {
    with_consistent_snapshot(library, |root| {
        let mut entries = Vec::new();
        let truncated = chain(root, item, revision, chapter, expected_version, ceiling, |p, md| {
            entries.push(HistoryEntry {
                kind: p.kind.clone(),
                revision_id: p.identity.revision_id.clone(),
                artifact_sha256: p.artifact_sha256.clone(),
                sha256: reader_sha256(md.as_bytes()),
                bytes: md.len(),
                depth: 0,
            });
            Ok(true)
        })?;
        let n = entries.len() as u32;
        for (i, e) in entries.iter_mut().enumerate() {
            e.depth = n - 1 - i as u32;
        }
        Ok(History { translation_version: expected_version.into(), entries, truncated })
    })
}

/// Read-only compare: one chunk of an older version in the current chain.
#[allow(clippy::too_many_arguments)]
pub fn version_chunk(library: &Path, item: &str, revision: &str, chapter: &str, expected_version: &str,
    artifact_sha256: &str, offset: usize, limit: usize, ceiling: Sensitivity) -> Result<VersionChunk> {
    anyhow::ensure!((4..=READER_CHUNK_BYTES).contains(&limit), "invalid_reader_limit");
    with_consistent_snapshot(library, |root| {
        let mut found = None;
        chain(root, item, revision, chapter, expected_version, ceiling, |p, md| {
            if p.artifact_sha256 == artifact_sha256 {
                found = Some((p.clone(), md.to_owned()));
                return Ok(false);
            }
            Ok(true)
        })?;
        let (p, md) = found.context("reader_translation_edit_not_found")?;
        anyhow::ensure!(offset <= md.len() && md.is_char_boundary(offset), "invalid_reader_offset");
        let mut end = offset.saturating_add(limit).min(md.len());
        while !md.is_char_boundary(end) {
            end -= 1;
        }
        Ok(VersionChunk {
            artifact_sha256: p.artifact_sha256,
            kind: p.kind,
            revision_id: p.identity.revision_id,
            sha256: reader_sha256(md.as_bytes()),
            bytes: md.len(),
            start: offset,
            end,
            markdown: md[offset..end].into(),
            next_offset: (end < md.len()).then_some(end),
        })
    })
}

#[cfg(test)]
#[path = "edit_tests.rs"]
mod tests;
