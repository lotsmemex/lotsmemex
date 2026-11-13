//! Synthetic `item_cover_v1` covers: PNGs drawn by this file (soft warm
//! gradients, never downloaded), served as the frozen contract describes:
//! ordered nullable batches, current-version-only chunks of at most 96 KiB,
//! `stale_cover_version` for any other version. Development and tests only.

use crate::covers::{MAX_COVER_CHUNK, THUMBNAIL_EDGE};
use crate::{Command, Response, Sensitivity};
use base64::Engine;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

/// Items with a synthetic cover (the long read, the podcast, the showcase).
pub const COVER_ITEMS: [&str; 3] = [crate::fixture::LONGREAD_ITEM, crate::fixture::PODCAST_ITEM, crate::fixture::SHOWCASE_ITEM];
/// Original size: 16:9, several chunks long.
pub const COVER_W: u32 = 960;
pub const COVER_H: u32 = 540;

/// How the synthetic agent misbehaves (from `FixtureControls`).
#[derive(Debug, Clone, Copy, Default)]
pub struct CoverFaults {
    /// `item_cover_batch` answers null for every item.
    pub all_null: bool,
    /// After the first chunk of the next read, the cover is republished (new
    /// version, new bytes) and that read's next chunk answers `stale_cover_version`.
    pub stale_once: bool,
    /// Chunks carry one flipped byte; their headers still claim the stored hash.
    pub corrupt: bool,
}

struct Rendered {
    version: String,
    original: Vec<u8>,
    thumbnail: Vec<u8>,
}

#[derive(Default)]
pub struct CoverState {
    /// Republications per item.
    generation: HashMap<String, u32>,
    rendered: HashMap<(String, u32), std::sync::Arc<Rendered>>,
}

impl std::fmt::Debug for CoverState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoverState").field("generation", &self.generation).finish()
    }
}

fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn err(code: &str) -> Response {
    Response { ok: false, reason_code: Some(code.into()), ..Default::default() }
}

fn ok(value: serde_json::Value) -> Response {
    Response { ok: true, item_cover: Some(value), ..Default::default() }
}

impl CoverState {
    fn current(&mut self, item: &str) -> Option<std::sync::Arc<Rendered>> {
        let seed = COVER_ITEMS.iter().position(|i| *i == item)?;
        let generation = *self.generation.get(item).unwrap_or(&0);
        let entry = self.rendered.entry((item.into(), generation)).or_insert_with(|| {
            let original = png(COVER_W, COVER_H, seed, generation);
            let thumbnail = png(THUMBNAIL_EDGE, THUMBNAIL_EDGE * COVER_H / COVER_W, seed, generation);
            let version = sha(format!("cover|{item}|{generation}|{}", sha(&original)).as_bytes());
            std::sync::Arc::new(Rendered { version, original, thumbnail })
        });
        Some(entry.clone())
    }

    fn republish(&mut self, item: &str) {
        *self.generation.entry(item.into()).or_default() += 1;
    }

    fn descriptor(item: &str, revision: &str, r: &Rendered) -> serde_json::Value {
        let raster = |bytes: &[u8], w: u32, h: u32| serde_json::json!({"sha256": sha(bytes), "media_type": "image/png", "width": w, "height": h, "byte_size": bytes.len()});
        let video = format!("synthetic{:02}", COVER_ITEMS.iter().position(|i| *i == item).unwrap_or(0));
        serde_json::json!({
            "item_id": item, "version": r.version, "source_revision_id": revision,
            "source": {"episode_url": format!("https://example.invalid/episodes/{item}"), "youtube_video_id": video,
                "image_url": format!("https://i.ytimg.com/vi/{video}/hqdefault.jpg"), "rss_sha256": sha(b"synthetic rss")},
            "raster": raster(&r.original, COVER_W, COVER_H),
            "thumbnail": raster(&r.thumbnail, THUMBNAIL_EDGE, THUMBNAIL_EDGE * COVER_H / COVER_W),
            "sensitivity": "private",
        })
    }

    /// Answers a cover command; `revision` is the visible item's current revision
    /// (`None`: missing, trashed or hidden).
    pub fn handle(&mut self, command: &Command, revision: &dyn Fn(&str) -> Option<String>, faults: &mut CoverFaults) -> Option<Response> {
        match command {
            Command::ItemCoverBatch { item_ids, max_sensitivity } => {
                if item_ids.len() > 50 { return Some(err("invalid_cover_request")) }
                let covers: Vec<serde_json::Value> = item_ids.iter().map(|id| {
                    let visible = *max_sensitivity >= Sensitivity::Private && !faults.all_null;
                    match (visible, revision(id)) {
                        (true, Some(rev)) => self.current(id).map_or(serde_json::Value::Null, |r| Self::descriptor(id, &rev, &r)),
                        _ => serde_json::Value::Null,
                    }
                }).collect();
                Some(ok(serde_json::Value::Array(covers)))
            }
            Command::ItemCoverChunk { item_id, version, thumbnail, offset, max_sensitivity } => {
                if *max_sensitivity < Sensitivity::Private || revision(item_id).is_none() { return Some(err("library_resource_not_found")) }
                let Some(r) = self.current(item_id) else { return Some(err("library_resource_not_found")) };
                if &r.version != version { return Some(err("stale_cover_version")) }
                if faults.stale_once && *offset > 0 {
                    faults.stale_once = false;
                    self.republish(item_id);
                    return Some(err("stale_cover_version"));
                }
                let bytes = if *thumbnail { &r.thumbnail } else { &r.original };
                if *offset >= bytes.len() { return Some(err("invalid_cover_offset")) }
                let end = (*offset + MAX_COVER_CHUNK).min(bytes.len());
                let mut part = bytes[*offset..end].to_vec();
                if faults.corrupt { part[0] ^= 0x5a; }
                Some(ok(serde_json::json!({
                    "media_type": "image/png", "sha256": sha(bytes), "total_bytes": bytes.len(), "offset": offset,
                    "next_offset": (end < bytes.len()).then_some(end),
                    "data_base64": base64::engine::general_purpose::STANDARD.encode(&part),
                })))
            }
            _ => None,
        }
    }
}

