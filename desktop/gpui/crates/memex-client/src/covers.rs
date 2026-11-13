//! `item_cover_v1`: an item's cover, stored apart from its text. The reader shows
//! the original above the canonical Markdown (never part of its byte coordinates);
//! list and future tile views read only the precomputed thumbnails.
//!
//! Nothing is sent unless the agent advertises `item_cover_v1`. Bytes reach a
//! renderer only after the whole resource of one cover version was assembled and
//! matches its descriptor's SHA-256 and size; a version change during a read
//! (`stale_cover_version`) asks the batch again and restarts at offset 0, never
//! joining chunks of two versions.
use crate::{Client, ClientError, Command, Sensitivity};
use base64::Engine;
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const COVER_FEATURE: &str = "item_cover_v1";
/// Largest `item_cover_batch` the agent accepts.
pub const MAX_COVER_BATCH: usize = 50;
/// Largest chunk the agent sends (decoded bytes).
pub const MAX_COVER_CHUNK: usize = 96 * 1024;
/// Largest raster the Store keeps.
pub const MAX_COVER_BYTES: usize = 5 * 1024 * 1024;
/// Longest edge of a precomputed thumbnail.
pub const THUMBNAIL_EDGE: u32 = 320;
/// Version changes tolerated while one cover is read.
const STALE_RETRIES: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Raster {
    pub sha256: String,
    pub media_type: String,
    pub width: u32,
    pub height: u32,
    pub byte_size: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CoverSource {
    #[serde(default)]
    pub episode_url: Option<String>,
    #[serde(default)]
    pub youtube_video_id: Option<String>,
    #[serde(default)]
    pub image_url: Option<String>,
    #[serde(default)]
    pub rss_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Cover {
    pub item_id: String,
    pub version: String,
    pub source_revision_id: String,
    pub source: CoverSource,
    pub raster: Raster,
    pub thumbnail: Raster,
    pub sensitivity: Sensitivity,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CoverChunk {
    pub media_type: String,
    pub sha256: String,
    pub total_bytes: usize,
    pub offset: usize,
    pub next_offset: Option<usize>,
    pub data_base64: String,
}

/// Verified bytes of one cover version (original or thumbnail).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverImage {
    pub cover: Cover,
    pub thumbnail: bool,
    pub bytes: Vec<u8>,
}

impl CoverImage {
    pub fn raster(&self) -> &Raster {
        if self.thumbnail { &self.cover.thumbnail } else { &self.cover.raster }
    }
}

fn hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn raster_ok(r: &Raster, thumbnail: bool) -> bool {
    let media = matches!(r.media_type.as_str(), "image/png" | "image/jpeg" | "image/webp");
    hex64(&r.sha256) && media && r.width > 0 && r.height > 0 && r.byte_size > 0 && r.byte_size <= MAX_COVER_BYTES
        && (!thumbnail || (r.media_type == "image/png" && r.width.max(r.height) <= THUMBNAIL_EDGE))
}

fn is_stale(e: &ClientError) -> bool {
    matches!(e, ClientError::Daemon(code) if code == "stale_cover_version")
}

impl Client {
    /// The agent advertises `item_cover_v1`; otherwise no cover request is sent.
    pub fn covers_supported(&self) -> Result<bool, ClientError> {
        Ok(self.capabilities()?.item_cover)
    }

    /// One `item_cover_batch`: a nullable cover per requested position (null:
    /// no visible cover). At most `MAX_COVER_BATCH` ids.
    pub fn cover_batch(&self, item_ids: &[String]) -> Result<Vec<Option<Cover>>, ClientError> {
        if !self.covers_supported()? { return Err(ClientError::Unsupported) }
        if item_ids.len() > MAX_COVER_BATCH { return Err(ClientError::Daemon("invalid_cover_request".into())) }
        if item_ids.is_empty() { return Ok(vec![]) }
        let value = self.backend.send(&Command::ItemCoverBatch { item_ids: item_ids.to_vec(), max_sensitivity: Sensitivity::Restricted })?
            .item_cover.ok_or(ClientError::InvalidFrame)?;
        let covers: Vec<Option<Cover>> = serde_json::from_value(value).map_err(|_| ClientError::InvalidFrame)?;
        let valid = covers.len() == item_ids.len() && covers.iter().zip(item_ids).all(|(c, id)| c.as_ref().is_none_or(|c| {
            &c.item_id == id && hex64(&c.version) && raster_ok(&c.raster, false) && raster_ok(&c.thumbnail, true)
        }));
        if !valid { return Err(ClientError::InvalidFrame) }
        Ok(covers)
    }

    /// The whole original (`thumbnail=false`) or thumbnail of exactly this cover
    /// version, verified against its descriptor. A version change fails with
    /// `stale_cover_version`; the caller asks the batch again.
    pub fn cover_bytes(&self, cover: &Cover, thumbnail: bool) -> Result<Vec<u8>, ClientError> {
        if !self.covers_supported()? { return Err(ClientError::Unsupported) }
        let want = if thumbnail { &cover.thumbnail } else { &cover.raster };
        let mut bytes = Vec::with_capacity(want.byte_size);
        loop {
            let value = self.backend.send(&Command::ItemCoverChunk {
                item_id: cover.item_id.clone(), version: cover.version.clone(), thumbnail, offset: bytes.len(), max_sensitivity: Sensitivity::Restricted,
            })?.item_cover.ok_or(ClientError::InvalidFrame)?;
            let r: CoverChunk = serde_json::from_value(value).map_err(|_| ClientError::InvalidFrame)?;
            if r.media_type != want.media_type || r.sha256 != want.sha256 || r.total_bytes != want.byte_size
                || r.offset != bytes.len() || r.data_base64.len() > MAX_COVER_CHUNK.div_ceil(3) * 4 {
                return Err(ClientError::InvalidFrame);
            }
            let chunk = base64::engine::general_purpose::STANDARD.decode(r.data_base64).map_err(|_| ClientError::InvalidFrame)?;
            if chunk.is_empty() || chunk.len() > MAX_COVER_CHUNK || bytes.len() + chunk.len() > want.byte_size { return Err(ClientError::InvalidFrame) }
            bytes.extend(chunk);
            let next = (bytes.len() < want.byte_size).then_some(bytes.len());
            if next != r.next_offset { return Err(ClientError::InvalidFrame) }
            if next.is_none() { break }
        }
        if bytes.len() != want.byte_size || format!("{:x}", Sha256::digest(&bytes)) != want.sha256 { return Err(ClientError::InvalidFrame) }
        Ok(bytes)
    }

    /// Reads `cover`'s resource; on `stale_cover_version` asks the batch again
    /// and restarts from offset 0 with the new version (`None`: no cover now).
    fn cover_image(&self, mut cover: Cover, thumbnail: bool) -> Result<Option<CoverImage>, ClientError> {
        for _ in 0..STALE_RETRIES {
            match self.cover_bytes(&cover, thumbnail) {
                Ok(bytes) => return Ok(Some(CoverImage { cover, thumbnail, bytes })),
                Err(e) if is_stale(&e) => match self.cover_batch(std::slice::from_ref(&cover.item_id))?.pop().flatten() {
                    Some(next) => cover = next,
                    None => return Ok(None),
                },
                Err(e) => return Err(e),
            }
        }
        Err(ClientError::Daemon("stale_cover_version".into()))
    }

    /// The reader's cover: the verified original, or `None` when the item has
    /// no visible cover. `Err(Unsupported)` without `item_cover_v1` (nothing sent).
    pub fn cover_original(&self, item_id: &str) -> Result<Option<CoverImage>, ClientError> {
        match self.cover_batch(&[item_id.to_string()])?.pop().flatten() {
            Some(cover) => self.cover_image(cover, false),
            None => Ok(None),
        }
    }

    /// Thumbnails for a list or tile view, in the order asked (batches of 50).
    /// A batch failure fails the call; one cover's failure is its own `Err`,
    /// and `Ok(None)` means no visible cover (no placeholder).
    pub fn cover_thumbnails(&self, item_ids: &[String]) -> Result<Vec<Result<Option<CoverImage>, ClientError>>, ClientError> {
        let mut out = Vec::with_capacity(item_ids.len());
        for ids in item_ids.chunks(MAX_COVER_BATCH) {
            for cover in self.cover_batch(ids)? {
                out.push(match cover { Some(c) => self.cover_image(c, true), None => Ok(None) });
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{FixtureBackend, FixtureControls, LONGREAD_ITEM, PODCAST_ITEM};
    use crate::{Backend, BackendMode, Response};
    use std::sync::{Arc, Mutex};

    /// The fixture, recording every command sent to it.
    #[derive(Clone)]
    struct Recording(FixtureBackend, Arc<Mutex<Vec<Command>>>);
    impl Backend for Recording {
        fn send(&self, command: &Command) -> Result<Response, ClientError> {
            self.1.lock().unwrap().push(command.clone());
            self.0.send(command)
        }
        fn mode(&self) -> BackendMode { BackendMode::SyntheticFixture }
    }

    fn client(controls: FixtureControls) -> (Client, Recording) {
        let fixture = FixtureBackend::new(40);
        fixture.set_controls(controls);
        let rec = Recording(fixture, Default::default());
        (Client::new(rec.clone()), rec)
    }

    fn cover_commands(rec: &Recording) -> Vec<Command> {
        rec.1.lock().unwrap().iter().filter(|c| matches!(c, Command::ItemCoverBatch { .. } | Command::ItemCoverChunk { .. })).cloned().collect()
    }

    fn chunk_versions(rec: &Recording) -> Vec<(String, usize)> {
        cover_commands(rec).into_iter().filter_map(|c| match c { Command::ItemCoverChunk { version, offset, .. } => Some((version, offset)), _ => None }).collect()
    }

    #[test]
    fn without_the_capability_no_cover_request_is_sent() {
        let (client, rec) = client(FixtureControls { no_item_cover: true, ..Default::default() });
        assert_eq!(client.cover_original(LONGREAD_ITEM), Err(ClientError::Unsupported));
        assert_eq!(client.cover_thumbnails(&[LONGREAD_ITEM.into()]).unwrap_err(), ClientError::Unsupported);
        assert!(cover_commands(&rec).is_empty());
    }

    #[test]
    fn the_original_is_assembled_from_several_chunks_and_matches_its_descriptor() {
        let (client, rec) = client(FixtureControls::default());
        let image = client.cover_original(LONGREAD_ITEM).unwrap().expect("synthetic cover");
        assert!(!image.thumbnail);
        assert_eq!(image.bytes.len(), image.cover.raster.byte_size);
        assert_eq!(format!("{:x}", Sha256::digest(&image.bytes)), image.cover.raster.sha256);
        assert!(image.bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        let chunks = chunk_versions(&rec);
        assert!(chunks.len() > 2, "{chunks:?}");
        assert!(chunks.iter().all(|(v, _)| *v == image.cover.version));
        assert!(rec.1.lock().unwrap().iter().all(|c| !matches!(c, Command::ItemCoverChunk { thumbnail: true, .. })));
    }

    #[test]
    fn null_means_no_cover_and_no_chunk_is_read() {
        let (client, rec) = client(FixtureControls::default());
        assert_eq!(client.cover_original("item_fixture_000001"), Ok(None));
        let (client_null, rec_null) = client_with_null();
        assert_eq!(client_null.cover_original(LONGREAD_ITEM), Ok(None));
        assert!(chunk_versions(&rec).is_empty() && chunk_versions(&rec_null).is_empty());
    }

    fn client_with_null() -> (Client, Recording) { client(FixtureControls { cover_null: true, ..Default::default() }) }

    #[test]
    fn a_version_change_mid_read_rereads_the_batch_and_restarts_at_zero() {
        let (plain, _) = client(FixtureControls::default());
        let before = plain.cover_original(LONGREAD_ITEM).unwrap().unwrap();
        let (client, rec) = client(FixtureControls { cover_stale_once: true, ..Default::default() });
        let image = client.cover_original(LONGREAD_ITEM).unwrap().unwrap();
        assert_ne!(image.cover.version, before.cover.version);
        assert_ne!(image.bytes, before.bytes, "the republished cover has new bytes");
        assert_eq!(format!("{:x}", Sha256::digest(&image.bytes)), image.cover.raster.sha256);
        let cmds = cover_commands(&rec);
        let batches = cmds.iter().filter(|c| matches!(c, Command::ItemCoverBatch { .. })).count();
        assert_eq!(batches, 2);
        // Old version: offsets 0 and the stale one; new version starts at 0 again.
        let chunks = chunk_versions(&rec);
        let first_new = chunks.iter().position(|(v, _)| *v == image.cover.version).unwrap();
        assert_eq!(chunks[first_new].1, 0);
        assert!(chunks[..first_new].iter().all(|(v, _)| *v == before.cover.version));
        assert!(chunks[first_new..].iter().all(|(v, _)| *v == image.cover.version));
    }

    #[test]
    fn a_hash_mismatch_is_an_error_never_bytes() {
        let (client, _) = client(FixtureControls { cover_corrupt: true, ..Default::default() });
        assert_eq!(client.cover_original(PODCAST_ITEM), Err(ClientError::InvalidFrame));
        let thumbs = client.cover_thumbnails(&[PODCAST_ITEM.into()]).unwrap();
        assert_eq!(thumbs, vec![Err(ClientError::InvalidFrame)]);
    }

    #[test]
    fn thumbnails_come_in_order_in_batches_of_fifty() {
        let (client, rec) = client(FixtureControls::default());
        let mut ids: Vec<String> = (0..60).map(|i| format!("item_fixture_{i:06}")).collect();
        ids[55] = LONGREAD_ITEM.into();
        let thumbs = client.cover_thumbnails(&ids).unwrap();
        assert_eq!(thumbs.len(), 60);
        let batches: Vec<usize> = cover_commands(&rec).iter().filter_map(|c| match c { Command::ItemCoverBatch { item_ids, .. } => Some(item_ids.len()), _ => None }).collect();
        assert_eq!(batches, vec![50, 10]);
        for (id, t) in ids.iter().zip(&thumbs) {
            let t = t.as_ref().unwrap();
            if [LONGREAD_ITEM, PODCAST_ITEM, crate::fixture::SHOWCASE_ITEM].contains(&id.as_str()) {
                let t = t.as_ref().expect("cover");
                assert!(t.thumbnail && &t.cover.item_id == id && t.raster().media_type == "image/png" && t.raster().width.max(t.raster().height) <= THUMBNAIL_EDGE);
                assert_eq!(t.bytes.len(), t.cover.thumbnail.byte_size);
            } else {
                assert!(t.is_none(), "{id}");
            }
        }
        assert!(rec.1.lock().unwrap().iter().all(|c| !matches!(c, Command::ItemCoverChunk { thumbnail: false, .. })));
    }
}
