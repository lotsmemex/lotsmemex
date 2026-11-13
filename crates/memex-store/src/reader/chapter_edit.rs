//! Explicit owner edits of one real edition chapter; never model-generated edits.
use super::*;

pub(crate) const ACTIVITY: &str = "owner_reader_chapter_edit_v1";
const PROOF: &str = "application/vnd.memex.owner-chapter-edit+json";
pub const MAX_CHUNK: usize = 8192;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Begin {
    pub item_id: String,
    pub revision_id: String,
    pub side: String,
    pub chapter: String,
    pub expected_chapter_sha256: String,
    pub request_id: String,
    pub bytes: usize,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub item_id: String,
    pub request_id: String,
    pub revision_id: String,
    pub side: String,
    pub chapter: String,
    pub chapter_sha256: String,
    pub changed: bool,
}

#[derive(Serialize)]
pub struct Upload {
    pub edit_id: String,
    pub received_bytes: usize,
    pub max_chunk_bytes: usize,
    pub state: String,
    pub receipt: Option<Receipt>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stage {
    schema_version: u32,
    principal: String,
    begin: Begin,
    received_bytes: usize,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Proof {
    schema_version: u32,
    principal: String,
    begin: Begin,
}

fn validate(begin: &Begin, principal: &str) -> Result<()> {
    anyhow::ensure!(
        valid_item_id(&begin.item_id)
            && valid_revision_id(&begin.revision_id)
            && matches!(begin.side.as_str(), "original" | "translation")
            && !begin.chapter.trim().is_empty()
            && begin.chapter.len() <= 128
            && !begin.request_id.is_empty()
            && begin.request_id.len() <= 128
            && begin
                .request_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            && valid_lower_hex_64(&begin.expected_chapter_sha256)
            && valid_lower_hex_64(&begin.sha256)
            && (principal == "local_owner" || valid_lower_hex_64(principal)),
        "invalid_reader_edit"
    );
    anyhow::ensure!(
        begin.bytes > 0 && begin.bytes <= MAX_TEXT,
        "reader_edit_too_large"
    );
    Ok(())
}

fn edit_id(principal: &str, request: &str) -> String {
    format!(
        "edit_{}",
        reader_sha256(&serde_json::to_vec(&(principal, request)).unwrap())
    )
}

fn stage_path(root: &Path, id: &str) -> Result<PathBuf> {
    anyhow::ensure!(
        id.strip_prefix("edit_").is_some_and(valid_lower_hex_64),
        "invalid_reader_edit"
    );
    Ok(root.join("catalog/reader-edits").join(format!("{id}.json")))
}

fn stage(
    root: &Path,
    item: &str,
    id: &str,
    principal: &str,
    ceiling: Sensitivity,
) -> Result<Stage> {
    anyhow::ensure!(valid_item_id(item), "invalid_reader_edit");
    read_item_at_sensitivity(root, item, ceiling)?.context("library_resource_not_found")?;
    let path = stage_path(root, id)?;
    let meta = fs::symlink_metadata(&path).map_err(|_| anyhow::anyhow!("reader_edit_not_found"))?;
    anyhow::ensure!(
        !meta.file_type().is_symlink() && meta.is_file() && meta.len() < 1024 * 1024,
        "invalid_reader_edit"
    );
    let s: Stage = load(root, path)?;
    validate(&s.begin, &s.principal)?;
    anyhow::ensure!(
        s.schema_version == 1
            && s.principal == principal
            && s.begin.item_id == item
            && edit_id(principal, &s.begin.request_id) == id,
        "reader_edit_not_found"
    );
    anyhow::ensure!(s.received_bytes <= s.begin.bytes, "invalid_reader_edit");
    Ok(s)
}

fn request_identity(begin: &Begin, principal: &str) -> Result<(String, String)> {
    Ok((
        format!("chapter-{}", edit_id(principal, &begin.request_id)),
        format!(
            "sha256:{}",
            reader_sha256(&serde_json::to_vec(&(ACTIVITY, principal, begin))?)
        ),
    ))
}

fn replay(root: &Path, begin: &Begin, principal: &str) -> Result<Option<Receipt>> {
    let (id, hash) = request_identity(begin, principal)?;
    let Some(r) = metadata_v2::load_metadata_request(root, &id)? else {
        return Ok(None);
    };
    anyhow::ensure!(
        r.request_sha256 == hash && r.receipt.item_id == begin.item_id,
        "request_id_conflict"
    );
    Ok(Some(Receipt {
        item_id: begin.item_id.clone(),
        request_id: begin.request_id.clone(),
        changed: r.receipt.revision_id != begin.revision_id,
        revision_id: r.receipt.revision_id,
        side: begin.side.clone(),
        chapter: begin.chapter.clone(),
        chapter_sha256: begin.sha256.clone(),
    }))
}

fn selected<'a>(
    doc: &'a ReaderDocument,
    bodies: &'a BTreeMap<String, (String, Option<String>)>,
    begin: &Begin,
) -> Result<(&'a Chapter, &'a str)> {
    let edition = doc
        .editions
        .iter()
        .find(|e| e.side == begin.side)
        .context("reader_side_not_found")?;
    let ch = edition
        .chapters
        .iter()
        .find(|c| c.key == begin.chapter)
        .context("reader_chapter_not_found")?;
    let text = bodies
        .get(&begin.side)
        .context("reader_side_not_found")?
        .0
        .get(ch.start..ch.end)
        .context("invalid_reader_edit")?;
    Ok((ch, text))
}

fn current(root: &Path, begin: &Begin, ceiling: Sensitivity) -> Result<()> {
    let (doc, bodies) = document_locked(root, &begin.item_id, &begin.revision_id, ceiling)?;
    anyhow::ensure!(
        reader_sha256(selected(&doc, &bodies, begin)?.1.as_bytes())
            == begin.expected_chapter_sha256,
        "stale_item_revision"
    );
    Ok(())
}

pub fn begin(
    library: &Path,
    request: Begin,
    principal: &str,
    ceiling: Sensitivity,
) -> Result<Upload> {
    validate(&request, principal)?;
    let root = existing_canonical_root(library)?;
    let _lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    read_item_at_sensitivity(&root, &request.item_id, ceiling)?
        .context("library_resource_not_found")?;
    let id = edit_id(principal, &request.request_id);
    if let Some(receipt) = replay(&root, &request, principal)? {
        return Ok(Upload {
            edit_id: id,
            received_bytes: request.bytes,
            max_chunk_bytes: MAX_CHUNK,
            state: "committed".into(),
            receipt: Some(receipt),
        });
    }
    current(&root, &request, ceiling)?;
    let path = stage_path(&root, &id)?;
    let received = match fs::symlink_metadata(&path) {
        Ok(_) => {
            let s = stage(&root, &request.item_id, &id, principal, ceiling)?;
            anyhow::ensure!(s.begin == request, "request_id_conflict");
            s.received_bytes
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let parent = ensure_contained_dir(&root, Path::new("catalog/reader-edits"))?;
            #[cfg(unix)]
            fs::set_permissions(parent, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
            durable_replace(
                &root,
                &path,
                &serde_json::to_vec(&Stage {
                    schema_version: 1,
                    principal: principal.into(),
                    begin: request,
                    received_bytes: 0,
                })?,
            )?;
            0
        }
        Err(e) => return Err(e.into()),
    };
    Ok(Upload {
        edit_id: id,
        received_bytes: received,
        max_chunk_bytes: MAX_CHUNK,
        state: "uploading".into(),
        receipt: None,
    })
}

pub fn chunk(
    library: &Path,
    item: &str,
    id: &str,
    offset: usize,
    markdown: &str,
    principal: &str,
    ceiling: Sensitivity,
) -> Result<usize> {
    anyhow::ensure!(
        !markdown.is_empty() && markdown.len() <= MAX_CHUNK && !markdown.contains('\0'),
        "invalid_reader_edit"
    );
    let root = existing_canonical_root(library)?;
    let _lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    let mut s = stage(&root, item, id, principal, ceiling)?;
    let received = s.received_bytes;
    anyhow::ensure!(
        offset <= received && markdown.len() <= s.begin.bytes.saturating_sub(offset),
        "reader_edit_upload_conflict"
    );
    let path = contained_output_path(&root, &stage_path(&root, id)?.with_extension("md"))?;
    if offset < received {
        use std::io::{Read, Seek, SeekFrom};
        anyhow::ensure!(
            offset + markdown.len() <= received,
            "reader_edit_upload_conflict"
        );
        let mut file = fs::File::open(&path)?;
        file.seek(SeekFrom::Start(offset as u64))?;
        let mut prior = vec![0; markdown.len()];
        file.read_exact(&mut prior)?;
        anyhow::ensure!(prior == markdown.as_bytes(), "reader_edit_upload_conflict");
        return Ok(received);
    }
    current(&root, &s.begin, ceiling)?;
    anyhow::ensure!(
        replay(&root, &s.begin, principal)?.is_none(),
        "reader_edit_upload_conflict"
    );
    use std::io::{Seek, SeekFrom, Write};
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    anyhow::ensure!(
        file.metadata()?.len() >= received as u64,
        "invalid_reader_edit"
    );
    // Discard only an unacknowledged tail after an interrupted chunk write.
    file.set_len(received as u64)?;
    file.seek(SeekFrom::Start(received as u64))?;
    file.write_all(markdown.as_bytes())?;
    file.sync_all()?;
    s.received_bytes += markdown.len();
    durable_replace(&root, &stage_path(&root, id)?, &serde_json::to_vec(&s)?)?;
    Ok(received + markdown.len())
}

fn prepare(
    doc: &ReaderDocument,
    bodies: &BTreeMap<String, (String, Option<String>)>,
    begin: &Begin,
    markdown: &str,
) -> Result<(BookManifest, BTreeMap<String, (String, Option<String>)>)> {
    anyhow::ensure!(
        doc.item_id == begin.item_id
            && doc.revision_id == begin.revision_id
            && reader_sha256(selected(doc, bodies, begin)?.1.as_bytes())
                == begin.expected_chapter_sha256,
        "stale_item_revision"
    );
    anyhow::ensure!(
        markdown.len() == begin.bytes
            && reader_sha256(markdown.as_bytes()) == begin.sha256
            && !markdown.trim().is_empty()
            && !markdown.contains('\0'),
        "invalid_reader_edit"
    );
    let mut manifest = BookManifest {
        schema_version: 1,
        alignment: doc.alignment.clone(),
        editions: doc.editions.clone(),
    };
    let mut next = bodies.clone();
    let edition = manifest
        .editions
        .iter_mut()
        .find(|e| e.side == begin.side)
        .context("reader_side_not_found")?;
    let pos = edition
        .chapters
        .iter()
        .position(|c| c.key == begin.chapter)
        .context("reader_chapter_not_found")?;
    let ch = &edition.chapters[pos];
    let body = &mut next.get_mut(&begin.side).unwrap().0;
    anyhow::ensure!(
        body.len() - (ch.end - ch.start) + markdown.len() <= MAX_TEXT,
        "reader_edit_too_large"
    );
    body.replace_range(ch.start..ch.end, markdown);
    let mut offset = ch.start;
    for (i, ch) in edition.chapters.iter_mut().enumerate().skip(pos) {
        let len = if i == pos {
            markdown.len()
        } else {
            ch.end - ch.start
        };
        ch.start = offset;
        offset += len;
        ch.end = offset;
    }
    edition.bytes = body.len();
    edition.sha256 = reader_sha256(body.as_bytes());
    validate_edition(edition, body)?;
    Ok((manifest, next))
}

pub(crate) fn validate_transition(base: &ItemRevision, next: &ItemRevision) -> Result<()> {
    repair::validate_transition_for(base, next, ACTIVITY)
}

pub(crate) fn validate_evidence(
    root: &Path,
    base: &ItemRevision,
    next: &ItemRevision,
) -> Result<()> {
    repair::validate_bundle(root, base, next, ACTIVITY, PROOF)?;
    let evidence: Evidence = load(
        root,
        root.join("evidence")
            .join(next.evidence_ids.last().unwrap())
            .join("evidence.json"),
    )?;
    let proof: Proof = serde_json::from_slice(&blob(root, &evidence, 8192)?)?;
    anyhow::ensure!(proof.schema_version == 1, "invalid_reader_edit");
    validate(&proof.begin, &proof.principal)?;
    let (new_doc, new_bodies) = document_revision_locked(root, next, next.sensitivity)?;
    let (_, new_text) = selected(&new_doc, &new_bodies, &proof.begin)?;
    let (old_doc, old_bodies) = document_revision_locked(root, base, base.sensitivity)?;
    let (manifest, expected) = prepare(&old_doc, &old_bodies, &proof.begin, new_text)?;
    let activity = next.provenance.last().unwrap();
    anyhow::ensure!(
        new_doc.editions == manifest.editions
            && new_doc.alignment == manifest.alignment
            && next.content_markdown == expected["translation"].0
            && new_bodies
                .iter()
                .all(|(side, body)| expected.get(side).is_some_and(|e| e.0 == body.0))
            && activity.input_checksums[1]
                == format!(
                    "sha256:{}",
                    reader_sha256(expected["original"].0.as_bytes())
                )
            && activity.input_checksums[2]
                == format!(
                    "sha256:{}",
                    reader_sha256(expected["translation"].0.as_bytes())
                ),
        "invalid_reader_edit"
    );
    Ok(())
}

pub fn committed(
    library: &Path,
    item_id: &str,
    id: &str,
    principal: &str,
    ceiling: Sensitivity,
) -> Result<Option<Receipt>> {
    let root = existing_canonical_root(library)?;
    let _lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    let s = stage(&root, item_id, id, principal, ceiling)?;
    replay(&root, &s.begin, principal)
}

pub fn commit(
    library: &Path,
    item_id: &str,
    id: &str,
    principal: &str,
    ceiling: Sensitivity,
) -> Result<Receipt> {
    let root = existing_canonical_root(library)?;
    let _lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    let s = stage(&root, item_id, id, principal, ceiling)?;
    if let Some(receipt) = replay(&root, &s.begin, principal)? {
        return Ok(receipt);
    }
    let (item, base) =
        read_item_at_sensitivity(&root, item_id, ceiling)?.context("library_resource_not_found")?;
    anyhow::ensure!(
        s.received_bytes == s.begin.bytes,
        "reader_edit_upload_conflict"
    );
    let path = contained_output_path(&root, &stage_path(&root, id)?.with_extension("md"))?;
    anyhow::ensure!(
        fs::metadata(&path)?.len() == s.begin.bytes as u64,
        "invalid_reader_edit"
    );
    let bytes = fs::read(path)?;
    let markdown = String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("invalid_reader_edit"))?;
    let (doc, bodies) = document_revision_locked(&root, &base, ceiling)?;
    let (manifest, next_bodies) = prepare(&doc, &bodies, &s.begin, &markdown)?;
    let (request_id, request_hash) = request_identity(&s.begin, principal)?;
    if selected(&doc, &bodies, &s.begin)?.1 == markdown {
        metadata_v2::write_metadata_request(
            &root,
            &MetadataEditRequestRecord {
                schema_version: 1,
                request_id,
                request_sha256: request_hash,
                receipt: ItemMetadataUpdateReceipt {
                    schema_version: 1,
                    item_id: item_id.into(),
                    previous_revision_id: base.revision_id.clone(),
                    revision_id: base.revision_id.clone(),
                    revision_number: base.revision_number,
                },
            },
        )?;
    } else {
        let (_, legacy) = read_legacy_item(&root, item_id)?;
        anyhow::ensure!(
            base.revision_number - legacy.revision_number < MAX_ITEM_REVISION_CHAIN,
            "item_revision_limit_exceeded"
        );
        let mut next =
            owner_edit_revision(&base, ACTIVITY, base.title.clone(), base.metadata.clone())?;
        next.content_markdown = next_bodies["translation"].0.clone();
        let proof = Proof {
            schema_version: 1,
            principal: principal.into(),
            begin: s.begin.clone(),
        };
        let source: Evidence = load(
            &root,
            root.join("evidence")
                .join(&base.evidence_ids[0])
                .join("evidence.json"),
        )?;
        let capture_id = new_id("capture");
        let payloads = [
            (
                "text/markdown",
                next_bodies["original"].0.as_bytes().to_vec(),
            ),
            (
                "text/markdown",
                next_bodies["translation"].0.as_bytes().to_vec(),
            ),
            (BOOK_MANIFEST, serde_json::to_vec(&manifest)?),
            (PROOF, serde_json::to_vec(&proof)?),
        ];
        ensure_contained_dir(&root, Path::new("reader-blobs"))?;
        let mut blob_ids = Vec::new();
        for (media, bytes) in payloads {
            let sha = reader_sha256(&bytes);
            let evidence_id = new_id("evidence");
            let relative = format!("reader-blobs/{sha}");
            let e = Evidence {
                schema_version: 1,
                evidence_id: evidence_id.clone(),
                source_id: source.source_id.clone(),
                capture_id: capture_id.clone(),
                blob_id: format!("sha256:{sha}"),
                logical_locator: relative.clone(),
                backend_ref: format!("memex-store:{relative}"),
                sensitivity: base.sensitivity,
                supersedes: None,
                descriptor: Some(memex_core::BlobDescriptor {
                    schema_version: 1,
                    locator: relative.clone(),
                    media_type: media.into(),
                    byte_size: bytes.len() as u64,
                    sha256: sha,
                    source_url: Some(base.source_url.clone()),
                }),
            };
            install_binary(&root, &root.join(relative), &bytes)?;
            let dir = ensure_contained_dir(&root, Path::new(&format!("evidence/{evidence_id}")))?;
            install_binary(&root, &dir.join("evidence.json"), &serde_json::to_vec(&e)?)?;
            next.evidence_ids.push(evidence_id.clone());
            let a = next.provenance.last_mut().unwrap();
            a.input_ids.push(evidence_id);
            a.input_checksums.push(e.blob_id.clone());
            blob_ids.push(e.blob_id);
        }
        let capture = CaptureRecord {
            schema_version: 1,
            capture_id: capture_id.clone(),
            adapter_id: "memex.desktop".into(),
            adapter_version: "1".into(),
            external_capture_id: next.revision_id.clone(),
            source_id: source.source_id,
            captured_at: next.provenance.last().unwrap().occurred_at.clone(),
            capture_method: ACTIVITY.into(),
            route_summary: Some("owner selected chapter edit".into()),
            source_url: base.source_url.clone(),
            canonical_url: base.canonical_url.clone(),
            sensitivity: base.sensitivity,
            blob_ids,
            ingest_input_sha256: None,
            evidence_descriptor_version: Some(1),
        };
        let dir = ensure_contained_dir(&root, Path::new(&format!("captures/{capture_id}")))?;
        install_binary(
            &root,
            &dir.join("capture.json"),
            &serde_json::to_vec(&capture)?,
        )?;
        validate_evidence(&root, &base, &next)?;
        commit_owner_revision_locked(&root, item, &base, &next, Some((request_id, request_hash)))?;
    }
    replay(&root, &s.begin, principal)?.context("invalid_reader_edit")
}
