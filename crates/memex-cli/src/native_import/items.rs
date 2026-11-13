//! Frozen offline records -> ordinary Store ingest journals. No new reader schema.
use super::*;
use memex_core::{
    BlobDescriptor, CaptureEnvelope, ItemDraft, ItemMetadata, SourceProfile, VerifiedBlob,
    VerifiedCapture,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::path::Component;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Blob {
    path: String,
    sha256: String,
    media_type: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    key: String,
    title: String,
    item_type: String,
    source_url: String,
    metadata: ItemMetadata,
    body: Blob,
    evidence: Vec<Blob>,
    status: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    schema_version: u32,
    source: SourceProfile,
    records: Vec<Blob>,
}

#[derive(Deserialize)]
struct ReceiptRow {
    #[serde(rename = "key")]
    _key: String,
    #[serde(rename = "status")]
    _status: String,
    #[serde(rename = "receipt")]
    _receipt: memex_core::IngestReceipt,
    #[serde(rename = "visible_revision")]
    _visible_revision: Option<String>,
}

// Metadata text fields are single-line in the existing Store contract. Preserve
// all source formatting in raw Evidence and the Markdown body; normalize only
// the structured projection, never truncate the reading text.
fn metadata_projection(value: ItemMetadata) -> Result<ItemMetadata> {
    fn clean(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::String(text) => {
                *text = text
                    .chars()
                    .map(|c| if c.is_control() { ' ' } else { c })
                    .collect::<String>()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
            }
            serde_json::Value::Array(values) => values.iter_mut().for_each(clean),
            serde_json::Value::Object(values) => values.values_mut().for_each(clean),
            _ => {}
        }
    }
    let mut json = serde_json::to_value(value)?;
    clean(&mut json);
    let mut value: ItemMetadata = serde_json::from_value(json)?;
    if let Some(tags) = &mut value.manual_tags {
        tags.value.retain(|tag| !tag.is_empty());
        tags.value.sort();
        tags.value.dedup();
        if tags.value.is_empty() {
            value.manual_tags = None;
        }
    }
    Ok(value)
}

fn object_path(root: &Path, blob: &Blob) -> Result<PathBuf> {
    let relative = Path::new(&blob.path);
    ensure!(
        !relative.as_os_str().is_empty()
            && relative
                .components()
                .all(|p| matches!(p, Component::Normal(_))),
        "unsafe import locator"
    );
    let mut path = root.to_owned();
    for part in relative.components() {
        path.push(part);
        ensure!(
            !fs::symlink_metadata(&path)?.file_type().is_symlink(),
            "symlink import locator"
        );
    }
    ensure!(fs::metadata(&path)?.is_file(), "non-file import locator");
    Ok(path)
}

fn read(root: &Path, blob: &Blob) -> Result<Vec<u8>> {
    let bytes = fs::read(object_path(root, blob)?)?;
    ensure!(
        format!("{:x}", Sha256::digest(&bytes)) == blob.sha256,
        "import input hash mismatch"
    );
    Ok(bytes)
}

// A 500-record request must not retain 500 large PDF originals simultaneously.
// Count both the Markdown draft and its Evidence copy. An oversized record is
// accepted alone; this is a buffering budget, not a new import exclusion.
const BATCH_PAYLOAD_BYTES: u64 = 64 * 1024 * 1024;

fn sync_staged_intent(file: &fs::File) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        loop {
            if unsafe { libc::fsync(file.as_raw_fd()) } == 0 {
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error.into());
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        file.sync_all()?;
        Ok(())
    }
}

fn flush_batch(
    library: &Path,
    intents: &Path,
    output: &mut fs::File,
    pending: &mut Vec<(VerifiedCapture, ItemDraft)>,
    identities: &mut Vec<(String, String)>,
    staged_intents: &mut Vec<(tempfile::TempPath, PathBuf)>,
) -> Result<()> {
    if pending.is_empty() {
        return Ok(());
    }
    // Flush every staged timestamp to the device before publishing ANY name.
    // A second full barrier makes those names durable before Store can allocate
    // identities. A crash before publication leaves only disposable temp files.
    fs::File::open(intents)?.sync_all()?;
    for (temporary, path) in staged_intents.drain(..) {
        temporary
            .persist_noclobber(path)
            .context("intent_publish_failed")?;
    }
    fs::File::open(intents)?.sync_all()?;
    let committed = memex_store::ingest_batch(library, std::mem::take(pending))?;
    for ((key, status), receipt) in identities.drain(..).zip(committed) {
        let visible = memex_store::read_item_at_sensitivity(
            library,
            &receipt.item_ids[0],
            Sensitivity::Private,
        )?;
        writeln!(
            output,
            "{}",
            serde_json::json!({"key": key, "status": status,
            "receipt": receipt, "visible_revision": visible.map(|(_, r)| r.revision_id)})
        )?;
    }
    // Canonical receipts are already durable; lost export rows replay safely.
    output.sync_all()?;
    Ok(())
}

