//! Read-only `added_at` (采集日期) policy and its guarded legacy backfill.
//!
//! Owner decision (bibliographic_metadata_v1 draft4): for capture-origin items
//! the clipping time *is* the MemeX entry time, so `added_at` is the
//! UTC-normalized `captured_at` of the capture that first successfully created
//! the item. New items carry it on the immutable `Item` record written by the
//! creation transaction (and frozen in its ingest journal).
//!
//! Items created earlier have no value there. Their `item.json` is pinned byte
//! for byte by the committed ingest journal and is never rewritten, so the
//! backfill persists one immutable, create-once record
//! `items/<item_id>/added_at.json` through the Store writer lock. It is the
//! only stored added_at for that item: the backfill never writes it when the
//! header has a value, and readers ignore nothing silently (a malformed record
//! fails the read). Readers take the header first, then a verified record.

use super::{
    canonical_root, catalog_keys, ingest_lock, install_binary, load, load_verified_item_header,
    recover_if_pending_locked, safe_component, safe_receipt_path, valid_item_id, CaptureRecord,
    IngestReceipt, Item, ItemRevision,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const SIDE_RECORD: &str = "added_at.json";
const SIDE_RECORD_SCHEMA: u32 = 1;
const ORIGIN_CAPTURE_BACKFILL: &str = "capture_backfill";
const PREVIEW_KIND: &str = "memex.added_at_backfill_preview";
const RECEIPT_KIND: &str = "memex.added_at_backfill_receipt";
const BACKFILL_SCHEMA: u32 = 1;
const RECEIPT_DIR: &str = "catalog/added-at-backfill";

#[cfg(test)]
thread_local! {
    /// Test-only creation override: `Some(None)` reproduces the pre-added_at
    /// writer, `Some(Some(v))` an earlier writer that stored another value.
    pub(crate) static INGEST_ADDED_AT_OVERRIDE: std::cell::RefCell<Option<Option<String>>> =
        const { std::cell::RefCell::new(None) };
}

/// UTC-normalized RFC3339 form of a capture's `captured_at`, or `None` when the
/// capture carries no parseable time (never substituted with another clock).
pub fn from_capture_time(captured_at: &str) -> Option<String> {
    let parsed = chrono::DateTime::parse_from_rfc3339(captured_at).ok()?;
    let value = parsed
        .with_timezone(&chrono::Utc)
        .to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true);
    memex_core::valid_added_at(&value).then_some(value)
}

/// Value persisted by the capture ingest creation transaction.
pub(crate) fn for_capture_creation(captured_at: &str) -> Option<String> {
    #[cfg(test)]
    if let Some(value) = INGEST_ADDED_AT_OVERRIDE.with(|value| value.borrow().clone()) {
        return value;
    }
    from_capture_time(captured_at)
}

/// Hook for the future Zotero import: a newly materialized item's added_at is
/// the import commit time, taken under the writer lock inside its creation
/// transaction (never Zotero dateAdded/dateModified or file times).
pub fn for_import_commit() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

