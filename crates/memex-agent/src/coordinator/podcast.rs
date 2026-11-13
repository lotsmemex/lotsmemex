//! One ordinary URL job owns capture/enrichment and its restartable artifacts.
//! No model entry point is used here; the queue also durably suppresses hooks.
use super::*;
use crate::{fetch::fetch_podcast_resource, queue::ChromeOptions};
use memex_intake::url_jobs::{podcast::{episode_from_feed, EpisodeMetadata, FEED_URL}, GenerationPolicy, QualityMode};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact { digest: String, size: u64, media: String }
impl Artifact {
    fn save(spool: &Path, bytes: &[u8], media: &str) -> Result<Self> {
        let digest = hex::encode(Sha256::digest(bytes));
        install(spool, &digest, bytes)?;
        Ok(Self { digest, size: bytes.len() as u64, media: media.into() })
    }
    fn read(&self, spool: &Path) -> Result<Vec<u8>> {
        anyhow::ensure!(self.digest.len() == 64 && self.digest.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()), "artifact_mismatch");
        read_exact_path(&spool.join("cas").join(&self.digest[..2]).join(&self.digest), &self.digest, self.size)
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Work {
    rss: Option<Artifact>,
    page: Option<Artifact>,
    captured_at: Option<String>,
    cover: Option<Artifact>,
    cover_source: Option<memex_store::covers::CoverSource>,
    cover_checked: bool,
    item_id: Option<String>,
    metadata_request: Option<memex_store::podcast_metadata::PodcastMetadataFill>,
    metadata_done: bool,
    conflicts: Vec<String>,
    cover_edit: Option<memex_store::covers::CoverEdit>,
    cover_status: Option<String>,
    archive_request: Option<memex_store::PodcastArchiveRequest>,
}

/// Exact URL identity, not title matching. Ambiguous duplicates are never a
/// license to create another item. Prefer the sole already-filed copy only.
pub(crate) fn existing(root: &Path, url: &str, target: Option<&str>, sources: &[String], expanded: bool, index: &std::sync::Mutex<SourceIndex>) -> Result<Option<String>> {
    existing_with_index(root, url, target, sources, expanded, index, false)
}

fn existing_with_index(root: &Path, url: &str, target: Option<&str>, sources: &[String], expanded: bool, index: &std::sync::Mutex<SourceIndex>, ready_only: bool) -> Result<Option<String>> {
    memex_store::with_consistent_snapshot(root, |root| {
        let groups = memex_store::read_visible_groups(root, Sensitivity::Restricted)?;
        let destination = target.map(|id| groups.nodes.iter().find(|n| n.group_id == id).context("library_resource_not_found")).transpose()?;
        if let Some(destination) = destination {
            anyhow::ensure!(expanded || destination.parent_id.as_deref() == Some(&destination.root_group_id), "invalid_podcast_target");
            let mut node = destination;
            loop {
                anyhow::ensure!(!node.read_only && node.owner_id == "owner_local" && node.root_group_id == destination.root_group_id, "invalid_podcast_target");
                let Some(parent) = node.parent_id.as_deref() else {
                    anyhow::ensure!(node.group_id == destination.root_group_id, "invalid_podcast_target");
                    break;
                };
                node = groups.nodes.iter().find(|n|n.group_id == parent).context("library_resource_not_found")?;
            }
        } else { anyhow::ensure!(expanded && sources.is_empty(), "invalid_podcast_target"); }
        anyhow::ensure!(sources.len() <= 2, "invalid_podcast_target");
        for id in sources {
            let destination = destination.context("invalid_podcast_target")?;
            let node = groups.nodes.iter().find(|n| n.group_id == *id).context("library_resource_not_found")?;
            anyhow::ensure!(node.group_id != destination.group_id && !node.read_only && node.owner_id == "owner_local"
                && node.root_group_id == destination.root_group_id && (node.group_id == destination.root_group_id || node.parent_id.as_deref() == Some(&destination.root_group_id)), "invalid_podcast_target");
        }
        if expanded && !memex_intake::url_jobs::podcast::is_episode_url(url) { return Ok(None); }
        let mut ids = if ready_only {
            let mut index = match index.try_lock() {
                Ok(index) => index,
                Err(std::sync::TryLockError::WouldBlock) => return Err(super::source_index::SourceIndexNotReady.into()),
                Err(_) => anyhow::bail!("source_index_unavailable"),
            };
            index.matching_ready(root, url)?
        } else {
            index.lock().map_err(|_| anyhow::anyhow!("source_index_unavailable"))?.matching(root, url)?
        };
        if ids.len() <= 1 { return Ok(ids.pop()); }
        let filed: Vec<_> = ids.into_iter().filter(|id| groups.memberships.iter().any(|m| m.item_id == *id
            && groups.nodes.iter().any(|n| n.group_id == m.group_id && destination.is_none_or(|d| n.root_group_id == d.root_group_id)))).collect();
        anyhow::ensure!(filed.len() == 1, "podcast_duplicate_conflict");
        Ok(filed.into_iter().next())
    })
}

impl<R: Resolver, T: HttpTransport> Coordinator<R, T> {
    pub(super) fn process_podcast(&mut self, job: JobRecord) -> Result<bool> {
        let result = self.run_podcast(&job);
        if let Err(error) = result {
            // Never persist network bodies, titles or arbitrary error text.
            if let Some(rejected) = error.downcast_ref::<memex_capture::RejectedAdmission>() {
                self.queue.finish(&job, State::NeedsUser, Stage::Validate,
                    Some(super::admission_class_text(rejected.0.class)), 0)?;
                return Ok(true);
            }
            let conflict = matches!(error.to_string().as_str(), "podcast_duplicate_conflict" | "invalid_podcast_target" | "library_resource_not_found" | "stale_item_revision" | "stale_group_version");
            self.queue.finish(&job,
                if conflict { State::NeedsUser } else if job.view.attempt_count >= MAX_AUTOMATIC_ATTEMPTS { State::Failed } else { State::RetryWait },
                Stage::Commit, Some(if conflict { "podcast_conflict" } else { "podcast_retry" }), 30)?;
        }
        Ok(true)
    }