pub(super) fn apply(plan_path: &Path, library: &Path, receipts: &Path) -> Result<()> {
    let root = plan_path
        .parent()
        .context("missing bundle root")?
        .canonicalize()?;
    let plan: Plan = serde_json::from_slice(&fs::read(plan_path)?)?;
    ensure!(
        plan.schema_version == 1 && plan.source.default_sensitivity == Sensitivity::Private,
        "unsupported native import plan"
    );
    ensure!(
        plan.source.source_kind == "zotero_library",
        "unexpected native import source"
    );
    let mut options = fs::OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let writer = options.open(root.join(".import.lock"))?;
    writer.try_lock().context("import_writer_busy")?;
    let mut records = Vec::new();
    let mut keys = std::collections::BTreeSet::new();
    for blob in &plan.records {
        let record: Record = serde_json::from_slice(&read(&root, blob)?)?;
        ensure!(
            keys.insert(record.key.clone()) && !record.key.is_empty(),
            "duplicate native source identity"
        );
        records.push(record);
    }
    // Intent timestamps are frozen before the first Store attempt, never taken
    // from Zotero dateAdded. A resumed prepared journal retains the same time.
    let intents = root.join("intents");
    if !intents.exists() {
        fs::create_dir(&intents)?;
    }
    ensure!(
        !fs::symlink_metadata(&intents)?.file_type().is_symlink(),
        "unsafe import intents"
    );
    fs::File::open(&root)?.sync_all()?;
    let mut options = fs::OpenOptions::new();
    options.create(true).read(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options.open(receipts)?;
    output.try_lock().context("receipt_writer_busy")?;
    let previous = fs::read(receipts)?;
    let complete = previous
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(0, |i| i + 1);
    // A newline is the receipt commit marker. Never ignore a corrupt completed
    // row; only discard a torn final append, whose Store receipt is replayable.
    for line in previous[..complete].split_inclusive(|b| *b == b'\n') {
        serde_json::from_slice::<ReceiptRow>(line).context("invalid_completed_import_receipt")?;
    }
    if complete != previous.len() {
        output.set_len(complete as u64)?;
        output.sync_all()?;
    }
    let mut pending = Vec::new();
    let mut identities = Vec::new();
    let mut staged_intents = Vec::new();
    let mut pending_bytes = 0u64;
    for (index, record) in records.into_iter().enumerate() {
        // Inspect sizes before loading another potentially large original.
        let mut bytes = fs::metadata(object_path(&root, &record.body)?)?.len();
        for blob in std::iter::once(&record.body).chain(record.evidence.iter()) {
            bytes = bytes.saturating_add(fs::metadata(object_path(&root, blob)?)?.len());
        }
        if !pending.is_empty() && pending_bytes.saturating_add(bytes) > BATCH_PAYLOAD_BYTES {
            flush_batch(
                library,
                &intents,
                &mut output,
                &mut pending,
                &mut identities,
                &mut staged_intents,
            )?;
            pending_bytes = 0;
            eprintln!("imported_or_replayed={index}");
        }
        let content_markdown = String::from_utf8(read(&root, &record.body)?)?;
        ensure!(!content_markdown.trim().is_empty(), "empty imported body");
        let intent_path = intents.join(format!("{:x}.json", Sha256::digest(record.key.as_bytes())));
        let timestamp = if intent_path.exists() {
            serde_json::from_slice::<String>(&fs::read(&intent_path)?)?
        } else {
            let value = memex_store::added_at_for_import_commit();
            let mut pending = tempfile::NamedTempFile::new_in(&intents)?;
            pending.write_all(&serde_json::to_vec(&value)?)?;
            sync_staged_intent(pending.as_file())?;
            staged_intents.push((pending.into_temp_path(), intent_path));
            value
        };
        let mut blobs = Vec::new();
        for blob in std::iter::once(&record.body).chain(record.evidence.iter()) {
            let bytes = read(&root, blob)?;
            blobs.push(VerifiedBlob {
                descriptor: BlobDescriptor {
                    schema_version: 1,
                    locator: blob.path.clone(),
                    media_type: blob.media_type.clone(),
                    byte_size: bytes.len() as u64,
                    sha256: blob.sha256.clone(),
                    source_url: None,
                },
                blob_id: format!("sha256:{}", blob.sha256),
                source_backend_ref: Some(format!(
                    "memex-native-import:{}/{}",
                    root.display(),
                    blob.path
                )),
                bytes,
            });
        }
        let capture = VerifiedCapture {
            source: plan.source.clone(),
            capture: CaptureEnvelope {
                schema_version: 1,
                adapter_id: "memex.zotero-native-import".into(),
                adapter_version: "1".into(),
                external_capture_id: record.key.clone(),
                source_id: plan.source.source_id.clone(),
                captured_at: timestamp,
                capture_method: "owner_authorized_offline_import".into(),
                sensitivity: Sensitivity::Private,
                route_summary: None,
                title: Some(record.title.clone()),
                source_url: record.source_url.clone(),
                canonical_url: None,
                artifacts: blobs.iter().map(|b| b.descriptor.clone()).collect(),
            },
            blobs,
        };
        let draft = ItemDraft {
            item_type: record.item_type,
            title: record.title,
            source_url: record.source_url,
            canonical_url: None,
            content_markdown,
            sensitivity: Sensitivity::Private,
            translator_id: "memex.zotero-native-import".into(),
            translator_version: "1".into(),
            metadata: metadata_projection(record.metadata)?,
            attachments: vec![],
        };
        pending.push((capture, draft));
        identities.push((record.key, record.status));
        pending_bytes = pending_bytes.saturating_add(bytes);
        if pending.len() == memex_store::MAX_INGEST_BATCH_ITEMS || index + 1 == plan.records.len()
        {
            flush_batch(
                library,
                &intents,
                &mut output,
                &mut pending,
                &mut identities,
                &mut staged_intents,
            )?;
            pending_bytes = 0;
            eprintln!("imported_or_replayed={}", index + 1);
        }
    }
    println!("{}", serde_json::json!({"processed": plan.records.len()}));
    Ok(())
}