fn same_instant(a: &str, b: &str) -> bool {
    match (
        chrono::DateTime::parse_from_rfc3339(a),
        chrono::DateTime::parse_from_rfc3339(b),
    ) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SideRecord {
    schema_version: u32,
    item_id: String,
    added_at: String,
    origin: String,
    capture_id: String,
    captured_at: String,
    preview_id: String,
}

fn side_record_path(root: &Path, item_id: &str) -> PathBuf {
    root.join("items").join(item_id).join(SIDE_RECORD)
}

fn capture_path(root: &Path, capture_id: &str) -> PathBuf {
    root.join("captures").join(capture_id).join("capture.json")
}

fn valid_preview_id(value: &str) -> bool {
    value
        .strip_prefix("addedatpreview_")
        .is_some_and(|rest| rest.len() == 32 && rest.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn load_side_record(root: &Path, item_id: &str) -> Result<Option<SideRecord>> {
    let path = side_record_path(root, item_id);
    match fs::symlink_metadata(&path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let record: SideRecord = load(root, path)?;
    if record.schema_version != SIDE_RECORD_SCHEMA
        || record.item_id != item_id
        || record.origin != ORIGIN_CAPTURE_BACKFILL
        || !safe_component(&record.capture_id, "capture_")
        || !valid_preview_id(&record.preview_id)
        || from_capture_time(&record.captured_at).as_deref() != Some(record.added_at.as_str())
    {
        anyhow::bail!("invalid added_at record");
    }
    let capture: CaptureRecord = load(root, capture_path(root, &record.capture_id))?;
    if capture.capture_id != record.capture_id || capture.captured_at != record.captured_at {
        anyhow::bail!("added_at record does not match its capture");
    }
    Ok(Some(record))
}

/// The item's stored `added_at`: the creation-time header value, else a
/// verified backfill record, else `None` (never derived at read time).
/// The caller supplies the verified Item from `read_item` under a consistent snapshot.
pub fn effective(root: &Path, item: &Item) -> Result<Option<String>> {
    if item.added_at.is_some() {
        if load_side_record(root, &item.item_id)?.is_some() {
            anyhow::bail!("added_at stored twice");
        }
        return Ok(item.added_at.clone());
    }
    Ok(load_side_record(root, &item.item_id)?.map(|record| record.added_at))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AddedAtBackfillStatus {
    SetFromCapture,
    AlreadyPresent,
    MissingCaptureTime,
    Conflict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddedAtSourceIdentity {
    pub source_id: String,
    pub adapter_id: String,
    pub external_capture_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddedAtSourceRef {
    pub capture_id: String,
    pub receipt: String,
    pub capture_sha256: String,
}

/// Canonical item version the preview row was computed against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddedAtItemBinding {
    pub current_revision_id: String,
    pub revision_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddedAtBackfillRow {
    pub item_id: String,
    pub source_identity: Option<AddedAtSourceIdentity>,
    pub captured_at: Option<String>,
    pub current_added_at: Option<String>,
    pub proposed_added_at: Option<String>,
    pub source_ref: Option<AddedAtSourceRef>,
    pub status: AddedAtBackfillStatus,
    pub reason: Option<String>,
    pub binding: Option<AddedAtItemBinding>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddedAtStatusCounts {
    pub set_from_capture: usize,
    pub already_present: usize,
    pub missing_capture_time: usize,
    pub conflict: usize,
}

/// Dry-run output. Writes nothing to the library; apply consumes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddedAtBackfillPreview {
    pub kind: String,
    pub schema_version: u32,
    pub preview_id: String,
    pub created_at: String,
    pub total_items: usize,
    pub status_counts: AddedAtStatusCounts,
    /// sha256 over the serialized rows; apply refuses edited previews.
    pub rows_sha256: String,
    pub rows: Vec<AddedAtBackfillRow>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AddedAtApplyResult {
    Applied,
    NoOp,
    Conflict,
    /// The item, its capture binding or its added_at changed after preview.
    SkippedStale,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddedAtApplyRow {
    pub item_id: String,
    pub status: AddedAtBackfillStatus,
    pub result: AddedAtApplyResult,
    pub added_at: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddedAtApplyCounts {
    pub applied: usize,
    pub no_op: usize,
    pub conflict: usize,
    pub skipped_stale: usize,
}

/// Durable apply receipt at `catalog/added-at-backfill/<preview_id>.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddedAtBackfillReceipt {
    pub kind: String,
    pub schema_version: u32,
    pub preview_id: String,
    pub rows_sha256: String,
    pub applied_at: String,
    pub status_counts: AddedAtStatusCounts,
    pub apply_counts: AddedAtApplyCounts,
    pub library_receipt: String,
    pub rows: Vec<AddedAtApplyRow>,
    /// True when an identical earlier apply's receipt is returned unchanged.
    #[serde(default)]
    pub replayed: bool,
}

struct CreatingCapture {
    receipt_path: String,
    creation_revision_id: String,
    capture: CaptureRecord,
    capture_sha256: String,
}

/// Item id -> capture records whose ingest receipt created that item.
fn creating_captures(root: &Path) -> Result<BTreeMap<String, Vec<CreatingCapture>>> {
    let mut map: BTreeMap<String, Vec<CreatingCapture>> = BTreeMap::new();
    let receipt_paths: BTreeSet<String> = catalog_keys(root)?.into_values().collect();
    for receipt_path in receipt_paths {
        let receipt: IngestReceipt = load(root, safe_receipt_path(root, &receipt_path)?)?;
        if receipt_path != format!("receipts/{}.json", receipt.capture_id)
            || !safe_component(&receipt.capture_id, "capture_")
            || receipt.item_ids.len() != 1
            || receipt.revision_ids.len() != 1
        {
            anyhow::bail!("invalid ingest receipt {receipt_path}");
        }
        let path = capture_path(root, &receipt.capture_id);
        let capture: CaptureRecord = load(root, path.clone())?;
        if capture.capture_id != receipt.capture_id {
            anyhow::bail!("ingest receipt capture mismatch {receipt_path}");
        }
        let capture_sha256 = format!("sha256:{:x}", Sha256::digest(fs::read(&path)?));
        map.entry(receipt.item_ids[0].clone())
            .or_default()
            .push(CreatingCapture {
                receipt_path,
                creation_revision_id: receipt.revision_ids[0].clone(),
                capture,
                capture_sha256,
            });
    }
    Ok(map)
}

fn item_ids(root: &Path) -> Result<Vec<String>> {
    let mut ids = Vec::new();
    let items = root.join("items");
    match fs::symlink_metadata(&items) {
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
            anyhow::bail!("unsafe items directory")
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(ids),
        Err(error) => return Err(error.into()),
    }
    for entry in fs::read_dir(&items)? {
        let name = entry?.file_name();
        if let Some(name) = name.to_str().filter(|name| valid_item_id(name)) {
            ids.push(name.to_owned());
        }
    }
    ids.sort();
    Ok(ids)
}

/// One preview row, computed only from Store-verified state.
fn classify(
    root: &Path,
    item_id: &str,
    captures: &[CreatingCapture],
) -> (AddedAtBackfillRow, Option<SideRecord>) {
    let mut row = AddedAtBackfillRow {
        item_id: item_id.to_owned(),
        source_identity: None,
        captured_at: None,
        current_added_at: None,
        proposed_added_at: None,
        source_ref: None,
        status: AddedAtBackfillStatus::Conflict,
        reason: None,
        binding: None,
    };
    let verified = super::read_item(root, item_id)
        .and_then(|_| load_verified_item_header(root, item_id))
        .and_then(|(item, revision_sha256)| {
            Ok((
                effective(root, &item)?,
                item.current_revision,
                revision_sha256,
            ))
        });
    let Ok((current, current_revision_id, revision_sha256)) = verified else {
        row.reason = Some("item_unreadable".into());
        return (row, None);
    };
    row.current_added_at = current.clone();
    row.binding = Some(AddedAtItemBinding {
        current_revision_id,
        revision_sha256,
    });
    let finish = |mut row: AddedAtBackfillRow, status, reason: &str| {
        row.status = status;
        row.reason = Some(reason.to_owned());
        (row, None)
    };
    // Source binding first: only one authoritative creating capture qualifies.
    let creating = match captures {
        [] => {
            return finish(
                row,
                AddedAtBackfillStatus::Conflict,
                "source_policy_mismatch",
            )
        }
        [one] => one,
        _ => {
            return finish(
                row,
                AddedAtBackfillStatus::Conflict,
                "ambiguous_creating_capture",
            )
        }
    };
    let capture = &creating.capture;
    row.source_identity = Some(AddedAtSourceIdentity {
        source_id: capture.source_id.clone(),
        adapter_id: capture.adapter_id.clone(),
        external_capture_id: capture.external_capture_id.clone(),
    });
    row.source_ref = Some(AddedAtSourceRef {
        capture_id: capture.capture_id.clone(),
        receipt: creating.receipt_path.clone(),
        capture_sha256: creating.capture_sha256.clone(),
    });
    // The creating revision's translate activity is written with the capture
    // time; disagreement means the source binding cannot be trusted.
    let creation: Result<ItemRevision> = load(
        root,
        root.join("items")
            .join(item_id)
            .join("revisions")
            .join(format!("{}.json", creating.creation_revision_id)),
    );
    let binding_agrees = creation.is_ok_and(|creation| {
        creation.revision_number == 1
            && creation.item_id == item_id
            && creation
                .provenance
                .iter()
                .find(|activity| activity.activity_type == "translate")
                .is_some_and(|activity| activity.occurred_at == capture.captured_at)
    });
    if !binding_agrees {
        return finish(
            row,
            AddedAtBackfillStatus::Conflict,
            "incompatible_source_binding",
        );
    }
    if !capture.captured_at.is_empty() {
        row.captured_at = Some(capture.captured_at.clone());
    }
    let Some(proposed) = from_capture_time(&capture.captured_at) else {
        let reason = if capture.captured_at.trim().is_empty() {
            "capture_time_missing"
        } else {
            "capture_time_invalid"
        };
        // Any existing value is retained without claiming agreement.
        return finish(row, AddedAtBackfillStatus::MissingCaptureTime, reason);
    };
    row.proposed_added_at = Some(proposed.clone());
    match current {
        Some(existing) if same_instant(&existing, &proposed) => {
            row.status = AddedAtBackfillStatus::AlreadyPresent;
            (row, None)
        }
        Some(_) => finish(
            row,
            AddedAtBackfillStatus::Conflict,
            "stored_added_at_differs",
        ),
        None => {
            row.status = AddedAtBackfillStatus::SetFromCapture;
            let record = SideRecord {
                schema_version: SIDE_RECORD_SCHEMA,
                item_id: item_id.to_owned(),
                added_at: proposed,
                origin: ORIGIN_CAPTURE_BACKFILL.to_owned(),
                capture_id: capture.capture_id.clone(),
                captured_at: capture.captured_at.clone(),
                preview_id: String::new(),
            };
            (row, Some(record))
        }
    }
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

fn rows_sha256(preview_id: &str, rows: &[AddedAtBackfillRow]) -> Result<String> {
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&(preview_id, rows))?)
    ))
}

/// Dry run: classify every item; writes nothing to the library.
pub fn preview_added_at_backfill(library: impl AsRef<Path>) -> Result<AddedAtBackfillPreview> {
    let root = canonical_root(library.as_ref())?;
    let preview_id = memex_core::new_id("addedatpreview");
    let _lock = crate::exclusive_read_lock(&root)?;
    let captures = creating_captures(&root)?;
    let mut status_counts = AddedAtStatusCounts::default();
    let mut rows = Vec::new();
    for item_id in item_ids(&root)? {
        let found = captures.get(&item_id).map(Vec::as_slice).unwrap_or(&[]);
        let (row, _) = classify(&root, &item_id, found);
        match row.status {
            AddedAtBackfillStatus::SetFromCapture => status_counts.set_from_capture += 1,
            AddedAtBackfillStatus::AlreadyPresent => status_counts.already_present += 1,
            AddedAtBackfillStatus::MissingCaptureTime => status_counts.missing_capture_time += 1,
            AddedAtBackfillStatus::Conflict => status_counts.conflict += 1,
        }
        rows.push(row);
    }
    Ok(AddedAtBackfillPreview {
        kind: PREVIEW_KIND.into(),
        schema_version: BACKFILL_SCHEMA,
        rows_sha256: rows_sha256(&preview_id, &rows)?,
        preview_id,
        created_at: now(),
        total_items: rows.len(),
        status_counts,
        rows,
    })
}

fn receipt_path(root: &Path, preview_id: &str) -> PathBuf {
    root.join(RECEIPT_DIR).join(format!("{preview_id}.json"))
}

fn load_receipt(root: &Path, preview_id: &str) -> Result<Option<AddedAtBackfillReceipt>> {
    let path = receipt_path(root, preview_id);
    match fs::symlink_metadata(&path) {
        Ok(_) => Ok(Some(load(root, path)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Apply a dry-run preview. Only its `set_from_capture` rows may write, and
/// only when the row recomputed under the writer lock is identical to the
/// preview row (same item version, capture binding and current value);
/// otherwise that item is skipped with zero writes. Each write creates the
/// item's immutable added_at record atomically; bodies, revisions, captures,
/// Evidence and groups are untouched. Replaying the same preview returns the
/// stored receipt; a different preview body under the same id is rejected.
pub fn apply_added_at_backfill(
    library: impl AsRef<Path>,
    preview: &AddedAtBackfillPreview,
) -> Result<AddedAtBackfillReceipt> {
    let root = canonical_root(library.as_ref())?;
    if preview.kind != PREVIEW_KIND
        || preview.schema_version != BACKFILL_SCHEMA
        || !valid_preview_id(&preview.preview_id)
        || preview.total_items != preview.rows.len()
        || rows_sha256(&preview.preview_id, &preview.rows)? != preview.rows_sha256
        || preview
            .rows
            .iter()
            .map(|row| row.item_id.as_str())
            .collect::<BTreeSet<_>>()
            .len()
            != preview.rows.len()
        || preview.rows.iter().any(|row| !valid_item_id(&row.item_id))
    {
        anyhow::bail!("invalid_backfill_preview");
    }
    {
        let _lock = crate::exclusive_read_lock(&root)?;
        if let Some(mut receipt) = load_receipt(&root, &preview.preview_id)? {
            if receipt.rows_sha256 != preview.rows_sha256 {
                anyhow::bail!("backfill_preview_conflict");
            }
            receipt.replayed = true;
            return Ok(receipt);
        }
    }
    let mut rows = Vec::with_capacity(preview.rows.len());
    let mut apply_counts = AddedAtApplyCounts::default();
    // One writer lock for the whole apply: captures are scanned once instead of once per
    // row (which made a 1,222-item library take most of an hour), and nothing can change
    // between the re-classification of a row and its write.
    let apply_lock = ingest_lock(&root)?;
    let writes = preview
        .rows
        .iter()
        .any(|row| row.status == AddedAtBackfillStatus::SetFromCapture);
    let captures = if writes {
        recover_if_pending_locked(&root)?;
        creating_captures(&root)?
    } else {
        Default::default()
    };
    for planned in &preview.rows {
        let mut row = AddedAtApplyRow {
            item_id: planned.item_id.clone(),
            status: planned.status,
            result: AddedAtApplyResult::NoOp,
            added_at: planned.current_added_at.clone(),
            reason: planned.reason.clone(),
        };
        match planned.status {
            AddedAtBackfillStatus::Conflict => row.result = AddedAtApplyResult::Conflict,
            AddedAtBackfillStatus::AlreadyPresent | AddedAtBackfillStatus::MissingCaptureTime => {}
            AddedAtBackfillStatus::SetFromCapture => {
                let found = captures
                    .get(&planned.item_id)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let (current, record) = classify(&root, &planned.item_id, found);
                match record {
                    Some(mut record) if &current == planned => {
                        record.preview_id = preview.preview_id.clone();
                        let bytes = serde_json::to_vec_pretty(&record)?;
                        install_binary(&root, &side_record_path(&root, &planned.item_id), &bytes)?;
                        if load_side_record(&root, &planned.item_id)?.as_ref() != Some(&record) {
                            anyhow::bail!("added_at record verification failed");
                        }
                        row.result = AddedAtApplyResult::Applied;
                        row.added_at = Some(record.added_at);
                        row.reason = None;
                    }
                    _ => {
                        row.result = AddedAtApplyResult::SkippedStale;
                        row.added_at = current.current_added_at;
                        row.reason = Some("preview_binding_changed".into());
                    }
                }
            }
        }
        match row.result {
            AddedAtApplyResult::Applied => apply_counts.applied += 1,
            AddedAtApplyResult::NoOp => apply_counts.no_op += 1,
            AddedAtApplyResult::Conflict => apply_counts.conflict += 1,
            AddedAtApplyResult::SkippedStale => apply_counts.skipped_stale += 1,
        }
        rows.push(row);
    }
    let library_receipt = format!("{RECEIPT_DIR}/{}.json", preview.preview_id);
    let receipt = AddedAtBackfillReceipt {
        kind: RECEIPT_KIND.into(),
        schema_version: BACKFILL_SCHEMA,
        preview_id: preview.preview_id.clone(),
        rows_sha256: preview.rows_sha256.clone(),
        applied_at: now(),
        status_counts: preview.status_counts.clone(),
        apply_counts,
        library_receipt: library_receipt.clone(),
        rows,
        replayed: false,
    };
    let path = root.join(&library_receipt);
    // Still under `apply_lock`; taking the lock again here would deadlock on flock.
    let _lock = apply_lock;
    fs::create_dir_all(path.parent().context("receipt has no parent")?)?;
    if let Some(mut existing) = load_receipt(&root, &preview.preview_id)? {
        // A concurrent identical apply finished first; its receipt stands.
        if existing.rows_sha256 != preview.rows_sha256 {
            anyhow::bail!("backfill_preview_conflict");
        }
        existing.replayed = true;
        return Ok(existing);
    }
    install_binary(&root, &path, &serde_json::to_vec_pretty(&receipt)?)?;
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookup::tests::*;
    use crate::metadata_v2::tests::*;
    use crate::tests::*;
    use crate::*;
    use crate::{
        ingest, read_item, read_item_metadata_v2, revise_item_metadata, ItemMetadataUpdate,
    };
    use memex_core::{IdentifierKind, ItemIdentifier, Sensitivity};
    use tempfile::tempdir;

    fn ingest_at(
        root: &Path,
        external: &str,
        captured_at: &str,
        stored: Option<Option<&str>>,
    ) -> String {
        let mut c = capture();
        c.capture.external_capture_id = external.into();
        c.capture.captured_at = captured_at.into();
        INGEST_ADDED_AT_OVERRIDE
            .with(|value| *value.borrow_mut() = stored.map(|v| v.map(str::to_owned)));
        let receipt = ingest(root, &c, draft());
        INGEST_ADDED_AT_OVERRIDE.with(|value| *value.borrow_mut() = None);
        receipt.unwrap().item_ids[0].clone()
    }

    fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
        fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
            for entry in fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    walk(base, &entry.path(), out);
                } else {
                    let relative = entry
                        .path()
                        .strip_prefix(base)
                        .unwrap()
                        .display()
                        .to_string();
                    out.insert(relative, fs::read(entry.path()).unwrap());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(root, root, &mut out);
        out.retain(|path, _| !path.ends_with(".lock") && path != crate::GENERATION_FILE);
        out
    }

    fn row<'a>(preview: &'a AddedAtBackfillPreview, item_id: &str) -> &'a AddedAtBackfillRow {
        preview
            .rows
            .iter()
            .find(|row| row.item_id == item_id)
            .unwrap()
    }

    fn v2_added_at(root: &Path, item_id: &str) -> serde_json::Value {
        let read = read_item_metadata_v2(root, item_id, Sensitivity::Restricted)
            .unwrap()
            .unwrap();
        serde_json::to_value(&read).unwrap()["added_at"].clone()
    }

    #[test]
    fn legacy_items_read_null_added_at_byte_identically_and_edits_keep_it_absent() {
        let d = tempdir().unwrap();
        let mut c = capture();
        c.capture.external_capture_id = "legacy".into();
        INGEST_ADDED_AT_OVERRIDE.with(|value| *value.borrow_mut() = Some(None));
        let receipt = ingest(d.path(), &c, draft());
        INGEST_ADDED_AT_OVERRIDE.with(|value| *value.borrow_mut() = None);
        let receipt = receipt.unwrap();
        let item_id = receipt.item_ids[0].clone();
        let item_bytes = fs::read(d.path().join("items").join(&item_id).join("item.json")).unwrap();
        let item: Item = serde_json::from_slice(&item_bytes).unwrap();
        assert_eq!(
            serde_json::to_string_pretty(&item).unwrap().as_bytes(),
            item_bytes.as_slice()
        );
        assert!(!String::from_utf8_lossy(&item_bytes).contains("added_at"));
        // Replay by the current writer neither rejects nor backfills it.
        assert!(ingest(d.path(), &c, draft()).unwrap().idempotent_replay);
        assert_eq!(read_item(d.path(), &item_id).unwrap().0.added_at, None);
        assert_eq!(v2_added_at(d.path(), &item_id), serde_json::Value::Null);
        let read = read_item_metadata_v2(d.path(), &item_id, Sensitivity::Restricted)
            .unwrap()
            .unwrap();
        assert!(
            serde_json::to_value(&read).unwrap()["item"]
                .get("added_at")
                .is_none(),
            "old ItemReadMetadata shape"
        );
        revise_item_metadata(
            d.path(),
            &item_id,
            ItemMetadataUpdate {
                expected_revision_id: receipt.revision_ids[0].clone(),
                title: "Edited".into(),
                manual_tags: vec![],
            },
        )
        .unwrap();
        assert_eq!(read_item(d.path(), &item_id).unwrap().0.added_at, None);
        assert!(!memex_core::valid_added_at("2026-11-02 08:00:00+08:00"));
        assert!(!memex_core::valid_added_at("2026-11-02T08:00:00+08:00"));
        assert!(memex_core::valid_added_at("2026-11-02T00:00:00.123456Z"));
    }

    #[test]
    fn capture_time_normalizes_offsets_and_never_invents_a_value() {
        assert_eq!(
            from_capture_time("2014-03-05T10:00:00+08:00").as_deref(),
            Some("2014-03-05T02:00:00Z")
        );
        assert_eq!(
            from_capture_time("2026-11-01T23:59:58.250-01:00").as_deref(),
            Some("2026-11-02T00:59:58.250Z")
        );
        for bad in [
            "",
            "2014-03-05",
            "2014-03-05 10:00",
            "yesterday",
            "2023-02-29T00:00:00Z",
        ] {
            assert_eq!(from_capture_time(bad), None, "{bad}");
        }
        assert!(same_instant(
            "2014-03-05T10:00:00+08:00",
            "2014-03-05T02:00:00.000000Z"
        ));
        assert!(!same_instant(
            "2014-03-05T10:00:00Z",
            "2014-03-05T02:00:00Z"
        ));
        assert!(memex_core::valid_added_at(&for_import_commit()));
    }

    #[test]
    fn backfill_classifies_previews_without_writing_and_applies_only_bound_rows() {
        let d = tempdir().unwrap();
        let root = d.path();
        // Evernote-style import: clipped years ago in Taipei, written by the
        // pre-added_at writer.
        let evernote = ingest_at(root, "enex-1", "2014-03-05T10:00:00+08:00", Some(None));
        let stale = ingest_at(root, "legacy-2", "2019-07-01T12:00:00Z", Some(None));
        // Current writer: offset-equivalent stored value already present.
        let present = ingest_at(root, "native-3", "2026-11-01T23:59:58+08:00", None);
        // An earlier writer stored commit time: never overwritten.
        let differs = ingest_at(
            root,
            "commit-era-4",
            "2026-10-30T08:00:00Z",
            Some(Some("2026-10-31T01:02:03.000004Z")),
        );
        let invalid = ingest_at(root, "bad-time-5", "05/03/2014 10:00", Some(None));
        let missing_kept = ingest_at(root, "no-time-6", "", Some(Some("2020-01-01T00:00:00Z")));

        let before = tree(root);
        let preview = preview_added_at_backfill(root).unwrap();
        assert_eq!(tree(root), before, "dry run writes nothing");
        assert_eq!(
            preview.status_counts,
            AddedAtStatusCounts {
                set_from_capture: 2,
                already_present: 1,
                missing_capture_time: 2,
                conflict: 1,
            }
        );
        let r = row(&preview, &evernote);
        assert_eq!(r.status, AddedAtBackfillStatus::SetFromCapture);
        assert_eq!(r.proposed_added_at.as_deref(), Some("2014-03-05T02:00:00Z"));
        assert_eq!(r.captured_at.as_deref(), Some("2014-03-05T10:00:00+08:00"));
        assert_eq!(r.current_added_at, None);
        assert_eq!(
            r.source_identity.as_ref().unwrap().external_capture_id,
            "enex-1"
        );
        assert!(r
            .source_ref
            .as_ref()
            .unwrap()
            .capture_sha256
            .starts_with("sha256:"));
        assert_eq!(
            row(&preview, &present).status,
            AddedAtBackfillStatus::AlreadyPresent
        );
        assert_eq!(
            row(&preview, &present).current_added_at.as_deref(),
            Some("2026-11-01T15:59:58Z")
        );
        let r = row(&preview, &differs);
        assert_eq!(
            (r.status, r.reason.as_deref()),
            (
                AddedAtBackfillStatus::Conflict,
                Some("stored_added_at_differs")
            )
        );
        let r = row(&preview, &invalid);
        assert_eq!(
            (
                r.status,
                r.reason.as_deref(),
                r.proposed_added_at.as_deref()
            ),
            (
                AddedAtBackfillStatus::MissingCaptureTime,
                Some("capture_time_invalid"),
                None
            )
        );
        let r = row(&preview, &missing_kept);
        assert_eq!(
            (r.status, r.reason.as_deref(), r.current_added_at.as_deref()),
            (
                AddedAtBackfillStatus::MissingCaptureTime,
                Some("capture_time_missing"),
                Some("2020-01-01T00:00:00Z")
            )
        );

        // Drift after preview: an owner edit moves `stale` to a new revision.
        let (_, revision) = read_item(root, &stale).unwrap();
        revise_item_metadata(
            root,
            &stale,
            ItemMetadataUpdate {
                expected_revision_id: revision.revision_id,
                title: "Edited after preview".into(),
                manual_tags: vec![],
            },
        )
        .unwrap();
        let before_apply = tree(root);
        let before_stamps = crate::item_change_stamps(root).unwrap();
        let receipt = apply_added_at_backfill(root, &preview).unwrap();
        let after_stamps = crate::item_change_stamps(root).unwrap();
        for ((id, before), (after_id, after)) in before_stamps.iter().zip(&after_stamps) {
            assert_eq!(id, after_id);
            assert_eq!(before != after, id == &evernote, "only the backfilled item's query stamp changes");
        }
        assert_eq!(
            receipt.apply_counts,
            AddedAtApplyCounts {
                applied: 1,
                no_op: 3,
                conflict: 1,
                skipped_stale: 1,
            }
        );
        let stale_row = receipt
            .rows
            .iter()
            .find(|row| row.item_id == stale)
            .unwrap();
        assert_eq!(stale_row.result, AddedAtApplyResult::SkippedStale);
        assert_eq!(stale_row.reason.as_deref(), Some("preview_binding_changed"));
        assert!(
            !side_record_path(root, &stale).exists(),
            "zero writes for stale row"
        );

        // Only the applied item's record and the run receipt are new; no
        // existing file (item, revision, capture, Evidence, body) changed.
        let after = tree(root);
        let mut new_paths: Vec<_> = after
            .keys()
            .filter(|path| !before_apply.contains_key(*path))
            .cloned()
            .collect();
        new_paths.sort();
        assert_eq!(
            new_paths,
            vec![
                receipt.library_receipt.clone(),
                format!("items/{evernote}/added_at.json"),
            ]
        );
        for (path, bytes) in &before_apply {
            assert_eq!(after.get(path), Some(bytes), "{path} changed");
        }
        assert_eq!(v2_added_at(root, &evernote), "2014-03-05T02:00:00Z");
        assert_eq!(
            read_item(root, &evernote).unwrap().0.added_at,
            None,
            "header untouched"
        );
        assert_eq!(v2_added_at(root, &differs), "2026-10-31T01:02:03.000004Z");

        // Replaying the same preview returns the stored receipt, writes nothing.
        let replay = apply_added_at_backfill(root, &preview).unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.rows, receipt.rows);
        assert_eq!(tree(root), after);
        // Same preview id with different (re-hashed) rows is a conflict.
        let mut forged = preview.clone();
        forged.rows.retain(|row| row.item_id != stale);
        forged.total_items = forged.rows.len();
        forged.rows_sha256 = rows_sha256(&forged.preview_id, &forged.rows).unwrap();
        assert_eq!(
            apply_added_at_backfill(root, &forged)
                .unwrap_err()
                .to_string(),
            "backfill_preview_conflict"
        );
        // An edited row without a matching hash is rejected outright.
        let mut tampered = preview.clone();
        tampered.preview_id = memex_core::new_id("addedatpreview");
        tampered.rows[0].proposed_added_at = Some("1999-01-01T00:00:00Z".into());
        assert_eq!(
            apply_added_at_backfill(root, &tampered)
                .unwrap_err()
                .to_string(),
            "invalid_backfill_preview"
        );

        // A fresh preview now sees the applied item as already present and
        // can still fill the item that drifted earlier.
        let again = preview_added_at_backfill(root).unwrap();
        assert_eq!(
            row(&again, &evernote).status,
            AddedAtBackfillStatus::AlreadyPresent
        );
        assert_eq!(
            row(&again, &stale).status,
            AddedAtBackfillStatus::SetFromCapture
        );
        let second = apply_added_at_backfill(root, &again).unwrap();
        assert_eq!(second.apply_counts.applied, 1);
        assert_eq!(v2_added_at(root, &stale), "2019-07-01T12:00:00Z");

        // Later writes and physical restore keep the backfilled value.
        let (_, revision) = read_item(root, &evernote).unwrap();
        revise_item_metadata(
            root,
            &evernote,
            ItemMetadataUpdate {
                expected_revision_id: revision.revision_id,
                title: "Edited after backfill".into(),
                manual_tags: vec![],
            },
        )
        .unwrap();
        let mut c = capture();
        c.capture.external_capture_id = "enex-1".into();
        c.capture.captured_at = "2014-03-05T10:00:00+08:00".into();
        assert!(ingest(root, &c, draft()).unwrap().idempotent_replay);
        assert_eq!(v2_added_at(root, &evernote), "2014-03-05T02:00:00Z");
        let restored = tempdir().unwrap();
        for (path, bytes) in tree(root) {
            let target = restored.path().join(path);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(target, bytes).unwrap();
        }
        assert_eq!(
            v2_added_at(restored.path(), &evernote),
            "2014-03-05T02:00:00Z"
        );
    }

    #[test]
    fn backfill_refuses_unknown_or_ambiguous_source_bindings() {
        let d = tempdir().unwrap();
        let root = d.path();
        let unknown = ingest_at(root, "orphan-1", "2015-01-01T00:00:00Z", Some(None));
        let ambiguous = ingest_at(root, "twice-2", "2016-01-01T00:00:00Z", Some(None));
        let keys_path = root.join("catalog/ingest-keys.json");
        let mut keys: BTreeMap<String, String> =
            serde_json::from_slice(&fs::read(&keys_path).unwrap()).unwrap();
        let receipt_of = |item: &str| {
            keys.iter()
                .find(|(_, path)| {
                    let receipt: IngestReceipt =
                        serde_json::from_slice(&fs::read(root.join(path.as_str())).unwrap())
                            .unwrap();
                    receipt.item_ids == [item.to_owned()]
                })
                .map(|(key, path)| (key.clone(), path.clone()))
                .unwrap()
        };
        // No creating capture: unknown origin (e.g. a Zotero-imported item).
        let (unknown_key, _) = receipt_of(&unknown);
        // Two captures disagreeing about the same item's creation.
        let (_, twice_path) = receipt_of(&ambiguous);
        keys.remove(&unknown_key);
        let mut receipt: IngestReceipt =
            serde_json::from_slice(&fs::read(root.join(&twice_path)).unwrap()).unwrap();
        let mut record: CaptureRecord =
            serde_json::from_slice(&fs::read(capture_path(root, &receipt.capture_id)).unwrap())
                .unwrap();
        let other = memex_core::new_id("capture");
        receipt.capture_id = other.clone();
        record.capture_id = other.clone();
        record.captured_at = "2017-01-01T00:00:00Z".into();
        fs::create_dir_all(root.join("captures").join(&other)).unwrap();
        fs::write(
            capture_path(root, &other),
            serde_json::to_vec_pretty(&record).unwrap(),
        )
        .unwrap();
        let other_path = format!("receipts/{other}.json");
        fs::write(
            root.join(&other_path),
            serde_json::to_vec_pretty(&receipt).unwrap(),
        )
        .unwrap();
        keys.insert("sha256:synthetic-second-capture".into(), other_path);
        fs::write(&keys_path, serde_json::to_vec_pretty(&keys).unwrap()).unwrap();

        let preview = preview_added_at_backfill(root).unwrap();
        let r = row(&preview, &unknown);
        assert_eq!(
            (
                r.status,
                r.reason.as_deref(),
                r.proposed_added_at.as_deref()
            ),
            (
                AddedAtBackfillStatus::Conflict,
                Some("source_policy_mismatch"),
                None
            )
        );
        let r = row(&preview, &ambiguous);
        assert_eq!(
            (r.status, r.reason.as_deref()),
            (
                AddedAtBackfillStatus::Conflict,
                Some("ambiguous_creating_capture")
            )
        );
        let before = tree(root);
        let receipt = apply_added_at_backfill(root, &preview).unwrap();
        assert_eq!(receipt.apply_counts.conflict, 2);
        assert_eq!(receipt.apply_counts.applied, 0);
        let after = tree(root);
        assert_eq!(after.len(), before.len() + 1, "only the receipt is written");
        assert_eq!(v2_added_at(root, &unknown), serde_json::Value::Null);
    }

    #[test]
    fn added_at_is_creating_capture_time_and_survives_every_later_write() {
        let d = tempdir().unwrap();
        let mut c = capture();
        // A failed attempt (rejected before the creation transaction) sets nothing.
        let mut failed = c.clone();
        failed.capture.external_capture_id = "attempt-failed".into();
        failed.capture.captured_at = "2026-10-01T00:00:00Z".into();
        failed.source.source_id = "../escape".into();
        assert!(ingest(d.path(), &failed, draft()).is_err());
        // Clipped just before midnight Taipei time, committed the next day:
        // added_at is that capture instant, normalized to UTC.
        c.capture.captured_at = "2026-11-01T23:59:58.250+08:00".into();
        let receipt = ingest(d.path(), &c, draft()).unwrap();
        let item_id = receipt.item_ids[0].clone();
        let added = read_item(d.path(), &item_id).unwrap().0.added_at.unwrap();
        assert_eq!(added, "2026-11-01T15:59:58.250Z");
        assert!(memex_core::valid_added_at(&added), "{added}");
        assert_eq!(list_items(d.path()).unwrap().len(), 1);
        // A retry of the same capture carrying a different time cannot redate it.
        let mut retried = c.clone();
        retried.capture.captured_at = "2026-11-03T00:00:00Z".into();
        let _ = ingest(d.path(), &retried, draft());
        assert_eq!(
            read_item(d.path(), &item_id).unwrap().0.added_at.as_deref(),
            Some(added.as_str())
        );
        let v2 = read_item_metadata_v2(d.path(), &item_id, Sensitivity::Restricted)
            .unwrap()
            .unwrap();
        assert_eq!(v2.added_at.as_deref(), Some(added.as_str()));
        assert_eq!(v2.item.added_at.as_deref(), Some(added.as_str()));

        let keep = |label: &str| {
            assert_eq!(
                read_item(d.path(), &item_id).unwrap().0.added_at.as_deref(),
                Some(added.as_str()),
                "{label}"
            );
        };
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(ingest(d.path(), &c, draft()).unwrap().idempotent_replay);
        keep("replay");
        let legacy = revise_item_metadata(
            d.path(),
            &item_id,
            ItemMetadataUpdate {
                expected_revision_id: receipt.revision_ids[0].clone(),
                title: "Edited".into(),
                manual_tags: vec![],
            },
        )
        .unwrap();
        keep("legacy edit");
        let v2edit = revise_item_metadata_v2(
            d.path(),
            &item_id,
            v2_edit(
                &item_id,
                &legacy.revision_id,
                "added-1",
                serde_json::json!({
                    "identifiers": [{"kind": "doi", "value": "10.1000/synth(1)"}]
                }),
                &[],
            ),
            Sensitivity::Restricted,
        )
        .unwrap();
        keep("v2 edit");
        let preview = lookup_preview_for(d.path(), &item_id, &v2edit.revision_id);
        let mut apply = apply_edit(
            &item_id,
            &v2edit.revision_id,
            "added-2",
            &preview,
            vec![candidate(&preview, "journal_title", None)],
        );
        apply.preview_id = Some(preview.preview_id.clone());
        revise_item_metadata_v2(d.path(), &item_id, apply, Sensitivity::Restricted).unwrap();
        keep("lookup apply");
        let group = edit_groups(
            d.path(),
            "added-g",
            GroupEditOp::CreateRoot {
                name: "G".into(),
                sensitivity: Sensitivity::Public,
            },
            Sensitivity::Restricted,
        )
        .unwrap();
        edit_groups(
            d.path(),
            "added-m",
            GroupEditOp::AddMembers {
                group_id: group.group_id,
                expected_version: group.version,
                item_ids: vec![item_id.clone()],
            },
            Sensitivity::Restricted,
        )
        .unwrap();
        keep("group membership");
        assert!(
            find_identifier_duplicates(
                d.path(),
                &[ItemIdentifier::new(IdentifierKind::Doi, "10.1000/synth(1)")],
                Sensitivity::Restricted,
                None
            )
            .unwrap()
            .len()
                == 1
        );
        keep("duplicate check");
        // Physical relocation/restore of the library keeps it (it is data, not mtime).
        let moved = tempdir().unwrap();
        let target = moved.path().join("restored");
        copy_tree(d.path(), &target);
        assert_eq!(
            read_item(&target, &item_id).unwrap().0.added_at.as_deref(),
            Some(added.as_str())
        );

        // An edit chain that changes added_at is rejected.
        let current = d.path().join("items").join(&item_id).join("current.json");
        let mut pointer: serde_json::Value =
            serde_json::from_slice(&fs::read(&current).unwrap()).unwrap();
        pointer["item"]["added_at"] = "2000-01-01T00:00:00.000000Z".into();
        fs::write(&current, serde_json::to_vec_pretty(&pointer).unwrap()).unwrap();
        assert!(read_item(d.path(), &item_id).is_err());
    }

    fn copy_tree(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_tree(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), &target).unwrap();
            }
        }
    }

    #[test]
    fn added_at_is_frozen_in_the_journal_through_crash_recovery() {
        let d = tempdir().unwrap();
        let c = capture();
        FAIL_APPLY_STEP.with(|value| value.set(Some(0)));
        assert!(ingest(d.path(), &c, draft()).is_err());
        FAIL_APPLY_STEP.with(|value| value.set(None));
        let path = journal_path(d.path(), &c);
        let journal: IngestJournal = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let entry = journal
            .entries
            .iter()
            .find(|e| e.path.ends_with("/item.json"))
            .unwrap();
        let staged: Item =
            serde_json::from_slice(&fs::read(path.parent().unwrap().join(&entry.staged)).unwrap())
                .unwrap();
        let frozen = staged.added_at.clone().unwrap();
        assert_eq!(
            Some(frozen.clone()),
            added_at::from_capture_time(&c.capture.captured_at)
        );
        assert_eq!(recover_pending_ingests(d.path()).unwrap(), 1);
        let replay = ingest(d.path(), &c, draft()).unwrap();
        assert!(replay.idempotent_replay);
        let item = read_item(d.path(), &replay.item_ids[0]).unwrap().0;
        assert_eq!(item.added_at.as_deref(), Some(frozen.as_str()));
    }
}