    fn podcast_fetch(&mut self, job: &JobRecord, url: &str, resource: bool, first: bool) -> Result<crate::fetch::Fetched> {
        self.queue.renew_episode(job)?;
        let queue = &mut self.queue;
        let mut before = |url: &url::Url, hops: u8| -> Result<()> {
            let origin = url.origin().ascii_serialization();
            let started = std::time::Instant::now();
            loop {
                let now = chrono::Utc::now().timestamp();
                let Some(until) = queue.reserve_capture_hop(job, &origin, first && hops == 0, now)? else { return Ok(()) };
                let wait = std::time::Duration::from_secs(until.saturating_sub(now) as u64);
                if started.elapsed() + wait > std::time::Duration::from_secs(25) {
                    return Err(OriginDeferred { origin, not_before: until }.into());
                }
                std::thread::sleep(wait);
            }
        };
        let fetched = if resource {
            fetch_podcast_resource(url, &self.resolver, &self.transport, &mut before)
        } else { fetch_scheduled(url, &self.resolver, &self.transport, &mut before) };
        match fetched {
            Ok(value) => Ok(value),
            Err(error) => {
                if let Some((origin, until)) = error.retry_after() {
                    self.queue.defer_capture_origin(origin, until)?;
                }
                Err(error.into())
            }
        }
    }

    fn run_podcast(&mut self, job: &JobRecord) -> Result<()> {
        let options: ChromeOptions = self.queue.chrome_options(&job.view.job_id)?.context("invalid_request")?;
        anyhow::ensure!(options.generation_policy == Some(GenerationPolicy::None) && options.quality_check == QualityMode::Off && !options.include_comments, "invalid_request");
        let target = options.target_group_id.as_deref();
        existing(&self.cfg.library, &job.url, target, &options.remove_from_group_ids, options.destination_mode.is_some(), &self.source_index)?;
        let mut work: Work = self.queue.episode_work(&job.view.job_id)?.unwrap_or_default();
        self.queue.stage(job, Stage::Fetch)?;
        if work.rss.is_none() {
            let fetched = self.podcast_fetch(job, FEED_URL, true, true)?;
            episode_from_feed(&fetched.body, &job.url)?;
            work.rss = Some(Artifact::save(&self.cfg.spool, &fetched.body, &fetched.media_type)?);
            self.queue.save_episode_work(job, &work)?;
        }
        let rss = work.rss.as_ref().context("artifact_mismatch")?.read(&self.cfg.spool)?;
        let episode = episode_from_feed(&rss, &job.url)?;
        if work.page.is_none() {
            let fetched = self.podcast_fetch(job, &episode.url, false, false)?;
            anyhow::ensure!(fetched.final_url.as_str() == episode.url, "podcast_page_identity");
            work.page = Some(Artifact::save(&self.cfg.spool, &fetched.body, &fetched.media_type)?);
            work.captured_at = Some(chrono::Utc::now().to_rfc3339());
            self.queue.save_episode_work(job, &work)?;
        }
        let page = work.page.as_ref().context("artifact_mismatch")?;
        let page_bytes = page.read(&self.cfg.spool)?;
        let retry_missing_cover = job.view.reason_code.as_deref() == Some("manual_retry")
            && job.view.attempt_count == 1 && work.cover.is_none() && work.cover_edit.is_none();
        if !work.cover_checked || retry_missing_cover {
            // A cover failure is an explicit partial result, not a failed capture.
            // Explicit retry may recover an older parser's negative result once;
            // automatic commit retries keep the frozen result and fetched bytes.
            let video = std::str::from_utf8(&page_bytes).ok().and_then(|html| lots_scout_extract::podcast_video_id(html).ok()).flatten();
            if let Some(video) = video {
                let image_url = format!("https://i.ytimg.com/vi/{video}/hqdefault.jpg");
                if let Ok(fetched) = self.podcast_fetch(job, &image_url, true, false) {
                    work.cover = Some(Artifact::save(&self.cfg.spool, &fetched.body, &fetched.media_type)?);
                    if work.cover_status.as_deref() == Some("unavailable") && work.cover_edit.is_none() {
                        work.cover_status = None;
                    }
                    work.cover_source = Some(memex_store::covers::CoverSource::Podcast {
                        episode_url: episode.url.clone(), youtube_video_id: video,
                        image_url: fetched.final_url.to_string(), rss_sha256: episode.rss_sha256.clone(),
                    });
                }
            }
            work.cover_checked = true;
            self.queue.save_episode_work(job, &work)?;
        }
        self.queue.renew_episode(job)?;
        self.queue.stage(job, Stage::Commit)?;
        let library = self.cfg.library.clone();
        let mut item_id = None;
        for _ in 0..3 {
            // Refresh and wait for other builders BEFORE taking the queue lock.
            existing(&library, &episode.url, target, &options.remove_from_group_ids, options.destination_mode.is_some(), &self.source_index)?;
            self.queue.renew_episode(job)?;
            let result = self.queue.episode_commit(job, || {
                if let Some(id) = existing_with_index(&library, &episode.url, target, &options.remove_from_group_ids, options.destination_mode.is_some(), &self.source_index, true)? { return Ok(id); }
                anyhow::ensure!(work.item_id.is_none(), "library_resource_not_found");
                let cap = web_capture(&format!("amp_podcast_{}", episode.episode_identity), work.captured_at.as_deref().context("artifact_mismatch")?,
                    &episode.url, &page.media, &page.digest, page_bytes.clone());
                let prepared = prepare_capture(CaptureSubmission { capture: cap, metadata: ItemMetadata::default(), attachments: vec![] },
                    AdmissionContext { http_status: Some(200), preview_approval: None })?;
                let receipt = commit_capture(&library, prepared)?;
                receipt.item_ids.first().cloned().context("invalid_item_receipt")
            });
            match result {
                Ok(id) => { item_id = Some(id); break; }
                Err(error) if error.is::<super::source_index::SourceIndexNotReady>() => continue,
                Err(error) => return Err(error),
            }
        }
        work.item_id = Some(item_id.context("source_index_busy")?);
        self.queue.save_episode_work(job, &work)?;
        self.enrich_podcast(job, &options, &episode, &rss, &mut work)
    }

