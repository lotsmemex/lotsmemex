//! Deterministic extraction from the worker-fetched Amp RSS, never client hints.
//! This module does no networking, item writes or generation.
use anyhow::{ensure, Context, Result};
use memex_core::{DatePrecision, PartialDate};
use roxmltree::{Document, Node, ParsingOptions};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub const FEED_URL: &str = "https://ampcode.com/podcast.rss";
pub const MAX_FEED_BYTES: usize = 2 * 1024 * 1024;
const ITUNES: &str = "http://www.itunes.com/dtds/podcast-1.0.dtd";

/// Only publisher episode routes enter the RSS worker; `none` is not a classifier.
pub fn is_episode_url(value: &str) -> bool {
    let Some(path) = value.strip_prefix("https://ampcode.com/podcast/") else { return false; };
    let number = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && s.parse::<u32>().is_ok_and(|n| n > 0);
    if let Some(episode) = path.strip_prefix("episode-") { return number(episode); }
    let Some((season, episode)) = path.strip_prefix("season-").and_then(|s| s.split_once("/episode-")) else { return false; };
    number(season) && number(episode)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EpisodeMetadata {
    pub episode_identity: String,
    pub url: String,
    pub guid: String,
    pub season: u32,
    pub episode: u32,
    pub rss_sha256: String,
    pub source_start_byte: usize,
    pub source_end_byte: usize,
    pub published_date: Option<PartialDate>,
    pub published_instant: Option<String>,
    pub duration_seconds: Option<u32>,
    pub duration_raw: Option<String>,
}

fn text<'a>(item: Node<'a, 'a>, namespace: Option<&str>, name: &str) -> Result<Option<&'a str>> {
    let mut fields = item.children().filter(|n| {
        n.is_element() && n.tag_name().name() == name && n.tag_name().namespace() == namespace
    });
    let first = fields.next();
    ensure!(fields.next().is_none(), "podcast_ambiguous_field");
    first
        .map(|node| {
            ensure!(
                !node.children().any(|n| n.is_element())
                    && node.children().filter(|n| n.is_text()).count() == 1,
                "podcast_invalid_field"
            );
            node.text().context("podcast_invalid_field")
        })
        .transpose()
}

fn positive(raw: &str) -> Result<u32> {
    ensure!(
        !raw.is_empty() && raw.len() <= 10 && raw.bytes().all(|b| b.is_ascii_digit()),
        "podcast_invalid_number"
    );
    let n = raw
        .parse::<u32>()
        .map_err(|_| anyhow::anyhow!("podcast_invalid_number"))?;
    ensure!(n > 0, "podcast_invalid_number");
    Ok(n)
}

/// Positive integer seconds, mm:ss or hh:mm:ss. No decimals, signs, units,
/// truncation or overflow. Whitespace is only removed at the outside.
pub fn duration_seconds(raw: &str) -> Result<u32> {
    let raw = raw.trim();
    ensure!(raw.len() <= 32, "podcast_invalid_duration");
    let parts: Vec<_> = raw.split(':').collect();
    if parts.len() == 1 {
        return positive(raw).map_err(|_| anyhow::anyhow!("podcast_invalid_duration"));
    }
    ensure!((2..=3).contains(&parts.len()), "podcast_invalid_duration");
    let mut seconds = 0_u32;
    for (index, part) in parts.iter().enumerate() {
        ensure!(
            !part.is_empty()
                && part.bytes().all(|b| b.is_ascii_digit())
                && (index == 0 || part.len() == 2),
            "podcast_invalid_duration"
        );
        let n = part
            .parse::<u32>()
            .map_err(|_| anyhow::anyhow!("podcast_invalid_duration"))?;
        ensure!(index == 0 || n < 60, "podcast_invalid_duration");
        seconds = seconds
            .checked_mul(60)
            .and_then(|s| s.checked_add(n))
            .context("podcast_invalid_duration")?;
    }
    ensure!(seconds > 0, "podcast_invalid_duration");
    Ok(seconds)
}

/// A worker calls this only for bytes fetched from FEED_URL under the network
/// policy. The original bytes must be persisted with the returned hash/range.
/// GUID is scoped to this exact feed; title and mutable body hashes are not IDs.
pub fn episode_from_feed(bytes: &[u8], requested_url: &str) -> Result<EpisodeMetadata> {
    ensure!(
        !bytes.is_empty() && bytes.len() <= MAX_FEED_BYTES,
        "podcast_feed_limit"
    );
    let requested =
        url::Url::parse(requested_url).map_err(|_| anyhow::anyhow!("podcast_invalid_url"))?;
    ensure!(
        requested.scheme() == "https"
            && requested.host_str() == Some("ampcode.com")
            && requested.username().is_empty()
            && requested.password().is_none()
            && requested.port().is_none()
            && requested.fragment().is_none()
            && requested.as_str() == requested_url,
        "podcast_invalid_url"
    );
    let xml = std::str::from_utf8(bytes).map_err(|_| anyhow::anyhow!("podcast_invalid_feed"))?;
    let doc = Document::parse_with_options(
        xml,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: 20_000,
            ..Default::default()
        },
    )
    .map_err(|_| anyhow::anyhow!("podcast_invalid_feed"))?;
    let root = doc.root_element();
    ensure!(root.has_tag_name("rss"), "podcast_invalid_feed");
    let mut channels = root.children().filter(|n| n.has_tag_name("channel"));
    let channel = channels.next().context("podcast_invalid_feed")?;
    ensure!(channels.next().is_none(), "podcast_invalid_feed");
    let items: Vec<_> = channel
        .children()
        .filter(|n| n.has_tag_name("item"))
        .collect();
    ensure!(items.len() <= 1024, "podcast_feed_limit");
    let mut urls = BTreeSet::new();
    let mut guids = BTreeSet::new();
    let mut selected = None;
    for item in items {
        let link = text(item, None, "link")?
            .context("podcast_invalid_feed")?
            .trim();
        let guid = text(item, None, "guid")?
            .context("podcast_invalid_feed")?
            .trim();
        ensure!(
            !guid.is_empty() && guid.len() <= 2048 && link.len() <= 2048,
            "podcast_invalid_feed"
        );
        ensure!(
            urls.insert(link) && guids.insert(guid),
            "podcast_duplicate_identity"
        );
        if link == requested_url {
            selected = Some((item, guid));
        }
    }
    let (item, guid) = selected.context("podcast_episode_not_found")?;
    let season = positive(
        text(item, Some(ITUNES), "season")?
            .context("podcast_invalid_feed")?
            .trim(),
    )?;
    let episode = positive(
        text(item, Some(ITUNES), "episode")?
            .context("podcast_invalid_feed")?
            .trim(),
    )?;
    let (published_date, published_instant) = match text(item, None, "pubDate")? {
        None => (None, None),
        Some(raw) => {
            ensure!(raw.len() <= 256, "podcast_invalid_date");
            let date = chrono::DateTime::parse_from_rfc2822(raw.trim())
                .map_err(|_| anyhow::anyhow!("podcast_invalid_date"))?;
            // Keep the source offset's calendar day, NOT UTC/Taipei conversion.
            let partial = PartialDate {
                value: Some(date.format("%Y-%m-%d").to_string()),
                precision: DatePrecision::Day,
                raw: Some(raw.into()),
            };
            partial
                .validate()
                .map_err(|_| anyhow::anyhow!("podcast_invalid_date"))?;
            (Some(partial), Some(date.to_rfc3339()))
        }
    };
    let duration_raw = text(item, Some(ITUNES), "duration")?.map(str::to_owned);
    let duration_seconds = duration_raw.as_deref().map(duration_seconds).transpose()?;
    let range = item.range();
    Ok(EpisodeMetadata {
        episode_identity: format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(FEED_URL, guid))?)
        ),
        url: requested_url.into(),
        guid: guid.into(),
        season,
        episode,
        rss_sha256: format!("{:x}", Sha256::digest(bytes)),
        source_start_byte: range.start,
        source_end_byte: range.end,
        published_date,
        published_instant,
        duration_seconds,
        duration_raw,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    const URL: &str = "https://ampcode.com/example-episode";
    fn feed(date: &str, duration: &str) -> String {
        format!(
            r#"<rss xmlns:itunes="{ITUNES}"><channel><item><guid>episode-7</guid><link>{URL}</link><itunes:season>2</itunes:season><itunes:episode>7</itunes:episode><pubDate>{date}</pubDate><itunes:duration>{duration}</itunes:duration></item></channel></rss>"#
        )
    }
    #[test]
    fn raw_offset_day_not_service_zone_and_stable_guid_identity() {
        let a = feed("Sun, 08 Nov 2026 23:30:00 -0800", "1:02:03");
        let first = episode_from_feed(a.as_bytes(), URL).unwrap();
        assert_eq!(
            first.published_date.as_ref().unwrap().value.as_deref(),
            Some("2026-11-08")
        );
        assert_eq!(
            first.published_instant.as_deref(),
            Some("2026-11-08T23:30:00-08:00")
        );
        assert_eq!(first.duration_seconds, Some(3723));
        assert!(a[first.source_start_byte..first.source_end_byte].starts_with("<item>"));
        let b = feed("Mon, 09 Nov 2026 07:30:00 +0000", "3723");
        let second = episode_from_feed(b.as_bytes(), URL).unwrap();
        assert_eq!(
            second.published_date.unwrap().value.as_deref(),
            Some("2026-11-09")
        );
        assert_eq!(first.episode_identity, second.episode_identity);
        assert_ne!(first.rss_sha256, second.rss_sha256);
    }
    #[test]
    fn ambiguity_malformed_fields_and_invalid_duration_fail_closed() {
        for (raw, expected) in [("7", 7), ("01:07", 67), ("120:07", 7207), ("00:01:07", 67)] {
            assert_eq!(duration_seconds(raw).unwrap(), expected);
        }
        for raw in [
            "0",
            "1.5",
            "-1",
            "1:60",
            "1:2",
            "1:60:00",
            "1:00:00:00",
            "4294967296",
            "99999999:00",
        ] {
            assert!(duration_seconds(raw).is_err(), "{raw}");
        }
        let good = feed("Sun, 08 Nov 2026 12:00:00 +0000", "67");
        for bad in [
            good.replace("</guid>", "</guid><guid>other</guid>"),
            good.replace("<rss ", "<!DOCTYPE rss [<!ENTITY a 'bad'>]><rss "),
            good.replace("Sun, 08 Nov", "Sun, 32 Nov"),
            good.replace("episode-7</guid>", "episode-7<!--split-->other</guid>"),
        ] {
            assert!(episode_from_feed(bad.as_bytes(), URL).is_err());
        }
        let item = &good[good.find("<item>").unwrap()..good.find("</item>").unwrap() + 7];
        let duplicate = good.replace("</channel>", &format!("{item}</channel>"));
        assert!(episode_from_feed(duplicate.as_bytes(), URL).is_err());
        assert!(
            episode_from_feed(good.as_bytes(), "https://ampcode.com/not-this-episode").is_err()
        );
        let missing = good
            .replace("<pubDate>Sun, 08 Nov 2026 12:00:00 +0000</pubDate>", "")
            .replace("<itunes:duration>67</itunes:duration>", "");
        let result = episode_from_feed(missing.as_bytes(), URL).unwrap();
        assert!(result.published_date.is_none() && result.duration_seconds.is_none());
    }
}
