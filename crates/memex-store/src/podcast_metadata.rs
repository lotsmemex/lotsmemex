//! Trusted owner-local RSS enrichment. Not a remote/untrusted parser endpoint.
use super::*;

pub(crate) const ACTIVITY: &str = "podcast_rss_metadata_fill";
const PROVIDER: &str = "memex.podcast-rss";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PodcastMetadataFill {
    pub item_id: String,
    pub expected_revision_id: String,
    pub request_id: String,
    pub source: PodcastRssSource,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PodcastRssSource {
    pub feed_url: String,
    pub episode_url: String,
    pub guid: String,
    /// `sha256:` prefixed digest of the complete original RSS bytes.
    pub rss_sha256: String,
    /// Half-open UTF-8 byte range of the selected RSS item.
    pub source_start_byte: usize,
    pub source_end_byte: usize,
    pub published_date: Option<PartialDate>,
    pub published_raw: Option<String>,
    pub published_instant: Option<String>,
    pub duration_seconds: Option<u32>,
    pub duration_raw: Option<String>,
}

fn hash(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn source_path(root: &Path, digest: &str, extension: &str) -> Result<PathBuf> {
    anyhow::ensure!(valid_sha256(digest), "invalid_podcast_source_hash");
    Ok(root
        .join("catalog/podcast-rss-sources")
        .join(format!("{}.{}", &digest[7..], extension)))
}

fn provenance(digest: &str) -> MetadataProvenance {
    MetadataProvenance {
        origin: MetadataOrigin::IntakeProvided,
        provider_id: PROVIDER.into(),
        provider_version: "1".into(),
        source_refs: vec![digest.into()],
    }
}

fn validate_input(source: &PodcastRssSource, rss: &[u8], base: &ItemRevision) -> Result<()> {
    anyhow::ensure!(
        rss.len() <= 32 * 1024 * 1024 && hash(rss) == source.rss_sha256,
        "invalid_podcast_rss_hash"
    );
    for url in [&source.feed_url, &source.episode_url] {
        let url = Url::parse(url)?;
        anyhow::ensure!(
            matches!(url.scheme(), "https" | "http")
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none(),
            "invalid_podcast_url"
        );
    }
    anyhow::ensure!(
        base.source_url == source.episode_url
            || base.canonical_url.as_deref() == Some(source.episode_url.as_str()),
        "podcast_episode_url_mismatch"
    );
    let text = std::str::from_utf8(rss)?;
    let episode = text
        .get(source.source_start_byte..source.source_end_byte)
        .context("invalid_podcast_source_range")?;
    // The trusted adapter performs XML parsing. Store independently binds its
    // selected range and raw tokens, including XML-escaped text or CDATA.
    let contains = |value: &str| {
        episode.contains(value)
            || episode.contains(
                &value
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;"),
            )
    };
    anyhow::ensure!(
        episode.starts_with("<item")
            && episode.ends_with("</item>")
            && valid_text(&source.guid, 4096)
            && contains(&source.guid)
            && contains(&source.episode_url),
        "invalid_podcast_episode_binding"
    );
    match (
        &source.published_date,
        &source.published_raw,
        &source.published_instant,
    ) {
        (Some(date), Some(raw), Some(instant)) => {
            anyhow::ensure!(
                valid_partial_date(date)
                    && valid_text(raw, 4096)
                    && contains(raw)
                    && date.raw.as_ref() == Some(raw),
                "invalid_podcast_date"
            );
            let parsed = chrono::DateTime::parse_from_rfc2822(raw.trim())?;
            let instant = chrono::DateTime::parse_from_rfc3339(instant)?;
            anyhow::ensure!(
                parsed == instant
                    && date.value.as_deref()
                        == Some(parsed.format("%Y-%m-%d").to_string().as_str())
                    && date.precision == memex_core::DatePrecision::Day,
                "invalid_podcast_date_binding"
            );
        }
        (None, None, None) => {}
        _ => anyhow::bail!("invalid_podcast_date"),
    }
    match (source.duration_seconds, &source.duration_raw) {
        (Some(seconds), Some(raw)) => {
            anyhow::ensure!(
                raw.trim()
                    .split(':')
                    .enumerate()
                    .all(|(i, part)| !part.is_empty()
                        && part.bytes().all(|b| b.is_ascii_digit())
                        && (i == 0 || part.len() == 2)),
                "invalid_podcast_duration"
            );
            let parts = raw
                .trim()
                .split(':')
                .map(str::parse::<u32>)
                .collect::<std::result::Result<Vec<_>, _>>()?;
            anyhow::ensure!(
                valid_text(raw, 128)
                    && contains(raw)
                    && seconds > 0
                    && !parts.is_empty()
                    && parts.len() <= 3
                    && (parts.len() == 1 || parts[1..].iter().all(|n| *n < 60)),
                "invalid_podcast_duration"
            );
            let value = parts
                .iter()
                .try_fold(0u32, |a, b| a.checked_mul(60)?.checked_add(*b));
            anyhow::ensure!(value == Some(seconds), "invalid_podcast_duration_binding");
        }
        (None, None) => {}
        _ => anyhow::bail!("invalid_podcast_duration"),
    }
    Ok(())
}

fn filled(base: &ItemMetadata, source: &PodcastRssSource, digest: &str) -> Result<ItemMetadata> {
    let mut conflicts = Vec::new();
    let mut next = base.clone();
    if let Some(date) = &source.published_date {
        let legacy = date
            .legacy_string()
            .context("invalid_podcast_date")?
            .to_owned();
        if base.published_date.is_some() || base.published_partial_date.is_some() {
            if base
                .published_date
                .as_ref()
                .is_some_and(|v| v.value != legacy)
                || base.published_partial_date.as_ref().is_some_and(|v| {
                    v.value.value != date.value || v.value.precision != date.precision
                })
            {
                conflicts.push("published_date");
            }
        } else {
            next.published_partial_date = Some(Sourced {
                value: date.clone(),
                provenance: provenance(digest),
            });
            next.published_date = Some(Sourced {
                value: legacy,
                provenance: provenance(digest),
            });
        }
    }
    if let Some(seconds) = source.duration_seconds {
        match &base.duration_seconds {
            Some(existing) if existing.value != seconds => conflicts.push("duration_seconds"),
            Some(_) => {}
            None => {
                next.duration_seconds = Some(Sourced {
                    value: seconds,
                    provenance: provenance(digest),
                })
            }
        }
    }
    anyhow::ensure!(
        conflicts.is_empty(),
        "podcast_metadata_conflict:{}",
        conflicts.join(",")
    );
    Ok(next)
}

pub(crate) fn transition_ok(base: &ItemRevision, next: &ItemRevision) -> bool {
    let Some(activity) = next.provenance.last() else {
        return false;
    };
    let ([_, source], [_, digest]) = (
        activity.input_ids.as_slice(),
        activity.input_checksums.as_slice(),
    ) else {
        return false;
    };
    if source != digest
        || !valid_sha256(digest)
        || next.title != base.title
        || next.metadata == base.metadata
    {
        return false;
    }
    if next.metadata.published_date != base.metadata.published_date
        || next.metadata.published_partial_date != base.metadata.published_partial_date
    {
        if base.metadata.published_date.is_some() || base.metadata.published_partial_date.is_some()
        {
            return false;
        }
        match (
            &next.metadata.published_date,
            &next.metadata.published_partial_date,
        ) {
            (Some(legacy), Some(date))
                if date.value.legacy_string() == Some(legacy.value.as_str()) => {}
            _ => return false,
        }
    }
    let mut rest = next.metadata.clone();
    macro_rules! check {
        ($($field:ident),*) => {$(
            if next.metadata.$field != base.metadata.$field
                && (base.metadata.$field.is_some() || next.metadata.$field.as_ref().is_none_or(|v| v.provenance != provenance(digest))) {
                return false;
            }
            rest.$field = base.metadata.$field.clone();
        )*};
    }
    check!(published_date, published_partial_date, duration_seconds);
    rest == base.metadata && validate_metadata(&next.metadata).is_ok()
}

pub(crate) fn validate_source(root: &Path, base: &ItemRevision, next: &ItemRevision) -> Result<()> {
    let digest = &next
        .provenance
        .last()
        .context("missing podcast activity")?
        .input_checksums[1];
    let source: PodcastRssSource = load(root, source_path(root, digest, "json")?)?;
    anyhow::ensure!(
        hash(&serde_json::to_vec(&source)?) == *digest,
        "invalid_podcast_source_hash"
    );
    let path = source_path(root, &source.rss_sha256, "rss")?;
    let mut rss = Vec::new();
    open_regular_nofollow(&contained_existing(root, &path)?)?
        .take(32 * 1024 * 1024 + 1)
        .read_to_end(&mut rss)?;
    validate_input(&source, &rss, base)?;
    anyhow::ensure!(
        filled(&base.metadata, &source, digest)? == next.metadata,
        "invalid_podcast_metadata_binding"
    );
    Ok(())
}

/// Owner-local worker only. All supplied values are bound to retained RSS;
/// conflicts abort atomically, existing equal values retain their provenance.
pub fn fill_podcast_metadata(
    library: impl AsRef<Path>,
    request: PodcastMetadataFill,
    rss: &[u8],
    max_sensitivity: Sensitivity,
) -> Result<ItemMetadataUpdateReceipt> {
    anyhow::ensure!(
        valid_item_id(&request.item_id)
            && valid_revision_id(&request.expected_revision_id)
            && valid_request_id(&request.request_id),
        "invalid_podcast_metadata_request"
    );
    let request_hash = hash(&serde_json::to_vec(&request)?);
    let root = existing_canonical_root(library.as_ref())?;
    let lock = ingest_lock(&root)?;
    recover_if_pending_locked(&root)?;
    let (item, current) = read_item_at_sensitivity(&root, &request.item_id, max_sensitivity)?
        .filter(|(i, r)| i.sensitivity.max(r.sensitivity) <= max_sensitivity)
        .context("library_resource_not_found")?;
    validate_input(&request.source, rss, &current)?;
    if let Some(record) = metadata_v2::load_metadata_request(&root, &request.request_id)? {
        anyhow::ensure!(
            record.request_sha256 == request_hash && record.receipt.item_id == request.item_id,
            "request_id_conflict"
        );
        // Current visibility alone does not authorize historical receipt replay.
        for id in [
            &record.receipt.previous_revision_id,
            &record.receipt.revision_id,
        ] {
            let revision: ItemRevision = load(
                &root,
                root.join("items")
                    .join(&item.item_id)
                    .join("revisions")
                    .join(format!("{id}.json")),
            )?;
            anyhow::ensure!(
                revision.sensitivity <= max_sensitivity,
                "library_resource_not_found"
            );
        }
        lock.unlock()?;
        return Ok(record.receipt);
    }
    anyhow::ensure!(
        current.revision_id == request.expected_revision_id,
        "stale_item_revision"
    );
    let (_, legacy) = read_legacy_item(&root, &item.item_id)?;
    anyhow::ensure!(
        current
            .revision_number
            .checked_sub(legacy.revision_number)
            .is_some_and(|n| n < MAX_ITEM_REVISION_CHAIN),
        "item_revision_limit_exceeded"
    );
    let source_bytes = serde_json::to_vec(&request.source)?;
    let digest = hash(&source_bytes);
    let metadata = filled(&current.metadata, &request.source, &digest)?;
    anyhow::ensure!(metadata != current.metadata, "metadata_unchanged");
    let mut revision = owner_edit_revision(&current, ACTIVITY, current.title.clone(), metadata)?;
    let activity = revision.provenance.last_mut().unwrap();
    activity.input_ids.push(digest.clone());
    activity.input_checksums.push(digest.clone());
    validate_owner_edit_transition(&current, &revision)?;
    ensure_contained_dir(&root, Path::new("catalog/podcast-rss-sources"))?;
    install_binary(
        &root,
        &source_path(&root, &request.source.rss_sha256, "rss")?,
        rss,
    )?;
    install_binary(&root, &source_path(&root, &digest, "json")?, &source_bytes)?;
    let receipt = commit_owner_revision_locked(
        &root,
        item,
        &current,
        &revision,
        Some((request.request_id, request_hash)),
    )?;
    lock.unlock()?;
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata_v2::tests::{tree, v2_edit};
    use crate::tests::{capture, draft};
    use tempfile::tempdir;

    fn fixture(sensitivity: Sensitivity) -> (tempfile::TempDir, PodcastMetadataFill, Vec<u8>) {
        let d = tempdir().unwrap();
        let mut c = capture();
        c.capture.sensitivity = sensitivity;
        let mut value = draft();
        value.sensitivity = sensitivity;
        value.source_url = "https://example.invalid/podcast/1".into();
        let receipt = ingest(d.path(), &c, value).unwrap();
        let raw = "Fri, 06 Nov 2026 23:00:00 -0800";
        let rss = format!("<rss><channel><item><guid>episode-1</guid><link>https://example.invalid/podcast/1</link><pubDate>{raw}</pubDate><itunes:duration>1:02:03</itunes:duration></item></channel></rss>").into_bytes();
        let text = std::str::from_utf8(&rss).unwrap();
        let request = PodcastMetadataFill {
            item_id: receipt.item_ids[0].clone(),
            expected_revision_id: receipt.revision_ids[0].clone(),
            request_id: "podcast-1".into(),
            source: PodcastRssSource {
                feed_url: "https://example.invalid/podcast.rss".into(),
                episode_url: "https://example.invalid/podcast/1".into(),
                guid: "episode-1".into(),
                rss_sha256: hash(&rss),
                source_start_byte: text.find("<item>").unwrap(),
                source_end_byte: text.find("</item>").unwrap() + 7,
                published_date: Some(PartialDate {
                    value: Some("2026-11-06".into()),
                    precision: memex_core::DatePrecision::Day,
                    raw: Some(raw.into()),
                }),
                published_raw: Some(raw.into()),
                published_instant: Some("2026-11-07T07:00:00Z".into()),
                duration_seconds: Some(3723),
                duration_raw: Some("1:02:03".into()),
            },
        };
        (d, request, rss)
    }

    #[test]
    fn podcast_metadata_replay_cas_and_strict_transition() {
        let (d, request, rss) = fixture(Sensitivity::Public);
        let base = read_item(d.path(), &request.item_id).unwrap().1;
        let receipt =
            fill_podcast_metadata(d.path(), request.clone(), &rss, Sensitivity::Public).unwrap();
        let next = read_item(d.path(), &request.item_id).unwrap().1;
        assert_eq!(next.metadata.duration_seconds.as_ref().unwrap().value, 3723);
        assert_eq!(
            next.metadata.published_date.as_ref().unwrap().value,
            "2026-11-06"
        );
        assert_eq!(next.content_markdown, base.content_markdown);
        assert_eq!(next.evidence_ids, base.evidence_ids);
        assert_eq!(next.attachments, base.attachments);
        let before = tree(d.path());
        assert_eq!(
            fill_podcast_metadata(d.path(), request.clone(), &rss, Sensitivity::Public).unwrap(),
            receipt
        );
        let mut competing = request.clone();
        competing.request_id = "podcast-2".into();
        assert_eq!(
            fill_podcast_metadata(d.path(), competing, &rss, Sensitivity::Public)
                .unwrap_err()
                .to_string(),
            "stale_item_revision"
        );
        let mut conflicting = request.clone();
        conflicting.source.feed_url.push_str("?changed");
        assert_eq!(
            fill_podcast_metadata(d.path(), conflicting, &rss, Sensitivity::Public)
                .unwrap_err()
                .to_string(),
            "request_id_conflict"
        );
        assert_eq!(tree(d.path()), before);
        for mutation in 0..5 {
            let mut forged = next.clone();
            match mutation {
                0 => forged.title = "forged".into(),
                1 => forged.attachments.push(AttachmentRef::Remote {
                    url: "https://example.invalid/x".into(),
                    media_type: None,
                    title: None,
                }),
                2 => {
                    forged
                        .metadata
                        .duration_seconds
                        .as_mut()
                        .unwrap()
                        .provenance = owner_provenance()
                }
                3 => forged.metadata.published_partial_date = None,
                _ => {
                    forged.metadata.publisher = Some(Sourced {
                        value: "forged".into(),
                        provenance: owner_provenance(),
                    })
                }
            }
            assert!(validate_owner_edit_transition(&base, &forged).is_err());
        }
        // Mutation of source bytes after a cached read must invalidate the chain.
        fs::write(
            source_path(d.path(), &request.source.rss_sha256, "rss").unwrap(),
            b"corrupt",
        )
        .unwrap();
        assert!(read_item(d.path(), &request.item_id).is_err());
    }

    #[test]
    fn podcast_metadata_rejects_bad_binding_and_preserves_owner_values() {
        let (d, request, rss) = fixture(Sensitivity::Public);
        let before = tree(d.path());
        for mutation in 0..6 {
            let mut bad = request.clone();
            match mutation {
                0 => bad.source.rss_sha256 = hash(b"wrong"),
                1 => bad.source.source_start_byte += 1,
                2 => bad.source.episode_url.push_str("/other"),
                3 => bad.source.duration_seconds = Some(0),
                4 => bad.source.published_date.as_mut().unwrap().value = Some("2026-02-30".into()),
                _ => bad.source.guid = "different-guid".into(),
            }
            assert!(fill_podcast_metadata(d.path(), bad, &rss, Sensitivity::Public).is_err());
            assert_eq!(tree(d.path()), before);
        }
        let edit = revise_item_metadata_v2(
            d.path(),
            &request.item_id,
            v2_edit(
                &request.item_id,
                &request.expected_revision_id,
                "owner-date",
                serde_json::json!({"published_date": {"value":"2025", "precision":"year"}}),
                &[],
            ),
            Sensitivity::Public,
        )
        .unwrap();
        let mut request = request;
        request.expected_revision_id = edit.revision_id;
        let before = tree(d.path());
        assert_eq!(
            fill_podcast_metadata(d.path(), request.clone(), &rss, Sensitivity::Public)
                .unwrap_err()
                .to_string(),
            "podcast_metadata_conflict:published_date"
        );
        assert_eq!(tree(d.path()), before);
        request.source.published_date = None;
        request.source.published_raw = None;
        request.source.published_instant = None;
        fill_podcast_metadata(d.path(), request.clone(), &rss, Sensitivity::Public).unwrap();
        let next = read_item(d.path(), &request.item_id).unwrap().1;
        assert_eq!(
            next.metadata.published_date.unwrap().provenance.origin,
            MetadataOrigin::OwnerEdit
        );
    }

    #[test]
    fn podcast_metadata_preserves_e5_translation_and_bound_marks() {
        use crate::reader::{self, annotations_ai as marks, chapter_translation as ct};
        use memex_core::{BlobDescriptor, VerifiedBlob};
        let (_unused, mut request, rss) = fixture(Sensitivity::Public);
        let d = tempdir().unwrap();
        let original = "# One\n\nEnglish source.\n";
        let translation = "# 一\n\n另一版本。\n";
        let editions = [
            ("original", "en", original),
            ("translation", "zh", translation),
        ]
        .map(|(side, language, body)| reader::Edition {
            side: side.into(),
            language: language.into(),
            sha256: reader::reader_sha256(body.as_bytes()),
            bytes: body.len(),
            chapters: vec![reader::Chapter {
                key: "one".into(),
                title: "One".into(),
                level: 1,
                start: 0,
                end: body.len(),
                counterpart: None,
            }],
        });
        let manifest = reader::BookManifest {
            schema_version: 1,
            alignment: "declared_chapter_order_not_paragraph_alignment".into(),
            editions: editions.to_vec(),
        };
        let mut c = capture();
        c.blobs = [
            ("original.md", "text/markdown", original.as_bytes().to_vec()),
            (
                "translation.md",
                "text/markdown",
                translation.as_bytes().to_vec(),
            ),
            (
                "manifest.json",
                reader::BOOK_MANIFEST,
                serde_json::to_vec(&manifest).unwrap(),
            ),
        ]
        .into_iter()
        .map(|(name, media, bytes)| {
            let sha = reader::reader_sha256(&bytes);
            VerifiedBlob {
                descriptor: BlobDescriptor {
                    schema_version: 1,
                    locator: name.into(),
                    media_type: media.into(),
                    byte_size: bytes.len() as u64,
                    sha256: sha.clone(),
                    source_url: None,
                },
                blob_id: format!("sha256:{sha}"),
                source_backend_ref: None,
                bytes,
            }
        })
        .collect();
        c.capture.artifacts = c.blobs.iter().map(|b| b.descriptor.clone()).collect();
        let mut value = draft();
        value.source_url = request.source.episode_url.clone();
        value.content_markdown = translation.into();
        let receipt = reader::ingest_reader_book(d.path(), c, value).unwrap();
        request.item_id = receipt.item_ids[0].clone();
        request.expected_revision_id = receipt.revision_ids[0].clone();
        let source = ct::source(
            d.path(),
            &request.item_id,
            &request.expected_revision_id,
            "one",
            Sensitivity::Public,
        )
        .unwrap();
        let ai = ct::publish(
            d.path(),
            source.identity,
            ct::Provenance {
                route: ct::Route {
                    provider_id: "synthetic".into(),
                    model_id: "fixture".into(),
                    reasoning_effort: None,
                },
                prompt_version: "v1".into(),
                prompt_sha256: reader::reader_sha256(b"prompt"),
                plan_sha256: reader::reader_sha256(b"plan"),
            },
            "# 一\n\n机器译文。\n".into(),
            Sensitivity::Public,
        )
        .unwrap();
        let page = marks::list(
            d.path(),
            &request.item_id,
            None,
            0,
            100,
            None,
            None,
            Sensitivity::Public,
        )
        .unwrap();
        marks::edit(
            d.path(),
            &request.item_id,
            &page.version,
            marks::Change::Put {
                annotation: marks::Annotation {
                    id: "mark-one".into(),
                    style: "red".into(),
                    anchor: marks::Anchor {
                        revision_id: request.expected_revision_id.clone(),
                        side: marks::SPACE.into(),
                        segments: vec![marks::Segment {
                            chapter: "one".into(),
                            translation_version: ai.version.clone(),
                            sha256: ai.sha256.clone(),
                            start: 0,
                            end: 5,
                        }],
                    },
                },
            },
            "owner",
            Sensitivity::Public,
        )
        .unwrap();
        let next =
            fill_podcast_metadata(d.path(), request.clone(), &rss, Sensitivity::Public).unwrap();
        let inherited = ct::metadata(
            d.path(),
            &request.item_id,
            &next.revision_id,
            "one",
            Sensitivity::Public,
        )
        .unwrap()
        .unwrap();
        assert_eq!(inherited.sha256, ai.sha256);
        assert_ne!(inherited.version, ai.version);
        let page = marks::list(
            d.path(),
            &request.item_id,
            None,
            0,
            100,
            None,
            None,
            Sensitivity::Public,
        )
        .unwrap();
        assert_eq!(page.annotations.len(), 1);
        assert_eq!(page.annotations[0].id, "mark-one");
        // Marks stay bound to their revision, and a metadata-only revision (same body) keeps
        // them current: the item's derived data follows its body, not its title.
        assert_eq!(page.annotations[0].status, "current");
    }

    #[test]
    fn podcast_metadata_concurrent_cas_has_one_winner() {
        let (d, request, rss) = fixture(Sensitivity::Public);
        let barrier = std::sync::Barrier::new(2);
        let results = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..2)
                .map(|i| {
                    let mut request = request.clone();
                    request.request_id = format!("race-{i}");
                    let (barrier, root, rss) = (&barrier, d.path(), &rss);
                    scope.spawn(move || {
                        barrier.wait();
                        fill_podcast_metadata(root, request, rss, Sensitivity::Public)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(
            results
                .into_iter()
                .find_map(Result::err)
                .unwrap()
                .to_string(),
            "stale_item_revision"
        );
        assert_eq!(
            read_item(d.path(), &request.item_id)
                .unwrap()
                .1
                .revision_number,
            2
        );
    }

    #[test]
    fn podcast_metadata_permissions_and_crash_copy_recovery() {
        let (d, request, rss) = fixture(Sensitivity::Private);
        let before = tree(d.path());
        assert_eq!(
            fill_podcast_metadata(d.path(), request.clone(), &rss, Sensitivity::Public)
                .unwrap_err()
                .to_string(),
            "library_resource_not_found"
        );
        assert_eq!(tree(d.path()), before);
        FAIL_ITEM_UPDATE_STEP.with(|v| v.set(Some(3)));
        let result = fill_podcast_metadata(d.path(), request.clone(), &rss, Sensitivity::Private);
        FAIL_ITEM_UPDATE_STEP.with(|v| v.set(None));
        assert!(result.is_err());
        // Stopped-writer copy, including raw audit source and pending journal.
        let restored = tempdir().unwrap();
        for (relative, bytes) in tree(d.path()) {
            let target = restored.path().join(&relative);
            if d.path().join(&relative).is_dir() {
                fs::create_dir_all(target).unwrap();
            } else {
                fs::create_dir_all(target.parent().unwrap()).unwrap();
                fs::write(target, bytes).unwrap();
            }
        }
        let receipt =
            fill_podcast_metadata(restored.path(), request.clone(), &rss, Sensitivity::Private)
                .unwrap();
        assert_eq!(
            read_item(restored.path(), &request.item_id)
                .unwrap()
                .1
                .revision_id,
            receipt.revision_id
        );
        assert_eq!(
            fill_podcast_metadata(restored.path(), request.clone(), &rss, Sensitivity::Private)
                .unwrap(),
            receipt
        );
        assert_eq!(
            fill_podcast_metadata(restored.path(), request, &rss, Sensitivity::Public)
                .unwrap_err()
                .to_string(),
            "library_resource_not_found"
        );
    }
}