    fn enrich_podcast(&mut self, job: &JobRecord, options: &ChromeOptions, episode: &EpisodeMetadata, rss: &[u8], work: &mut Work) -> Result<()> {
        use memex_store::podcast_metadata::{fill_podcast_metadata, PodcastMetadataFill, PodcastRssSource};
        let item = work.item_id.clone().context("invalid_item_receipt")?;
        let library = self.cfg.library.clone();
        let current = || memex_store::read_item_at_sensitivity(&library, &item, Sensitivity::Restricted)?.context("library_resource_not_found");
        if !work.metadata_done {
            if work.metadata_request.is_none() {
                let (_, revision) = current()?;
                let m = &revision.metadata;
                let date_missing = m.published_date.is_none() && m.published_partial_date.is_none();
                let duration_missing = m.duration_seconds.is_none();
                if let Some(date) = &episode.published_date {
                    if !date_missing && (m.published_date.as_ref().is_some_and(|v| Some(v.value.as_str()) != date.value.as_deref())
                        || m.published_partial_date.as_ref().is_some_and(|v| v.value.value != date.value || v.value.precision != date.precision)) {
                        work.conflicts.push("published_date".into());
                    }
                }
                if m.duration_seconds.as_ref().is_some_and(|v| episode.duration_seconds.is_some_and(|n| v.value != n)) { work.conflicts.push("duration_seconds".into()); }
                if (date_missing && episode.published_date.is_some()) || (duration_missing && episode.duration_seconds.is_some()) {
                    let published_date = episode.published_date.clone().filter(|_| date_missing);
                    work.metadata_request = Some(PodcastMetadataFill {
                        item_id: item.clone(), expected_revision_id: revision.revision_id,
                        request_id: format!("{}:metadata", job.view.job_id),
                        source: PodcastRssSource {
                            feed_url: FEED_URL.into(), episode_url: episode.url.clone(), guid: episode.guid.clone(),
                            rss_sha256: format!("sha256:{}", episode.rss_sha256), source_start_byte: episode.source_start_byte, source_end_byte: episode.source_end_byte,
                            published_raw: published_date.as_ref().and_then(|d| d.raw.clone()),
                            published_instant: episode.published_instant.clone().filter(|_| date_missing), published_date,
                            duration_seconds: episode.duration_seconds.filter(|_| duration_missing), duration_raw: episode.duration_raw.clone().filter(|_| duration_missing),
                        },
                    });
                }
                self.queue.save_episode_work(job, work)?;
            }
            if let Some(request) = &work.metadata_request {
                let receipt = fill_podcast_metadata(&self.cfg.library, request.clone(), rss, Sensitivity::Restricted)?;
                if memex_analysis::translation_status(&self.cfg.library, &item, &receipt.previous_revision_id)?
                    .is_some_and(|status| status.state == memex_analysis::TranslationJobState::Succeeded) {
                    anyhow::ensure!(matches!(memex_analysis::preserve_completed_translation(
                        &self.cfg.library, &item, &receipt.previous_revision_id, &receipt.revision_id)?,
                        memex_analysis::TranslationPreservation::Preserved(_)), "translation_preservation_gap");
                }
            }
            work.metadata_done = true;
            self.queue.save_episode_work(job, work)?;
        }
        if work.cover_status.is_none() {
            let existing = memex_store::covers::cover_batch(&self.cfg.library, &[item.clone()], Sensitivity::Restricted)?.remove(0);
            if work.cover_edit.is_none() && existing.is_some() {
                work.cover_status = Some("preserved".into());
            } else if let (Some(artifact), Some(source)) = (&work.cover, &work.cover_source) {
                if work.cover_edit.is_none() {
                    work.cover_edit = Some(memex_store::covers::CoverEdit {
                        item_id: item.clone(), expected_revision_id: current()?.1.revision_id,
                        expected_cover_version: None, request_id: format!("{}:cover", job.view.job_id), source: source.clone(),
                    });
                    self.queue.save_episode_work(job, work)?;
                }
                let bytes = artifact.read(&self.cfg.spool)?;
                match memex_store::covers::put_cover(&self.cfg.library, work.cover_edit.clone().unwrap(), &bytes, rss, Sensitivity::Restricted) {
                    Ok(_) => work.cover_status = Some("stored".into()),
                    Err(e) if matches!(e.to_string().as_str(), "invalid_cover_image" | "cover_placeholder") => work.cover_status = Some("unavailable".into()),
                    Err(e) => return Err(e),
                }
            } else { work.cover_status = Some("unavailable".into()); }
            self.queue.save_episode_work(job, work)?;
        }
        if options.target_group_id.is_some() {
            self.archive_destination(job, options, &item, work)?;
        }
        let revision = current()?.1.revision_id;
        let has_memberships = memex_store::with_consistent_snapshot(&self.cfg.library, |root| {
            Ok(memex_store::read_visible_groups(root, Sensitivity::Restricted)?.memberships.iter().any(|m| m.item_id == item))
        })?;
        self.queue.set_chrome_result(job, &serde_json::json!({"enrichment": {
            "state": if work.cover_status.as_deref() == Some("unavailable") || !work.conflicts.is_empty() { "partial" } else { "complete" },
            "cover": work.cover_status, "metadata_conflicts": work.conflicts, "archived": options.target_group_id.is_some(),
            "placement": if options.target_group_id.is_some() { "selected_group" } else if has_memberships { "existing_locations_preserved" } else { "unfiled" },
        }}))?;
        self.queue.finish_stored(job, &item, &revision)
    }

    pub(super) fn archive_capture_destination(&mut self, job: &JobRecord, item: &str) -> Result<()> {
        if let Some(options) = self.queue.chrome_options(&job.view.job_id)? {
            if options.target_group_id.is_some() {
                let mut work: Work = self.queue.episode_work(&job.view.job_id)?.unwrap_or_default();
                self.archive_destination(job, &options, item, &mut work)?;
            }
        }
        Ok(())
    }

