//! Only revision-bound, verified local raster bytes may reach a renderer.
use crate::{Client, ClientError, Command, Sensitivity};
use base64::Engine;
use serde::Deserialize;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Deserialize)]
pub struct ImageChunk {
    pub item_id: String,
    pub revision_id: String,
    pub target: String,
    pub media_type: String,
    pub sha256: String,
    pub total_bytes: usize,
    pub offset: usize,
    pub next_offset: Option<usize>,
    pub data_base64: String,
}

pub fn local_image_target(target: &str) -> Option<(&str, &str)> {
    let (hash, ext) = target.strip_prefix("attachments/")?.split_once('.')?;
    let hash = hash.strip_prefix("sha256-").unwrap_or(hash);
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) { return None }
    let media = match ext { "png" => "image/png", "jpg" | "jpeg" => "image/jpeg", "gif" => "image/gif", "webp" => "image/webp", _ => return None };
    Some((hash, media))
}

impl Client {
    pub fn image_read(&self, item: &str, revision: &str, target: &str) -> Result<Vec<u8>, ClientError> {
        let (hash, media) = local_image_target(target).ok_or(ClientError::InvalidFrame)?;
        let mut bytes = Vec::new();
        let mut total = None;
        loop {
            let r = self.backend.send(&Command::ImageRead { item_id: item.into(), expected_revision_id: revision.into(),
                target: target.into(), offset: bytes.len(), max_sensitivity: Sensitivity::Restricted })?.image.ok_or(ClientError::InvalidFrame)?;
            if r.item_id != item || r.revision_id != revision || r.target != target || r.sha256 != hash || r.media_type != media
                || r.offset != bytes.len() || r.total_bytes == 0 || r.total_bytes > 64 * 1024 * 1024
                || total.is_some_and(|n| n != r.total_bytes) || r.data_base64.len() > 131072 {
                return Err(ClientError::InvalidFrame);
            }
            total = Some(r.total_bytes);
            let chunk = base64::engine::general_purpose::STANDARD.decode(r.data_base64).map_err(|_| ClientError::InvalidFrame)?;
            if chunk.is_empty() || chunk.len() > 96 * 1024 || bytes.len() + chunk.len() > r.total_bytes { return Err(ClientError::InvalidFrame) }
            bytes.extend(chunk);
            let next = (bytes.len() < r.total_bytes).then_some(bytes.len());
            if next != r.next_offset { return Err(ClientError::InvalidFrame) }
            if next.is_none() { break }
        }
        if format!("{:x}", Sha256::digest(&bytes)) != hash { return Err(ClientError::InvalidFrame) }
        Ok(bytes)
    }
}