/// Soft palettes: (top-left, bottom-right, glow) in 0..1 RGB.
const PALETTES: [[[f32; 3]; 3]; 3] = [
    [[0.97, 0.86, 0.80], [0.66, 0.44, 0.46], [1.00, 0.93, 0.84]], // peach to dusty rose
    [[0.95, 0.89, 0.78], [0.55, 0.45, 0.42], [1.00, 0.95, 0.86]], // champagne to warm taupe
    [[0.92, 0.84, 0.86], [0.48, 0.38, 0.50], [0.99, 0.90, 0.88]], // blush to mauve dusk
];

/// Pixels of synthetic cover `seed` after `generation` republications.
fn pixel(x: f32, y: f32, w: f32, h: f32, seed: usize, generation: u32) -> [u8; 3] {
    let [top, bottom, glow] = PALETTES[(seed + generation as usize) % PALETTES.len()];
    let (u, v) = (x / w, y / h);
    let t = (v * 0.75 + u * 0.25).clamp(0., 1.);
    let t = t * t * (3. - 2. * t);
    let mut c = [0.; 3];
    for k in 0..3 { c[k] = top[k] + (bottom[k] - top[k]) * t; }
    // A low warm sun and a few soft out-of-focus lights; sizes relative to the height.
    let lights: [(f32, f32, f32, f32); 6] = [(0.68, 0.46, 0.17, 0.85), (0.22, 0.28, 0.07, 0.45), (0.38, 0.74, 0.05, 0.40), (0.86, 0.78, 0.045, 0.45), (0.12, 0.80, 0.08, 0.35), (0.52, 0.20, 0.035, 0.40)];
    for (i, (lx, ly, r, a)) in lights.iter().enumerate() {
        let shift = (seed as f32 * 0.13 + i as f32 * 0.07 + generation as f32 * 0.23) % 0.2 - 0.1;
        let dx = (u - (lx + shift)) * w / h;
        let dy = v - ly;
        let d = (dx * dx + dy * dy).sqrt() / r;
        // A disc with a soft edge for the small lights, a wide halo for the sun.
        let t = if i == 0 { (-d * d * 0.9).exp() } else { (1. - ((d - 0.85) * 5.).clamp(0., 1.)) * 0.8 + (-d * d * 0.5).exp() * 0.2 } * a;
        for k in 0..3 { c[k] += (glow[k] - c[k]) * t; }
    }
    // A thin horizon of light and a gentle vignette.
    let band = (-(v - 0.64).powi(2) * 400.).exp() * 0.18;
    for k in 0..3 { c[k] += (1. - c[k]) * band; }
    let vignette = 1. - 0.18 * (((u - 0.5) * 1.6).powi(2) + ((v - 0.5) * 1.6).powi(2)).min(1.);
    c.map(|v| ((v * vignette).clamp(0., 1.) * 255.).round() as u8)
}

/// An RGB PNG (stored deflate blocks), drawn at `w` × `h`.
pub fn png(w: u32, h: u32, seed: usize, generation: u32) -> Vec<u8> {
    let mut raw = Vec::with_capacity((h * (w * 3 + 1)) as usize);
    for y in 0..h {
        raw.push(0);
        for x in 0..w { raw.extend(pixel(x as f32, y as f32, w as f32, h as f32, seed, generation)); }
    }
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65535).collect();
    for (i, b) in blocks.iter().enumerate() {
        z.push(u8::from(i + 1 == blocks.len()));
        let n = b.len() as u16;
        z.extend(n.to_le_bytes());
        z.extend((!n).to_le_bytes());
        z.extend_from_slice(b);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw { a = (a + x as u32) % 65521; b = (b + a) % 65521; }
    z.extend(((b << 16) | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend(w.to_be_bytes());
    ihdr.extend(h.to_be_bytes());
    ihdr.extend([8, 2, 0, 0, 0]);
    for (kind, data) in [(b"IHDR", ihdr), (b"IDAT", z), (b"IEND", vec![])] {
        out.extend((data.len() as u32).to_be_bytes());
        let mut crc_input = kind.to_vec();
        crc_input.extend(&data);
        out.extend(&crc_input);
        out.extend(crc32(&crc_input).to_be_bytes());
    }
    out
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 { crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 }; }
    }
    !crc
}