    fn archive_destination(&mut self, job: &JobRecord, options: &ChromeOptions, item: &str, work: &mut Work) -> Result<()> {
        existing(&self.cfg.library, &job.url, options.target_group_id.as_deref(), &options.remove_from_group_ids, options.destination_mode.is_some(), &self.source_index)?;
        if work.archive_request.is_none() {
            work.archive_request = Some(memex_store::with_consistent_snapshot(&self.cfg.library, |root| {
                let (_, revision) = memex_store::read_item_at_sensitivity(root, &item, Sensitivity::Restricted)?.context("library_resource_not_found")?;
                let groups = memex_store::read_visible_groups(root, Sensitivity::Restricted)?;
                let target = groups.nodes.iter().find(|n| Some(&n.group_id) == options.target_group_id.as_ref()).context("library_resource_not_found")?;
                let mut sources = vec![];
                for id in &options.remove_from_group_ids {
                    let node = groups.nodes.iter().find(|n| n.group_id == *id).context("library_resource_not_found")?;
                    anyhow::ensure!(node.root_group_id == target.root_group_id && (node.group_id == target.root_group_id || node.parent_id.as_deref() == Some(&target.root_group_id)) && node.group_id != target.group_id && !node.read_only && node.owner_id == "owner_local", "invalid_podcast_target");
                    if groups.memberships.iter().any(|m| m.item_id == item && m.group_id == *id) {
                        sources.push(memex_store::PodcastArchiveSource { group_id: id.clone(), expected_version: node.version_token() });
                    }
                }
                Ok(memex_store::PodcastArchiveRequest {
                    item_id: item.to_owned(), expected_revision_id: revision.revision_id, request_id: format!("{}:archive", job.view.job_id),
                    target_group_id: target.group_id.clone(), expected_target_version: target.version_token(), sources,
                    allow_root_or_nested_target: options.destination_mode.is_some(),
                })
            })?);
            self.queue.save_episode_work(job, work)?;
        }
        memex_store::archive_podcast(&self.cfg.library, work.archive_request.clone().unwrap(), Sensitivity::Restricted)?;
        // A historical receipt does not mean the owner has not moved it since.
        memex_store::with_consistent_snapshot(&self.cfg.library, |root| {
            let groups = memex_store::read_visible_groups(root, Sensitivity::Restricted)?;
            anyhow::ensure!(groups.memberships.iter().any(|m| m.item_id == item && Some(&m.group_id) == options.target_group_id.as_ref())
                && !groups.memberships.iter().any(|m| m.item_id == item && options.remove_from_group_ids.contains(&m.group_id)), "stale_group_membership");
            Ok(())
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch::RawResponse;
    use memex_intake::url_jobs::{UrlEnqueue, UrlJobBackend, UrlJobCommand};
    use std::{net::{IpAddr, SocketAddr}, sync::{Arc, atomic::{AtomicUsize, Ordering}}, time::Duration};

    struct Network { database: PathBuf, calls: Arc<AtomicUsize>, rss: Vec<u8>, page: Option<String> }
    impl Resolver for Network {
        fn resolve(&self, _: &str, _: Duration) -> Result<Vec<IpAddr>> { Ok(vec!["93.184.216.34".parse()?]) }
    }
    impl HttpTransport for Network {
        fn get(&self, url: &url::Url, pinned: &[SocketAddr], _: Duration) -> Result<RawResponse> {
            assert!(!pinned.is_empty());
            self.calls.fetch_add(1, Ordering::SeqCst);
            // Synthetic transport advances only the test scheduler's due time;
            // production pacing is exercised by the separate origin-boundary tests.
            rusqlite::Connection::open(&self.database)?.execute("UPDATE capture_origins SET next_fetch_at=0", [])?;
            let (media, body) = if url.as_str() == FEED_URL { ("application/rss+xml", self.rss.clone()) }
                else if url.host_str() == Some("i.ytimg.com") { ("image/png", include_bytes!("../../examples/fixtures/reading-image.png").to_vec()) }
                else { ("text/html", self.page.clone().unwrap_or_else(html).into_bytes()) };
            Ok(RawResponse { status: 200, headers: vec![("content-type".into(), media.into())], body })
        }
    }
    struct ForbiddenQuality;
    impl memex_analysis::capture_quality::CaptureQualityModel for ForbiddenQuality {
        fn descriptor(&self) -> memex_analysis::capture_quality::CaptureQualityDescriptor { panic!("quality route resolved") }
        fn check(&self, _: memex_analysis::capture_quality::CaptureQualityInput<'_>) -> std::result::Result<String, memex_analysis::capture_quality::CaptureQualityFailure> { panic!("quality model called") }
    }
    fn html() -> String {
        "<article><h1>Synthetic podcast</h1><p>This synthetic public transcript contains enough deterministic prose for the Readability translator to extract a stable document without any private data. Each speaker describes reproducible engineering examples and reviews a bounded experiment.</p><iframe src=\"https://www.youtube.com/embed/abcdefghijk\"></iframe></article>".into()
    }
    fn url(n: usize) -> String { format!("https://ampcode.com/podcast/season-2/episode-{n}") }
    fn feed() -> Vec<u8> {
        let items: String = (1..=17).map(|n| format!("<item><guid>episode-{n}</guid><link>{}</link><itunes:season>2</itunes:season><itunes:episode>{n}</itunes:episode><pubDate>Sun, 08 Nov 2026 23:30:00 -0800</pubDate><itunes:duration>{}</itunes:duration></item>", url(n), 600+n)).collect();
        format!("<rss xmlns:itunes=\"http://www.itunes.com/dtds/podcast-1.0.dtd\"><channel>{items}</channel></rss>").into_bytes()
    }
    fn config(root: &Path) -> AgentConfig {
        AgentConfig { library: root.join("library"), database: root.join("agent.db"), spool: root.join("spool"), socket: root.join("agent.sock"), legacy_translation: None, intake: None, metadata_lookup_contact: None, chrome_executable: None }
    }
    fn coordinator(cfg: &AgentConfig, calls: &Arc<AtomicUsize>) -> Coordinator<Network, Network> {
        let network = || Network { database: cfg.database.clone(), calls: calls.clone(), rss: feed(), page: None };
        Coordinator::new(cfg.clone(), network(), network()).unwrap().with_quality_model(Arc::new(ForbiddenQuality))
    }
    fn edit(library: &Path, op: memex_store::GroupEditOp) -> memex_store::GroupEditReceipt {
        memex_store::edit_groups(library, &format!("fixture-{}", uuid::Uuid::now_v7()), op, Sensitivity::Restricted).unwrap()
    }
    fn node(library: &Path, id: &str) -> memex_store::GroupNode {
        memex_store::with_consistent_snapshot(library, |root| Ok(memex_store::read_visible_groups(root, Sensitivity::Restricted)?.nodes.into_iter().find(|n| n.group_id == id).unwrap())).unwrap()
    }
    fn folder(library: &Path, parent: &str, name: &str) -> String {
        edit(library, memex_store::GroupEditOp::CreateFolder { parent_id: parent.into(), expected_parent_version: node(library, parent).version_token(), name: name.into() }).group_id
    }
    fn request(n: usize, key: usize, target: &str, sources: &[String]) -> UrlEnqueue {
        serde_json::from_value(serde_json::json!({"schema_version":1,"request_key":format!("chrome_capture_{key:032x}"),"url":url(n),"include_comments":false,"quality_check":"off","generation_policy":"none","target_group_id":target,"remove_from_group_ids":sources})).unwrap()
    }
    fn copy_tree(source: &Path, dest: &Path) {
        fs::create_dir_all(dest).unwrap();
        for entry in fs::read_dir(source).unwrap() {
            let entry = entry.unwrap(); let path = dest.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() { copy_tree(&entry.path(), &path); }
            else { fs::copy(entry.path(), path).unwrap(); }
        }
    }

    #[test]
    fn explicit_retry_recovers_legacy_negative_cover_without_recapturing() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path());
        let calls = Arc::new(AtomicUsize::new(0));
        let mut worker = coordinator(&cfg, &calls);
        let root = edit(&cfg.library, memex_store::GroupEditOp::CreateRoot { name: "Main".into(), sensitivity: Sensitivity::Internal }).group_id;
        let backend = crate::queue::ChromeIntake::new(cfg.database.clone(), cfg.library.clone()).unwrap();
        let owner = "a".repeat(64);
        let mut req = request(1, 1, &root, &[]);
        req.destination_mode = Some(memex_intake::url_jobs::DestinationMode::Inbox);
        req.target_group_id = None;
        let accepted = backend.execute(&owner, UrlJobCommand::Enqueue(req)).unwrap();
        let id = accepted["job_id"].as_str().unwrap();
        let job = worker.queue.lease(240).unwrap().unwrap();
        let page = "<nav><button>Log in</button><a href='https://youtu.be/PROMOTION01'>Promotion</a></nav><main><h1>Episode</h1><iframe src='https://www.youtube.com/embed/abcdefghijk'></iframe><div class=transcript><p>Frozen public transcript.</p></div></main>";
        let work = Work {
            rss: Some(Artifact::save(&cfg.spool, &feed(), "application/rss+xml").unwrap()),
            page: Some(Artifact::save(&cfg.spool, page.as_bytes(), "text/html").unwrap()),
            captured_at: Some("2026-11-09T00:00:00Z".into()), cover_checked:true,
            ..Work::default()
        };
        worker.queue.save_episode_work(&job, &work).unwrap();
        worker.queue.finish(&job, State::Failed, Stage::Commit, Some("podcast_retry"), 0).unwrap();
        drop(worker);
        // Advance only synthetic origin pacing; manual retry still uses the
        // original accepted identity and frozen RSS/page on a reopened worker.
        rusqlite::Connection::open(&cfg.database).unwrap().execute("UPDATE capture_origins SET next_fetch_at=0", []).unwrap();
        assert_eq!(backend.execute(&owner, UrlJobCommand::Retry(id.into())).unwrap()["job_id"], id);
        let mut worker = coordinator(&cfg, &calls);
        assert!(worker.process_one().unwrap());
        let receipt = worker.queue.chrome_receipt(&owner, id).unwrap();
        assert_eq!(receipt["state"], "succeeded");
        assert_eq!(receipt["enrichment"]["cover"], "stored");
        assert_eq!(calls.load(Ordering::SeqCst), 1); // Image only, never RSS/page/model.
        let after: Work = worker.queue.episode_work(id).unwrap().unwrap();
        assert_eq!(after.page.unwrap().digest, work.page.unwrap().digest);
        assert_eq!(after.rss.unwrap().digest, work.rss.unwrap().digest);
        assert!(!worker.process_one().unwrap());
    }

    #[test]
    fn episode_navigation_is_not_a_login_wall_and_real_walls_do_not_retry() {
        for wall in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let cfg = config(dir.path());
            let calls = Arc::new(AtomicUsize::new(0));
            let mut worker = coordinator(&cfg, &calls);
            let root = edit(&cfg.library, memex_store::GroupEditOp::CreateRoot { name: "Main".into(), sensitivity: Sensitivity::Internal }).group_id;
            let body = if wall { "<input type=password>Log in to continue" } else { "Public synthetic transcript with a complete first and last paragraph." };
            worker.transport.page = Some(format!("<nav><button>Log in</button><a href='https://youtu.be/PROMOTION01'>Promotion</a></nav><main><h1>Episode</h1><iframe src='https://www.youtube.com/embed/abcdefghijk'></iframe><div class=transcript><p>{body}</p></div></main>"));
            let backend = crate::queue::ChromeIntake::new(cfg.database.clone(), cfg.library.clone()).unwrap();
            let owner = "a".repeat(64);
            let make = || {
                let mut req = request(1, 1, &root, &[]);
                req.destination_mode = Some(memex_intake::url_jobs::DestinationMode::Inbox);
                req.target_group_id = None;
                req
            };
            let accepted = backend.execute(&owner, UrlJobCommand::Enqueue(make())).unwrap();
            assert!(worker.process_one().unwrap());
            let receipt = worker.queue.chrome_receipt(&owner, accepted["job_id"].as_str().unwrap()).unwrap();
            assert_eq!(receipt["state"], if wall { "needs_user" } else { "succeeded" }, "{receipt}");
            if wall {
                assert_eq!(receipt["reason"], "interactive_login_required");
                assert!(!worker.process_one().unwrap());
            } else {
                let item = receipt["item_id"].as_str().unwrap();
                let (_, revision) = memex_store::read_item(&cfg.library, item).unwrap();
                assert_eq!(revision.metadata.duration_seconds.unwrap().value, 601);
                assert!(revision.content_markdown.contains("Public synthetic transcript"));
                assert!(!revision.content_markdown.contains("Log in"));
                assert_eq!(receipt["enrichment"]["cover"], "stored");
            }
            assert_eq!(backend.execute(&owner, UrlJobCommand::Enqueue(make())).unwrap()["job_id"], accepted["job_id"]);
        }
    }

    #[test]
    fn accepted_request_reconciles_after_target_or_duplicate_changes() {
        for duplicate_change in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let cfg = config(dir.path());
            let calls = Arc::new(AtomicUsize::new(0));
            let _worker = coordinator(&cfg, &calls); // Do not run network/worker.
            let main = edit(&cfg.library, memex_store::GroupEditOp::CreateRoot {
                name: "Main".into(), sensitivity: Sensitivity::Internal,
            }).group_id;
            let target = folder(&cfg.library, &main, "AmpCode");
            let owner = "a".repeat(64);
            let backend = crate::queue::ChromeIntake::new(cfg.database.clone(), cfg.library.clone()).unwrap();
            let accepted = backend.execute(&owner, UrlJobCommand::Enqueue(request(1, 1, &target, &[]))).unwrap();
            // The caller loses the response. Before reconciliation, current
            // admission becomes invalid, without changing the stored request.
            if duplicate_change {
                for key in ["copy-one", "copy-two"] {
                    let bytes = html().into_bytes();
                    let digest = hex::encode(Sha256::digest(&bytes));
                    let prepared = prepare_capture(CaptureSubmission {
                        capture: web_capture(key, "2026-01-01T00:00:00Z", &url(1), "text/html", &digest, bytes),
                        metadata: ItemMetadata::default(), attachments: vec![],
                    }, AdmissionContext { http_status: Some(200), preview_approval: None }).unwrap();
                    commit_capture(&cfg.library, prepared).unwrap();
                }
            } else {
                let path = cfg.library.join("catalog/groups/state.json");
                let mut state: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                for node in state["nodes"].as_array_mut().unwrap() {
                    if node["group_id"] == target { node["read_only"] = true.into(); }
                }
                fs::write(path, serde_json::to_vec(&state).unwrap()).unwrap();
            }
            let reopened = crate::queue::ChromeIntake::new(cfg.database.clone(), cfg.library.clone()).unwrap();
            let replay = reopened.execute(&owner, UrlJobCommand::Enqueue(request(1, 1, &target, &[]))).unwrap();
            assert_eq!(replay["job_id"], accepted["job_id"]);
            let changed = reopened.execute(&owner, UrlJobCommand::Enqueue(request(2, 1, &target, &[]))).unwrap_err();
            assert_eq!(changed.code(), "request_identity_conflict");
            let foreign = reopened.execute(&"b".repeat(64), UrlJobCommand::Enqueue(request(1, 1, &target, &[]))).unwrap_err();
            assert_eq!(foreign.code(), "resource_not_found");
            let fresh = reopened.execute(&owner, UrlJobCommand::Enqueue(request(1, 2, &target, &[]))).unwrap_err();
            assert_eq!(fresh.code(), if duplicate_change { "podcast_duplicate_conflict" } else { "invalid_request" });
            let db = rusqlite::Connection::open(&cfg.database).unwrap();
            assert_eq!(db.query_row("SELECT count(*) FROM jobs", [], |r| r.get::<_,u64>(0)).unwrap(), 1);
            assert_eq!(db.query_row("SELECT count(*) FROM chrome_episode_requests", [], |r| r.get::<_,u64>(0)).unwrap(), 1);
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn explicit_destinations_preserve_memberships_and_replay() {
        use memex_intake::url_jobs::DestinationMode;
        for depth in 0..4 {
            let dir = tempfile::tempdir().unwrap();
            let cfg = config(dir.path());
            let calls = Arc::new(AtomicUsize::new(0));
            let mut worker = coordinator(&cfg, &calls);
            let main = edit(&cfg.library, memex_store::GroupEditOp::CreateRoot { name: "Main".into(), sensitivity: Sensitivity::Internal }).group_id;
            let child = folder(&cfg.library, &main, "Child");
            let nested = folder(&cfg.library, &child, "Nested");
            let target = if depth == 1 { &main } else { &nested };
            let owner = "a".repeat(64);
            let backend = crate::queue::ChromeIntake::new(cfg.database.clone(), cfg.library.clone()).unwrap();
            let make = |key| {
                let mut r = request(1, key, target, &[]);
                r.destination_mode = Some(if depth == 0 { DestinationMode::Inbox } else { DestinationMode::SelectedGroup });
                if depth == 0 { r.target_group_id = None; }
                if depth == 3 { r.url = "https://example.org/article".into(); }
                r
            };
            let accepted = backend.execute(&owner, UrlJobCommand::Enqueue(make(1))).unwrap();
            assert!(worker.process_one().unwrap());
            let id = accepted["job_id"].as_str().unwrap();
            let receipt = worker.queue.chrome_receipt(&owner, id).unwrap();
            assert_eq!(receipt["state"], "succeeded", "{receipt}");
            let item = receipt["item_id"].as_str().unwrap();
            let groups = memex_store::read_visible_groups(&cfg.library, Sensitivity::Restricted).unwrap();
            assert_eq!(groups.memberships.iter().filter(|m| m.item_id == item).count(), if depth == 0 { 0 } else { 1 });
            if depth != 0 { assert!(groups.memberships.iter().any(|m| m.item_id == item && m.group_id == *target)); }
            edit(&cfg.library, memex_store::GroupEditOp::AddMembers { group_id: child.clone(), expected_version: node(&cfg.library, &child).version_token(), item_ids: vec![item.into()] });
            drop(worker);
            let reopened = crate::queue::ChromeIntake::new(cfg.database.clone(), cfg.library.clone()).unwrap();
            let replay = reopened.execute(&owner, UrlJobCommand::Enqueue(make(1))).unwrap();
            assert_eq!(replay["job_id"], accepted["job_id"]);
            if depth != 3 {
                assert_eq!(reopened.execute(&owner, UrlJobCommand::Enqueue(make(2))).unwrap()["job_id"], accepted["job_id"]);
            }
            assert!(memex_store::read_visible_groups(&cfg.library, Sensitivity::Restricted).unwrap().memberships.iter().any(|m| m.item_id == item && m.group_id == child));
            assert_eq!(calls.load(Ordering::SeqCst), if depth == 3 { 1 } else { 3 });
        }
    }

    #[test]
    fn inbox_existing_preserves_location_and_ambiguous_copies_reject() {
        for copies in [1, 2] {
            let dir = tempfile::tempdir().unwrap();
            let cfg = config(dir.path());
            let calls = Arc::new(AtomicUsize::new(0));
            let mut worker = coordinator(&cfg, &calls);
            let root = edit(&cfg.library, memex_store::GroupEditOp::CreateRoot { name: "Filed".into(), sensitivity: Sensitivity::Internal }).group_id;
            let mut ids = vec![];
            for copy in 0..copies {
                let bytes = html().into_bytes();
                let digest = hex::encode(Sha256::digest(&bytes));
                let prepared = prepare_capture(CaptureSubmission {
                    capture: web_capture(&format!("existing-{copy}"), "2026-01-01T00:00:00Z", &url(1), "text/html", &digest, bytes),
                    metadata: ItemMetadata::default(), attachments: vec![],
                }, AdmissionContext { http_status: Some(200), preview_approval: None }).unwrap();
                let id = commit_capture(&cfg.library, prepared).unwrap().item_ids[0].clone();
                edit(&cfg.library, memex_store::GroupEditOp::AddMembers { group_id: root.clone(), expected_version: node(&cfg.library, &root).version_token(), item_ids: vec![id.clone()] });
                ids.push(id);
            }
            let before = memex_store::read_visible_groups(&cfg.library, Sensitivity::Restricted).unwrap().memberships;
            let mut req = request(1, 1, &root, &[]);
            req.target_group_id = None;
            req.destination_mode = Some(memex_intake::url_jobs::DestinationMode::Inbox);
            let backend = crate::queue::ChromeIntake::new(cfg.database.clone(), cfg.library.clone()).unwrap();
            let result = backend.execute(&"a".repeat(64), UrlJobCommand::Enqueue(req));
            if copies == 2 {
                assert_eq!(result.unwrap_err().code(), "podcast_duplicate_conflict");
                assert!(!worker.process_one().unwrap());
                assert_eq!(calls.load(Ordering::SeqCst), 0);
            } else {
                let job = result.unwrap()["job_id"].as_str().unwrap().to_owned();
                assert!(worker.process_one().unwrap());
                let receipt = worker.queue.chrome_receipt(&"a".repeat(64), &job).unwrap();
                assert_eq!(receipt["state"], "succeeded", "{receipt}");
                assert_eq!(receipt["item_id"], ids[0]);
                assert_eq!(receipt["enrichment"]["placement"], "existing_locations_preserved");
                assert_eq!(receipt["enrichment"]["archived"], false);
                assert_eq!(memex_store::list_items(&cfg.library).unwrap().len(), 1);
            }
            assert_eq!(memex_store::read_visible_groups(&cfg.library, Sensitivity::Restricted).unwrap().memberships, before);
        }
    }

    #[test]
    fn warm_admission_observes_new_duplicates_revisions_trash_and_restore() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path());
        let calls = Arc::new(AtomicUsize::new(0));
        let _worker = coordinator(&cfg, &calls);
        let backend = crate::queue::ChromeIntake::new(cfg.database.clone(), cfg.library.clone()).unwrap();
        let enqueue = |key: usize| {
            let mut req = request(1, key, "unused", &[]);
            req.target_group_id = None;
            req.destination_mode = Some(memex_intake::url_jobs::DestinationMode::Inbox);
            backend.execute(&"a".repeat(64), UrlJobCommand::Enqueue(req))
        };
        let first = enqueue(1).unwrap(); // Empty library, cached negative.
        let mut ids = vec![];
        for copy in 0..2 {
            let bytes = html().into_bytes();
            let digest = hex::encode(Sha256::digest(&bytes));
            let prepared = prepare_capture(CaptureSubmission {
                capture: web_capture(&format!("warm-{copy}"), "2026-01-01T00:00:00Z", &url(1), "text/html", &digest, bytes),
                metadata: ItemMetadata::default(), attachments: vec![],
            }, AdmissionContext { http_status: Some(200), preview_approval: None }).unwrap();
            let receipt = commit_capture(&cfg.library, prepared).unwrap();
            ids.push(receipt.item_ids[0].clone());
            if copy == 0 { assert_eq!(enqueue(2).unwrap()["job_id"], first["job_id"]); }
        }
        assert_eq!(enqueue(3).unwrap_err().code(), "podcast_duplicate_conflict");
        let (_, revision) = memex_store::read_item(&cfg.library, &ids[1]).unwrap();
        memex_store::trash_item(&cfg.library, &ids[1], &revision.revision_id, "duplicate", Some(&ids[0]), None).unwrap();
        assert_eq!(enqueue(4).unwrap()["job_id"], first["job_id"]);
        let (_, revision) = memex_store::read_item(&cfg.library, &ids[0]).unwrap();
        let revised = memex_store::revise_item_metadata(&cfg.library, &ids[0], memex_store::ItemMetadataUpdate {
            expected_revision_id: revision.revision_id, title: "Changed synthetic title".into(), manual_tags: vec![],
        }).unwrap();
        assert_eq!(enqueue(5).unwrap()["job_id"], first["job_id"]);
        memex_store::restore_item(&cfg.library, &ids[1]).unwrap();
        assert_eq!(enqueue(6).unwrap_err().code(), "podcast_duplicate_conflict");
        memex_store::trash_item(&cfg.library, &ids[1], &revision_id(&cfg.library, &ids[1]), "duplicate", Some(&ids[0]), None).unwrap();
        // A warmed match must not bypass canonical verification after corruption.
        let path = cfg.library.join("items").join(&ids[0]).join("revisions").join(format!("{}.json", revised.revision_id));
        let bytes = fs::read(&path).unwrap();
        fs::write(&path, b"{}").unwrap();
        assert!(enqueue(7).is_err());
        fs::write(&path, bytes).unwrap();
        assert_eq!(enqueue(7).unwrap()["job_id"], first["job_id"]);
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        fn revision_id(library: &Path, id: &str) -> String {
            memex_store::read_item(library, id).unwrap().1.revision_id
        }
    }

