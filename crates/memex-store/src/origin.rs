//! Read the immutable creation receipt, not the current URL, title or folder.
use super::*;
use memex_core::ItemOrigin;

/// Owner-local projection; call under a consistent Store snapshot. Later
/// metadata edits and file reconversion never change the creation origin.
pub fn item_origin(root: &Path, item_id: &str) -> Result<Option<ItemOrigin>> {
    let Some((capture, _)) = creating_source(root, item_id)? else { return Ok(None) };
    Ok(match capture.capture_method.as_str() {
        "owner_authorized_offline_import" | "legacy_normalized_jsonl_v0" => Some(ItemOrigin::Import),
        "safe_http_fetch" | "public_article_api_fetch" | "browser_snapshot_v1" => Some(ItemOrigin::Capture),
        _ => None,
    })
}

/// Immutable creation binding shared by origin and date projection. This does
/// not authorize disclosure; the caller must apply current and source ceilings.
pub(crate) fn creating_source(root: &Path, item_id: &str) -> Result<Option<(CaptureRecord, Vec<Evidence>)>> {
    let (_, initial) = read_legacy_item(root, item_id)?;
    let Some(evidence_id) = initial.evidence_ids.first() else {
        return Ok(None);
    };
    if !safe_component(evidence_id, "evidence_") {
        anyhow::bail!("invalid origin evidence");
    }
    let evidence: Evidence = load(
        root,
        root.join("evidence").join(evidence_id).join("evidence.json"),
    )?;
    if evidence.evidence_id != *evidence_id || !safe_component(&evidence.capture_id, "capture_") {
        anyhow::bail!("invalid origin evidence");
    }
    let capture: CaptureRecord = load(
        root,
        root.join("captures").join(&evidence.capture_id).join("capture.json"),
    )?;
    let receipt: IngestReceipt = load(
        root,
        root.join("receipts").join(format!("{}.json", evidence.capture_id)),
    )?;
    if capture.schema_version != SCHEMA_VERSION
        || evidence.schema_version != SCHEMA_VERSION
        || receipt.schema_version != SCHEMA_VERSION
        || capture.capture_id != evidence.capture_id
        || capture.source_id != evidence.source_id
        || receipt.capture_id != capture.capture_id
        || !receipt.item_ids.iter().any(|id| id == item_id)
        || !receipt.revision_ids.contains(&initial.revision_id)
        || receipt.evidence_ids != initial.evidence_ids
    {
        anyhow::bail!("invalid origin receipt");
    }
    let mut evidence_records = Vec::with_capacity(initial.evidence_ids.len());
    for id in &initial.evidence_ids {
        if !safe_component(id, "evidence_") { anyhow::bail!("invalid origin evidence") }
        let record: Evidence = load(root, root.join("evidence").join(id).join("evidence.json"))?;
        if record.schema_version != SCHEMA_VERSION || record.evidence_id != *id
            || record.capture_id != capture.capture_id || record.source_id != capture.source_id
            || !capture.blob_ids.contains(&record.blob_id)
        { anyhow::bail!("invalid origin evidence") }
        evidence_records.push(record);
    }
    Ok(Some((capture, evidence_records)))
}
