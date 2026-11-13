use crate::{revision_ref, ItemRevisionRef};
use anyhow::{bail, Result};
use memex_core::{Sensitivity, SCHEMA_VERSION};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

pub const MAX_CONTEXT_BYTES: usize = 16 * 1024;
// Leave room for the UDS envelope, including JSON escaping and source metadata.
pub const MAX_CONTEXT_JSON_BYTES: usize = 192 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextReadRequest {
    pub item_id: String,
    pub expected_revision_id: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub max_sensitivity: Sensitivity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextSlice {
    pub schema_version: u32,
    pub input: ItemRevisionRef,
    pub origin: String,
    pub coordinate_space: String,
    pub content_sha256: String,
    pub total_bytes: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub content_markdown: String,
    pub source_url: String,
    pub canonical_url: Option<String>,
    pub sensitivity: Sensitivity,
}

/// Read an exact half-open UTF-8 byte range from the current verified revision.
///
/// The ceiling narrows a local owner's read, not authentication for a remote
/// caller. Remote adapters must impose their own authorized ceiling. Store
/// recovery and verification happen under its snapshot lock; no model is used.
/// Coordinates address derived Markdown, never original HTML/PDF Evidence.
pub fn context_read(
    library: impl AsRef<Path>,
    request: &ContextReadRequest,
) -> Result<ContextSlice> {
    if request.item_id.is_empty()
        || request.item_id.len() > 256
        || request.expected_revision_id.is_empty()
        || request.expected_revision_id.len() > 256
        || request.start_byte >= request.end_byte
    {
        bail!("invalid_context_request");
    }
    if request.end_byte - request.start_byte > MAX_CONTEXT_BYTES {
        bail!("context_range_too_large");
    }
    let pair = memex_store::with_consistent_snapshot(library, |root| {
        memex_store::read_item_at_sensitivity(root, &request.item_id, request.max_sensitivity)
    })
    // Do not leak storage paths, contents or integrity diagnostics to consumers.
    .map_err(|_| anyhow::anyhow!("context_read_failed"))?;
    let (item, revision) = pair.ok_or_else(|| anyhow::anyhow!("context_resource_not_found"))?;
    let sensitivity = item.sensitivity.max(revision.sensitivity);
    if sensitivity > request.max_sensitivity {
        bail!("context_resource_not_found");
    }
    if revision.revision_id != request.expected_revision_id {
        bail!("stale_item_revision");
    }
    let text = revision
        .content_markdown
        .get(request.start_byte..request.end_byte)
        .ok_or_else(|| anyhow::anyhow!("invalid_context_range"))?;
    let result = ContextSlice {
        schema_version: SCHEMA_VERSION,
        input: revision_ref(&item.item_id, &revision)?,
        origin: "machine_projection".into(),
        coordinate_space: "revision_markdown_utf8_v1".into(),
        content_sha256: format!("sha256:{:x}", Sha256::digest(&revision.content_markdown)),
        total_bytes: revision.content_markdown.len(),
        start_byte: request.start_byte,
        end_byte: request.end_byte,
        content_markdown: text.to_owned(),
        source_url: revision.source_url,
        canonical_url: revision.canonical_url,
        sensitivity,
    };
    if serde_json::to_vec(&result)?.len() > MAX_CONTEXT_JSON_BYTES {
        bail!("result_too_large");
    }
    Ok(result)
}