    #[test]
    fn seventeen_episode_capture_enrich_replay_restore_and_zero_generation() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path());
        let calls = Arc::new(AtomicUsize::new(0));
        let mut worker = coordinator(&cfg, &calls);
        let main = edit(&cfg.library, memex_store::GroupEditOp::CreateRoot { name: "Main".into(), sensitivity: Sensitivity::Internal }).group_id;
        let inbox = folder(&cfg.library, &main, "Inbox");
        let target = folder(&cfg.library, &main, "AmpCode");
        let decoy = folder(&cfg.library, &inbox, "AmpCode");
        let sources = vec![main.clone(), inbox.clone()];
        let mut old = std::collections::BTreeMap::new();
        for n in 14..=17 {
            let bytes = html().into_bytes(); let digest = hex::encode(Sha256::digest(&bytes));
            let prepared = prepare_capture(CaptureSubmission { capture: web_capture(&format!("existing-{n}"), "2026-01-01T00:00:00Z", &url(n), "text/html", &digest, bytes), metadata: ItemMetadata::default(), attachments: vec![] }, AdmissionContext { http_status: Some(200), preview_approval: None }).unwrap();
            let id = commit_capture(&cfg.library, prepared).unwrap().item_ids[0].clone();
            for group in [&main, &inbox, &decoy] {
                edit(&cfg.library, memex_store::GroupEditOp::AddMembers { group_id: group.clone(), expected_version: node(&cfg.library, group).version_token(), item_ids: vec![id.clone()] });
            }
            old.insert(n, (id.clone(), memex_store::read_item(&cfg.library, &id).unwrap().1));
        }
        // Seed a completed E5 translation before the no-generation batch starts.
        // This deterministic fixture is not the provider spy used by the worker.
        struct SeedTranslation;
        impl memex_analysis::TranslationModel for SeedTranslation {
            fn descriptor(&self) -> memex_analysis::TranslationModelDescriptor {
                crate::provider_generation::tests::fake_descriptor()
            }
            fn translate(&mut self, _: memex_analysis::TranslationModelInput<'_>) -> std::result::Result<String, memex_analysis::TranslationModelError> {
                Ok("这是已有的合成译文，补充播客来源信息不能重新生成或丢失它。".into())
            }
        }
        let (e5, e5_before) = &old[&15];
        memex_analysis::enqueue_item_translation(&cfg.library, e5, &e5_before.revision_id).unwrap();
        memex_analysis::TranslationQueue::open(&cfg.library).unwrap().process_next(&mut SeedTranslation).unwrap();
        let old_projection = memex_analysis::read_translation_projection(&cfg.library, e5, Sensitivity::Restricted, Some((&e5_before.revision_id, 0))).unwrap();
        assert_eq!(old_projection.state, "succeeded");
        let old_records = memex_analysis::list_records(&cfg.library).unwrap();
        assert_eq!(old_records.len(), 2);
        let mut marks = memex_store::read_annotations(&cfg.library, e5, &e5_before.revision_id, Sensitivity::Restricted).unwrap();
        let version = marks.version().unwrap();
        marks.underlines.push(memex_store::Underline { start: 0, end: 8 });
        memex_store::save_annotations(&cfg.library, &version, marks, Sensitivity::Restricted).unwrap();
        let mark_path = format!("catalog/reading-annotations/{e5}/{}.json", e5_before.revision_id);
        let mark_bytes = fs::read(cfg.library.join(&mark_path)).unwrap();
        // A second E7 is retained, never chosen or implicitly deleted by enrich.
        let bytes = html().into_bytes(); let digest = hex::encode(Sha256::digest(&bytes));
        let prepared = prepare_capture(CaptureSubmission { capture: web_capture("isolated-copy", "2026-01-01T00:00:00Z", &url(17), "text/html", &digest, bytes), metadata: ItemMetadata::default(), attachments: vec![] }, AdmissionContext { http_status: Some(200), preview_approval: None }).unwrap();
        let isolated = commit_capture(&cfg.library, prepared).unwrap().item_ids[0].clone();
        let owner = "a".repeat(64);
        let backend = crate::queue::ChromeIntake::new(cfg.database.clone(), cfg.library.clone()).unwrap();
        let mut jobs = vec![];
        for n in 1..=17 {
            let result = backend.execute(&owner, UrlJobCommand::Enqueue(request(n, n, &target, &sources))).unwrap();
            let duplicate = backend.execute(&owner, UrlJobCommand::Enqueue(request(n, n+100, &target, &sources))).unwrap();
            assert_eq!(result["job_id"], duplicate["job_id"]);
            jobs.push(result["job_id"].as_str().unwrap().to_owned());
        }
        // Fail after canonical capture. Recovery must not refetch or duplicate.
        let blocked = cfg.library.join("catalog/podcast-rss-sources");
        fs::write(&blocked, b"synthetic fault").unwrap();
        assert!(worker.process_one().unwrap());
        assert_eq!(worker.queue.get(&jobs[0]).unwrap().state, State::RetryWait);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        drop(worker); fs::remove_file(blocked).unwrap();
        worker = coordinator(&cfg, &calls);
        worker.queue.retry(&jobs[0]).unwrap();
        assert!(worker.process_one().unwrap());
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        for _ in 1..17 {
            // Advance synthetic time between jobs without changing production
            // pacing or retry semantics (covered by their own boundary tests).
            rusqlite::Connection::open(&cfg.database).unwrap().execute_batch(
                "UPDATE capture_origins SET next_fetch_at=0; UPDATE jobs SET next_attempt_at=0 WHERE state='queued';").unwrap();
            assert!(worker.process_one().unwrap());
        }
        assert_eq!(calls.load(Ordering::SeqCst), 51);
        for (i, job) in jobs.iter().enumerate() {
            let receipt = worker.queue.chrome_receipt(&owner, job).unwrap();
            assert_eq!(receipt["state"], "succeeded", "{receipt}");
            assert_eq!(receipt["enrichment"]["state"], "complete", "{receipt}");
            let item = receipt["item_id"].as_str().unwrap();
            let (_, revision) = memex_store::read_item(&cfg.library, item).unwrap();
            assert_eq!(revision.metadata.published_date.unwrap().value, "2026-11-08");
            assert_eq!(revision.metadata.duration_seconds.unwrap().value, 601+i as u32);
            if let Some((id, before)) = old.get(&(i+1)) {
                assert_eq!(item, id); assert_eq!(revision.content_markdown, before.content_markdown);
                assert_eq!(revision.evidence_ids, before.evidence_ids); assert_eq!(revision.attachments, before.attachments);
            }
            assert!(worker.queue.record(job).unwrap().translation_hook_completed);
            assert_eq!(backend.execute(&owner, UrlJobCommand::Enqueue(request(i+1, i+201, &target, &sources))).unwrap()["job_id"], *job);
        }
        assert_eq!(memex_store::list_items(&cfg.library).unwrap().len(), 18);
        assert!(!memex_store::is_item_trashed(&cfg.library, &isolated).unwrap());
        let groups = memex_store::with_consistent_snapshot(&cfg.library, |root| memex_store::read_visible_groups(root, Sensitivity::Restricted)).unwrap();
        assert_eq!(groups.memberships.iter().filter(|m| m.group_id == target).count(), 17);
        assert_eq!(groups.memberships.iter().filter(|m| m.group_id == decoy).count(), 4);
        assert!(!groups.memberships.iter().any(|m| sources.contains(&m.group_id)));
        assert!(worker.queue.pending_translation_intent().unwrap().is_none());
        drop(worker); drop(backend);
        let fake = crate::provider_generation::tests::FakeProvider::default();
        for _ in 0..3 {
            let mut provider = crate::provider_generation::ProviderWorker::new(Box::new(fake.clone()));
            assert!(!provider.step(&cfg.library, Duration::ZERO).unwrap());
            assert!(Queue::open(&cfg.database).unwrap().pending_translation_intent().unwrap().is_none());
        }
        assert!(fake.calls.lock().unwrap().is_empty());
        let backup = tempfile::tempdir().unwrap(); copy_tree(&cfg.library, &backup.path().join("library"));
        let restored = backup.path().join("library");
        for root in [&cfg.library, &restored] {
            let (_, current) = memex_store::read_item(root, e5).unwrap();
            assert_ne!(current.revision_id, e5_before.revision_id);
            let projection = memex_analysis::read_translation_projection(root, e5, Sensitivity::Restricted, Some((&current.revision_id, 0))).unwrap();
            assert_eq!(projection.state, "succeeded");
            assert_eq!(projection.page.as_ref().unwrap().content_markdown, old_projection.page.as_ref().unwrap().content_markdown);
            assert_eq!(projection.page.as_ref().unwrap().content_sha256, old_projection.page.as_ref().unwrap().content_sha256);
            assert_eq!(fs::read(root.join(&mark_path)).unwrap(), mark_bytes);
            let records = memex_analysis::list_records(root).unwrap();
            for original in &old_records {
                assert!(records.iter().any(|record| serde_json::to_value(record).unwrap() == serde_json::to_value(original).unwrap()));
            }
        }
        for (item, _) in memex_store::list_items(&restored).unwrap() {
            let cover = memex_store::covers::cover_batch(&restored, &[item.item_id.clone()], Sensitivity::Restricted).unwrap().remove(0);
            if let Some(cover) = cover {
                for thumb in [false, true] {
                    let mut offset = 0; let mut bytes = vec![];
                    loop {
                        let chunk = memex_store::covers::cover_chunk(&restored, &item.item_id, &cover.version, thumb, offset, Sensitivity::Restricted).unwrap();
                        bytes.extend_from_slice(&chunk.bytes);
                        match chunk.next_offset { Some(next) => offset = next, None => break }
                    }
                    assert_eq!(hex::encode(Sha256::digest(&bytes)), if thumb { &cover.thumbnail.sha256 } else { &cover.raster.sha256 }.to_owned());
                }
            }
        }
    }
}
