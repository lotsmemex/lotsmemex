//! Explicit, capability-gated webpage collection. Never performs network I/O.
use super::*;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WebOptions {
    pub target_group_id: Option<String>,
}

pub(super) fn http_error(error: &anyhow::Error) -> HttpError {
    match error.to_string().as_str() {
        "request_identity_conflict" | "web_duplicate_conflict" | "web_content_conflict" => HttpError::new(409),
        "group_read_only" => HttpError::new(403),
        "library_resource_not_found" | "invalid_web_target" => HttpError::new(422),
        _ => HttpError::new(500),
    }
}

/// Accept the same V1 binary envelope as Chrome. Dry runs admit and translate all
/// bytes without creating a library, spool, receipt, or membership.
pub fn collect_binary(library: &Path, snapshots: Option<&Path>, bytes: &[u8], dry_run: bool) -> Result<serde_json::Value> {
    anyhow::ensure!(bytes.len() as u64 <= MAX_REQUEST, "capture_size_limit");
    let mut input = std::io::Cursor::new(bytes);
    let (capture, metadata, attachments, _, comments, quality, options) = prepare_binary(&mut input, bytes.len() as u64)?;
    anyhow::ensure!(input.position() == bytes.len() as u64, "trailing_capture_bytes");
    let options = options.context("web_snapshot_required")?;
    anyhow::ensure!(comments.is_none() && quality.is_none(), "unsupported_web_options");
    let source_url = capture.capture.source_url.clone();
    let prepared = memex_capture::prepare_capture(
        memex_capture::CaptureSubmission { capture, metadata, attachments },
        memex_capture::AdmissionContext::default(),
    )?;
    if dry_run {
        return Ok(serde_json::json!({"schema_version":1,"dry_run":true,"source_url":source_url,
            "target_group_id":options.target_group_id,"admission":prepared.admission()}));
    }
    let library = safe_root(library, false)?;
    let snapshots = safe_root(snapshots.context("snapshots_directory_required")?, true)?;
    anyhow::ensure!(!library.starts_with(&snapshots) && !snapshots.starts_with(&library), "storage roots overlap");
    for blob in prepared.source_blobs() {
        install_snapshot(&snapshots, &blob.descriptor.sha256, &blob.bytes)?;
    }
    Ok(serde_json::to_value(memex_capture::commit_web_capture(&library, prepared, options.target_group_id.as_deref())?)?)
}
