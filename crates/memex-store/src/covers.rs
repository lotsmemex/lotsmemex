//! Independent owner-local cover resources. Never changes an item revision,
//! Evidence or attachments. Network fetching and episode verification belong to
//! the intake worker; no path or arbitrary URL is accepted by resource reads.
use super::*;
use image::{ImageFormat, ImageReader};
use std::io::Cursor;

pub const MAX_COVER_BYTES: usize = 5 * 1024 * 1024;
pub const MAX_COVER_PIXELS: u64 = 20_000_000;
pub const MAX_COVER_BATCH: usize = 50;
const MAX_STATE_BYTES: u64 = 1024 * 1024;
const MAX_REQUESTS: usize = 256;
const DIR: &str = "catalog/item-covers";

#[cfg(test)]
thread_local! { static INTERRUPT: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) }; }
fn publication_step(step: usize) -> Result<()> {
    #[cfg(test)]
    anyhow::ensure!(
        INTERRUPT.with(|s| s.get()) != Some(step),
        "simulated_cover_interruption"
    );
    #[cfg(not(test))]
    let _ = step;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Raster {
    pub sha256: String,
    pub media_type: String,
    pub width: u32,
    pub height: u32,
    pub byte_size: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum CoverSource {
    Podcast {
        episode_url: String,
        youtube_video_id: String,
        image_url: String,
        rss_sha256: String,
    },
    Weixin {
        article_url: String,
        image_url: String,
        html_sha256: String,
    },
}

impl CoverSource {
    fn source_url(&self) -> &str {
        match self { Self::Podcast { episode_url, .. } => episode_url, Self::Weixin { article_url, .. } => article_url }
    }
    fn evidence_sha256(&self) -> &str {
        match self { Self::Podcast { rss_sha256, .. } => rss_sha256, Self::Weixin { html_sha256, .. } => html_sha256 }
    }
    fn evidence_limit(&self) -> usize {
        match self { Self::Podcast { .. } => 2 * 1024 * 1024, Self::Weixin { .. } => 8 * 1024 * 1024 }
    }
    fn valid(&self) -> bool {
        match self {
            Self::Podcast { episode_url, youtube_video_id, image_url, .. } => episode_url.len() <= 2048 && youtube_cover_url(youtube_video_id, image_url),
            Self::Weixin { article_url, image_url, .. } => {
                let Ok(url) = Url::parse(article_url) else { return false; };
                let id = url.path().strip_prefix("/s/").unwrap_or("");
                article_url.len() <= 4096 && url.to_string() == *article_url
                    && url.scheme() == "https" && url.host_str() == Some("mp.weixin.qq.com")
                    && url.username().is_empty() && url.password().is_none() && url.port().is_none() && url.fragment().is_none()
                    && !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
                    && weixin_image_url(image_url)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cover {
    pub item_id: String,
    pub version: String,
    pub source_revision_id: String,
    pub source: CoverSource,
    pub raster: Raster,
    /// Precomputed PNG, longest edge <= 320; no per-read image resizing.
    pub thumbnail: Raster,
    pub sensitivity: Sensitivity,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoverEdit {
    pub item_id: String,
    pub expected_revision_id: String,
    pub expected_cover_version: Option<String>,
    pub request_id: String,
    pub source: CoverSource,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    digest: String,
    cover: Cover,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    schema_version: u32,
    item_id: String,
    current: Option<Cover>,
    requests: BTreeMap<String, Receipt>,
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Strict source URL shape; the caller must ALSO enforce DNS pinning and check
/// every redirect before fetching. A syntactically allowed URL is not SSRF proof.
pub fn youtube_cover_url(video: &str, value: &str) -> bool {
    if video.len() != 11
        || !video
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return false;
    }
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    url.scheme() == "https"
        && url.host_str() == Some("i.ytimg.com")
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && ["maxresdefault.jpg", "hqdefault.jpg", "mqdefault.jpg"]
            .iter()
            .any(|name| value == format!("https://i.ytimg.com/vi/{video}/{name}"))
}

/// Only WeChat's public article raster CDN, not arbitrary links found in HTML.
/// Callers must still pin public DNS answers and validate every redirect.
pub fn weixin_image_url(value: &str) -> bool {
    let Ok(url) = Url::parse(value) else { return false; };
    let parts: Vec<_> = url.path().split('/').collect();
    value.len() <= 4096 && url.to_string() == value && url.scheme() == "https"
        && url.host_str() == Some("mmbiz.qpic.cn") && url.port().is_none()
        && url.username().is_empty() && url.password().is_none() && url.fragment().is_none()
        && parts.len() == 4 && matches!(parts[1], "mmbiz_jpg" | "mmbiz_png" | "mmbiz_gif" | "mmbiz_webp" | "sz_mmbiz_jpg" | "sz_mmbiz_png" | "sz_mmbiz_gif" | "sz_mmbiz_webp" | "mmbiz")
        && !parts[2].is_empty() && parts[2].bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        && !parts[3].is_empty() && parts[3].bytes().all(|b| b.is_ascii_digit())
}

fn decode(bytes: &[u8]) -> Result<(Raster, image::DynamicImage)> {
    anyhow::ensure!(
        !bytes.is_empty() && bytes.len() <= MAX_COVER_BYTES,
        "invalid_cover_image"
    );
    let format = image::guess_format(bytes).map_err(|_| anyhow::anyhow!("invalid_cover_image"))?;
    let media = match format {
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::Png => "image/png",
        ImageFormat::WebP => "image/webp",
        _ => anyhow::bail!("invalid_cover_image"),
    };
    let (width, height) = ImageReader::with_format(Cursor::new(bytes), format)
        .into_dimensions()
        .map_err(|_| anyhow::anyhow!("invalid_cover_image"))?;
    anyhow::ensure!(
        width > 0 && height > 0 && u64::from(width) * u64::from(height) <= MAX_COVER_PIXELS,
        "invalid_cover_image"
    );
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(100 * 1024 * 1024);
    limits.max_image_width = Some(width);
    limits.max_image_height = Some(height);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .map_err(|_| anyhow::anyhow!("invalid_cover_image"))?;
    Ok((
        Raster {
            sha256: hash(bytes),
            media_type: media.into(),
            width,
            height,
            byte_size: bytes.len(),
        },
        decoded,
    ))
}

fn state_path(root: &Path, item: &str) -> PathBuf {
    root.join(DIR).join(format!("{item}.json"))
}
fn resource_path(root: &Path, digest: &str) -> Result<PathBuf> {
    anyhow::ensure!(valid_lower_hex_64(digest), "cover_unavailable");
    Ok(root.join(DIR).join("resources").join(digest))
}

fn read_bounded(root: &Path, path: &Path, max: u64) -> Result<Vec<u8>> {
    // Check each parent, not only the final file or canonical containment.
    let relative = path.strip_prefix(root)?;
    let mut cursor = root.to_path_buf();
    for component in relative.components() {
        anyhow::ensure!(
            matches!(component, std::path::Component::Normal(_)),
            "cover_unavailable"
        );
        cursor.push(component);
        anyhow::ensure!(
            !fs::symlink_metadata(&cursor)?.file_type().is_symlink(),
            "cover_unavailable"
        );
    }
    let file = open_regular_nofollow(&cursor)?;
    anyhow::ensure!(file.metadata()?.len() <= max, "cover_unavailable");
    let mut bytes = Vec::new();
    file.take(max + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() as u64 <= max, "cover_unavailable");
    Ok(bytes)
}

fn state(root: &Path, item: &str) -> Result<State> {
    let path = state_path(root, item);
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(State {
                schema_version: 1,
                item_id: item.into(),
                current: None,
                requests: BTreeMap::new(),
            })
        }
        Err(e) => return Err(e.into()),
        Ok(_) => {}
    }
    let state: State = serde_json::from_slice(&read_bounded(root, &path, MAX_STATE_BYTES)?)?;
    anyhow::ensure!(
        state.schema_version == 1 && state.item_id == item && state.requests.len() <= MAX_REQUESTS,
        "cover_unavailable"
    );
    anyhow::ensure!(
        state.requests.iter().all(|(key, r)| valid_request_id(key)
            && valid_lower_hex_64(&r.digest)
            && r.cover.item_id == item),
        "cover_unavailable"
    );
    if let Some(cover) = &state.current {
        anyhow::ensure!(
            cover.item_id == item
                && valid_lower_hex_64(&cover.version)
                && state.requests.values().any(|r| &r.cover == cover),
            "cover_unavailable"
        );
    }
    Ok(state)
}

fn authorized(root: &Path, cover: &Cover, ceiling: Sensitivity) -> Result<()> {
    anyhow::ensure!(cover.sensitivity <= ceiling, "library_resource_not_found");
    let revision = reader::revision_text::historical_revision(
        root,
        &cover.item_id,
        &cover.source_revision_id,
        ceiling,
    )?;
    anyhow::ensure!(
        revision.source_url == cover.source.source_url()
            || revision.canonical_url.as_deref() == Some(cover.source.source_url()),
        "cover_unavailable"
    );
    let rss = read_bounded(
        root,
        &resource_path(root, cover.source.evidence_sha256())?,
        cover.source.evidence_limit() as u64,
    )?;
    anyhow::ensure!(cover.source.valid() && hash(&rss) == cover.source.evidence_sha256(), "cover_unavailable");
    Ok(())
}

/// Trusted owner-local worker API, NOT an untrusted remote edit endpoint. The
/// worker supplies the exact verified RSS response and fetched raster. Publication
/// is one atomic state+receipt rename after immutable resources are durable.
pub fn put_cover(
    library: &Path,
    edit: CoverEdit,
    image: &[u8],
    rss: &[u8],
    ceiling: Sensitivity,
) -> Result<Cover> {
    anyhow::ensure!(
        valid_item_id(&edit.item_id)
            && valid_revision_id(&edit.expected_revision_id)
            && valid_request_id(&edit.request_id)
            && edit
                .expected_cover_version
                .as_deref()
                .is_none_or(valid_lower_hex_64)
            && edit.source.valid()
            && !rss.is_empty()
            && rss.len() <= edit.source.evidence_limit()
            && hash(rss) == edit.source.evidence_sha256(),
        "invalid_cover_request"
    );
    let (raster, decoded) = decode(image)?;
    // YouTube's missing-image placeholder is 120x90. Do not report it as success.
    anyhow::ensure!(
        match &edit.source {
            CoverSource::Podcast { .. } => raster.width >= 320 && raster.height >= 180,
            CoverSource::Weixin { .. } => raster.width >= 64 && raster.height >= 64,
        },
        "cover_placeholder"
    );
    let digest = hash(&serde_json::to_vec(&("item_cover_v1", &edit, &raster))?);
    let root = existing_canonical_root(library)?;
    let lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    let (item, revision) = read_item_at_sensitivity(&root, &edit.item_id, ceiling)?
        .context("library_resource_not_found")?;
    let mut state = state(&root, &edit.item_id)?;
    // Authorize replay before returning even a historical receipt; no reapply.
    if let Some(receipt) = state.requests.get(&edit.request_id) {
        anyhow::ensure!(receipt.digest == digest, "request_id_conflict");
        authorized(&root, &receipt.cover, ceiling)?;
        let result = receipt.cover.clone();
        lock.unlock()?;
        return Ok(result);
    }
    anyhow::ensure!(
        revision.revision_id == edit.expected_revision_id,
        "stale_item_revision"
    );
    anyhow::ensure!(
        revision.source_url == edit.source.source_url()
            || revision.canonical_url.as_deref() == Some(edit.source.source_url()),
        "cover_source_mismatch"
    );
    anyhow::ensure!(
        state.current.as_ref().map(|c| &c.version) == edit.expected_cover_version.as_ref(),
        "stale_cover_version"
    );
    anyhow::ensure!(state.requests.len() < MAX_REQUESTS, "cover_request_limit");
    let mut thumbnail_bytes = Cursor::new(Vec::new());
    decoded
        .thumbnail(320, 320)
        .write_to(&mut thumbnail_bytes, ImageFormat::Png)?;
    let thumbnail_bytes = thumbnail_bytes.into_inner();
    let (thumbnail, _) = decode(&thumbnail_bytes)?;
    let cover = Cover {
        item_id: edit.item_id.clone(),
        version: hash(&serde_json::to_vec(&(
            &digest,
            state.requests.len(),
            &state.current,
        ))?),
        source_revision_id: revision.revision_id,
        source: edit.source,
        raster,
        thumbnail,
        sensitivity: item.sensitivity.max(revision.sensitivity),
    };
    ensure_contained_dir(&root, Path::new(DIR).join("resources").as_path())?;
    for (digest, bytes) in [
        (cover.raster.sha256.as_str(), image),
        (cover.thumbnail.sha256.as_str(), thumbnail_bytes.as_slice()),
        (cover.source.evidence_sha256(), rss),
    ] {
        install_binary(&root, &resource_path(&root, digest)?, bytes)?;
    }
    publication_step(1)?;
    state.current = Some(cover.clone());
    state.requests.insert(
        edit.request_id,
        Receipt {
            digest,
            cover: cover.clone(),
        },
    );
    let bytes = serde_json::to_vec(&state)?;
    anyhow::ensure!(bytes.len() as u64 <= MAX_STATE_BYTES, "cover_request_limit");
    durable_replace(&root, &state_path(&root, &edit.item_id), &bytes)?;
    publication_step(2)?;
    lock.unlock()?;
    Ok(cover)
}

/// Bounded batch metadata only; hidden/missing items return None indistinguishably.
pub fn cover_batch(
    library: &Path,
    items: &[String],
    ceiling: Sensitivity,
) -> Result<Vec<Option<Cover>>> {
    anyhow::ensure!(
        items.len() <= MAX_COVER_BATCH && items.iter().all(|id| valid_item_id(id)),
        "invalid_cover_request"
    );
    with_consistent_snapshot(library, |root| {
        items
            .iter()
            .map(|item| {
                if read_item_at_sensitivity(root, item, ceiling)?.is_none() {
                    return Ok(None);
                }
                let Some(cover) = state(root, item)?.current else {
                    return Ok(None);
                };
                if cover.sensitivity > ceiling {
                    return Ok(None);
                }
                authorized(root, &cover, ceiling)?;
                Ok(Some(cover))
            })
            .collect()
    })
}

/// Version-bound, permission-checked chunks; original and precomputed thumbnail
/// share the same gate. A caller cannot use an old digest as a resource capability.
pub fn cover_chunk(
    library: &Path,
    item: &str,
    version: &str,
    thumbnail: bool,
    offset: usize,
    ceiling: Sensitivity,
) -> Result<ImageChunk> {
    anyhow::ensure!(
        valid_item_id(item) && valid_lower_hex_64(version),
        "invalid_cover_request"
    );
    with_consistent_snapshot(library, |root| {
        read_item_at_sensitivity(root, item, ceiling)?.context("library_resource_not_found")?;
        let cover = state(root, item)?
            .current
            .context("library_resource_not_found")?;
        authorized(root, &cover, ceiling)?;
        anyhow::ensure!(cover.version == version, "stale_cover_version");
        let raster = if thumbnail {
            &cover.thumbnail
        } else {
            &cover.raster
        };
        let bytes = read_bounded(
            root,
            &resource_path(root, &raster.sha256)?,
            MAX_COVER_BYTES as u64,
        )?;
        anyhow::ensure!(
            bytes.len() == raster.byte_size && hash(&bytes) == raster.sha256,
            "cover_unavailable"
        );
        anyhow::ensure!(offset <= bytes.len(), "invalid_cover_offset");
        let end = offset.saturating_add(IMAGE_CHUNK_BYTES).min(bytes.len());
        Ok(ImageChunk {
            media_type: raster.media_type.clone(),
            sha256: raster.sha256.clone(),
            total_bytes: bytes.len(),
            offset,
            next_offset: (end < bytes.len()).then_some(end),
            bytes: bytes[offset..end].to_vec(),
        })
    })
}

#[cfg(test)]
mod tests;
