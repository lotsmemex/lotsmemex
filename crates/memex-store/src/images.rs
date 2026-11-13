//! Revision-bound raster reads. Import Evidence stays immutable; no URL fetching
//! or client-selected filesystem path is used to supply an image.
use super::*;
use std::{collections::VecDeque, os::unix::fs::MetadataExt, sync::{Arc, Mutex, OnceLock}, time::{Duration, Instant}};

pub const IMAGE_CHUNK_BYTES: usize = 96 * 1024;
const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;

struct CachedImage {
    path: PathBuf,
    hash: String,
    stamp: (u64, u64, u64, i64, i64, i64, i64),
    bytes: Arc<Vec<u8>>,
    used: Instant,
}
static IMAGES: OnceLock<Mutex<VecDeque<CachedImage>>> = OnceLock::new();

fn verified_image(path: &Path, descriptor: &memex_core::BlobDescriptor, media: &str) -> Result<Arc<Vec<u8>>> {
    let file = open_regular_nofollow(path)?;
    let m = file.metadata()?;
    let stamp = (m.dev(), m.ino(), m.len(), m.mtime(), m.mtime_nsec(), m.ctime(), m.ctime_nsec());
    if m.len() != descriptor.byte_size { anyhow::bail!("image_read_failed") }
    let cache = IMAGES.get_or_init(Default::default);
    {
        let mut cache = cache.lock().map_err(|_| anyhow::anyhow!("image_read_failed"))?;
        cache.retain(|c| c.used.elapsed() < Duration::from_secs(60));
        if let Some(i) = cache.iter().position(|c| c.path == path && c.hash == descriptor.sha256 && c.stamp == stamp) {
            let mut hit = cache.remove(i).unwrap();
            hit.used = Instant::now();
            let bytes = hit.bytes.clone();
            if !image_signature_matches(media, &bytes) { anyhow::bail!("image_read_failed") }
            cache.push_back(hit);
            return Ok(bytes);
        }
    }
    let mut bytes = Vec::with_capacity(m.len() as usize);
    file.take(MAX_IMAGE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != descriptor.byte_size || format!("{:x}", Sha256::digest(&bytes)) != descriptor.sha256
        || !image_signature_matches(media, &bytes) { anyhow::bail!("image_read_failed") }
    let bytes = Arc::new(bytes);
    let mut cache = cache.lock().map_err(|_| anyhow::anyhow!("image_read_failed"))?;
    while cache.len() >= 32 || cache.iter().map(|c| c.bytes.len()).sum::<usize>() + bytes.len() > 128 * 1024 * 1024 { cache.pop_front(); }
    cache.push_back(CachedImage { path: path.into(), hash: descriptor.sha256.clone(), stamp, bytes: bytes.clone(), used: Instant::now() });
    Ok(bytes)
}

fn image_path(root: &Path, relative: &Path) -> Result<PathBuf> {
    let mut path = root.to_path_buf();
    for component in relative.components() {
        if !matches!(component, std::path::Component::Normal(_)) { anyhow::bail!("image_read_failed") }
        path.push(component);
        if fs::symlink_metadata(&path)?.file_type().is_symlink() { anyhow::bail!("image_read_failed") }
    }
    contained_existing(root, &path)
}

pub struct ImageChunk {
    pub media_type: String,
    pub sha256: String,
    pub total_bytes: usize,
    pub offset: usize,
    pub next_offset: Option<usize>,
    pub bytes: Vec<u8>,
}

pub fn read_image(
    library: impl AsRef<Path>, item_id: &str, expected_revision_id: &str,
    target: &str, offset: usize, ceiling: Sensitivity,
) -> Result<ImageChunk> {
    // Import conversion and captured attachments both use this content-addressed
    // relative Markdown target. Do not interpret URL escapes, '..' or file URLs.
    let name = target.strip_prefix("attachments/").unwrap_or("");
    let (hash, ext) = name.split_once('.').unwrap_or(("", ""));
    let hash = hash.strip_prefix("sha256-").unwrap_or(hash);
    let media = match ext {
        "png" => "image/png", "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif", "webp" => "image/webp",
        _ => anyhow::bail!("image_resource_not_found"),
    };
    if !valid_lower_hex_64(hash) { anyhow::bail!("image_resource_not_found") }
    with_consistent_snapshot(library, |root| {
        if !valid_item_id(item_id) || trash::is_trashed(root, item_id)? { anyhow::bail!("image_resource_not_found") }
        image_path(root, &Path::new("items").join(item_id))?;
        // Reverify the current revision and its relationship on every chunk,
        // but verify only the attachment being disclosed, not every other image.
        let (item, revision) = match load_current_pointer(root, item_id)? {
            Some((pointer, revision)) => (pointer.item, revision),
            None => read_legacy_revision(root, item_id)?,
        };
        if item.sensitivity.max(revision.sensitivity) > ceiling {
            anyhow::bail!("image_resource_not_found")
        }
        if revision.revision_id != expected_revision_id { anyhow::bail!("stale_item_revision") }
        let mut found = None;
        for id in &revision.evidence_ids {
            let evidence: Evidence = load(root, root.join("evidence").join(id).join("evidence.json"))?;
            if evidence.blob_id != format!("sha256:{hash}") { continue }
            let Some(descriptor) = &evidence.descriptor else { continue };
            if evidence.evidence_id != *id || evidence.schema_version != SCHEMA_VERSION
                || evidence.sensitivity > ceiling || descriptor.schema_version != SCHEMA_VERSION
                || descriptor.sha256 != hash || evidence.logical_locator != descriptor.locator {
                anyhow::bail!("image_read_failed")
            }
            let path = if let Some(AttachmentRef::Evidence { relative_path, .. }) = revision.attachments.iter()
                .find(|a| matches!(a, AttachmentRef::Evidence { evidence_id, relative_path, .. } if evidence_id == id && relative_path == target)) {
                validate_attachment_evidence(&item, id, relative_path, &evidence)?;
                image_path(root, &Path::new("items").join(item_id).join(relative_path))?
            } else if let Some(relative) = evidence.backend_ref.strip_prefix("memex-local-import:") {
                // The old offline importer retained assets in the data root's
                // sealed Imports tree, with generic binary MIME descriptors.
                // Exact Evidence binding, not a guessed original export path.
                let parts: Vec<_> = Path::new(relative).components().collect();
                if descriptor.locator != relative || parts.len() != 5
                    || !parts.iter().all(|c| matches!(c, std::path::Component::Normal(_)))
                    || parts[0].as_os_str() != "Imports" || parts[2].as_os_str() != "raw"
                    || parts[3].as_os_str() != "objects" || parts[4].as_os_str() != hash {
                    anyhow::bail!("image_read_failed")
                }
                let data = root.parent().context("image_read_failed")?;
                image_path(data, Path::new(relative))?
            } else { continue };
            found = Some((path, descriptor.clone()));
            break;
        }
        let (path, descriptor) = found.ok_or_else(|| anyhow::anyhow!("image_resource_not_found"))?;
        if descriptor.byte_size == 0 || descriptor.byte_size > MAX_IMAGE_BYTES {
            anyhow::bail!("image_too_large")
        }
        let total = descriptor.byte_size as usize;
        if offset >= total { anyhow::bail!("invalid_image_range") }
        let end = offset.saturating_add(IMAGE_CHUNK_BYTES).min(total);
        let verified = verified_image(&path, &descriptor, media)?;
        Ok(ImageChunk { media_type: media.into(), sha256: hash.into(), total_bytes: total,
            offset, next_offset: (end < total).then_some(end), bytes: verified[offset..end].to_vec() })
    })
    .map_err(|e| match e.to_string().as_str() {
        "image_resource_not_found" | "stale_item_revision" | "image_too_large" | "invalid_image_range" => e,
        _ => anyhow::anyhow!("image_read_failed"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use memex_core::{BlobDescriptor, VerifiedBlob};

    #[test]
    fn imported_image_reads_are_revision_scoped_chunked_and_verified() {
        let dir = tempfile::tempdir().unwrap();
        let library = dir.path().join("Library");
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend((0..IMAGE_CHUNK_BYTES + 31).map(|i| (i % 251) as u8));
        let hash = format!("{:x}", Sha256::digest(&bytes));
        let relative = format!("Imports/synthetic/raw/objects/{hash}");
        let path = dir.path().join(&relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &bytes).unwrap();
        let target = format!("attachments/{hash}.png");
        let mut capture = crate::tests::capture();
        capture.capture.sensitivity = Sensitivity::Private;
        let descriptor = BlobDescriptor { schema_version: 1, locator: relative.clone(),
            media_type: "application/octet-stream".into(), byte_size: bytes.len() as u64,
            sha256: hash.clone(), source_url: None };
        capture.blobs = vec![VerifiedBlob { descriptor: descriptor.clone(), blob_id: format!("sha256:{hash}"),
            source_backend_ref: Some(format!("memex-local-import:{relative}")), bytes: bytes.clone() }];
        capture.capture.artifacts = vec![descriptor];
        let mut draft = crate::tests::draft();
        draft.sensitivity = Sensitivity::Private;
        draft.content_markdown = format!("Before\n\n![Diagram]({target})\n\nAfter");
        let receipt = ingest(&library, &capture, draft.clone()).unwrap();
        let id = &receipt.item_ids[0];
        let revision = read_item(&library, id).unwrap().1.revision_id;
        let read = |target: &str, offset, ceiling| read_image(&library, id, &revision, target, offset, ceiling);
        let first = read(&target, 0, Sensitivity::Private).unwrap();
        assert_eq!(first.bytes, bytes[..IMAGE_CHUNK_BYTES]);
        assert_eq!(first.next_offset, Some(IMAGE_CHUNK_BYTES));
        let last = read(&target, IMAGE_CHUNK_BYTES, Sensitivity::Private).unwrap();
        assert_eq!(last.bytes, bytes[IMAGE_CHUNK_BYTES..]);
        assert_eq!(last.next_offset, None);
        assert_eq!(last.sha256, hash);
        assert_eq!(last.media_type, "image/png");
        assert!(read(&target, bytes.len(), Sensitivity::Private).is_err());
        assert!(read(&target, 0, Sensitivity::Public).is_err());
        assert!(read_image(&library, id, "revision_stale", &target, 0, Sensitivity::Private).is_err());
        for invalid in [format!("../{target}"), format!("file:///{target}"), target.replace(".png", ".svg"), format!("attachments/{}.png", "0".repeat(64))] {
            assert!(read(&invalid, 0, Sensitivity::Private).is_err());
        }
        capture.capture.external_capture_id = "other".into();
        capture.blobs.clear(); capture.capture.artifacts.clear();
        let other = ingest(&library, &capture, draft).unwrap();
        let other_revision = read_item(&library, &other.item_ids[0]).unwrap().1.revision_id;
        assert!(read_image(&library, &other.item_ids[0], &other_revision, &target, 0, Sensitivity::Private).is_err());
        let mut bad = bytes.clone(); bad[20] ^= 1;
        fs::write(&path, &bad).unwrap();
        assert!(read(&target, 0, Sensitivity::Private).is_err());
        fs::remove_file(&path).unwrap();
        let outside = dir.path().join("outside.png");
        fs::write(&outside, &bytes).unwrap();
        #[cfg(unix)] {
            std::os::unix::fs::symlink(&outside, &path).unwrap();
            assert!(read(&target, 0, Sensitivity::Private).is_err());
        }
    }
}
