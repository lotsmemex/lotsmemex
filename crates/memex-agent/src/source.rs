//! Compile-time source extension point. A build that links extra source
//! handlers passes them to `ipc::run_daemon_with_sources` (or
//! `Coordinator::with_source_handler`); the default daemon registers none.
//!
//! A handler claims a leased URL job that the owner did not route through
//! Chrome and drives it to a terminal or retry state through `SourceJob`,
//! which exposes only lease-checked queue, spool, fetch and Store operations.
use crate::{
    config::AgentConfig,
    coordinator::{install, read_exact_path, read_verified, SourceIndex},
    fetch::{FetchFailure, Fetched, HttpTransport, Resolver},
    queue::{JobRecord, Queue, Stage, State},
};
use anyhow::{Context, Result};
use memex_capture::{commit_capture, PreparedCapture};
use memex_core::IngestReceipt;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

pub use crate::coordinator::MAX_AUTOMATIC_ATTEMPTS;
pub use crate::coordinator::{admission_class_text, web_capture, SourceIndexNotReady};

pub trait SourceHandler: Send + Sync {
    /// Whether this handler processes a leased URL job without Chrome options.
    fn claims(&self, url: &str) -> bool;
    /// Image URLs this source may fetch with `SourceJob::fetch_exact_image`.
    /// The production transport also uses it to request an image response.
    fn exact_image_url(&self, _url: &str) -> bool {
        false
    }
    /// Runs one job. Business outcomes are recorded with `finish` or
    /// `finish_stored`; an `Err` restarts the capture worker.
    fn process(&self, job: &mut SourceJob<'_>) -> Result<()>;
}

pub struct SourceJob<'a> {
    pub(crate) queue: &'a mut Queue,
    pub(crate) cfg: &'a AgentConfig,
    pub(crate) resolver: &'a dyn Resolver,
    pub(crate) transport: &'a dyn HttpTransport,
    pub(crate) source_index: &'a Arc<Mutex<SourceIndex>>,
    pub(crate) handler: &'a dyn SourceHandler,
    pub(crate) renderer: &'a dyn crate::render::PageRenderer,
    pub(crate) record: JobRecord,
}

/// One page opened in the local Chrome; same shape as `FrozenPage`.
pub struct RenderedPage {
    pub bytes: Vec<u8>,
    pub media_type: String,
    pub captured_at: String,
    pub final_url: String,
}

/// The frozen page of a job whose fetch already completed before a restart.
pub struct FrozenPage {
    pub bytes: Vec<u8>,
    pub media_type: String,
    pub captured_at: String,
    pub final_url: Option<String>,
}

impl SourceJob<'_> {
    pub fn job_id(&self) -> &str {
        &self.record.view.job_id
    }
    pub fn url(&self) -> &str {
        &self.record.url
    }
    pub fn attempt_count(&self) -> u32 {
        self.record.view.attempt_count
    }
    pub fn external_capture_id(&self) -> Option<&str> {
        self.record.external_capture_id.as_deref()
    }
    pub fn library(&self) -> &Path {
        &self.cfg.library
    }
    pub fn spool(&self) -> &Path {
        &self.cfg.spool
    }
    pub fn renew(&mut self) -> Result<()> {
        self.queue.renew_episode(&self.record)
    }
    pub fn stage(&mut self, stage: Stage) -> Result<()> {
        self.queue.stage(&self.record, stage)
    }
    pub fn finish(&mut self, state: State, stage: Stage, reason: &str, delay: i64) -> Result<()> {
        self.queue
            .finish(&self.record, state, stage, Some(reason), delay)
    }
    pub fn finish_stored(&mut self, item_id: &str, revision_id: &str) -> Result<()> {
        self.queue.finish_stored(&self.record, item_id, revision_id)
    }
    /// The job's restartable work journal, keyed by job identity.
    pub fn work<T: DeserializeOwned>(&self) -> Result<Option<T>> {
        self.queue.episode_work(&self.record.view.job_id)
    }
    pub fn save_work(&mut self, value: &impl Serialize) -> Result<()> {
        self.queue.save_episode_work(&self.record, value)
    }
    pub fn frozen_page(&self) -> Result<Option<FrozenPage>> {
        if self.record.artifact_digest.is_none() {
            return Ok(None);
        }
        Ok(Some(FrozenPage {
            bytes: read_verified(&self.cfg.spool, &self.record)?,
            media_type: self
                .record
                .artifact_media
                .clone()
                .context("artifact_mismatch")?,
            captured_at: self
                .record
                .captured_at
                .clone()
                .context("artifact_mismatch")?,
            final_url: self.record.final_url.clone(),
        }))
    }
    pub fn fetch_page(&self, url: &str) -> std::result::Result<Fetched, FetchFailure> {
        crate::fetch::fetch(url, self.resolver, self.transport)
    }
    /// Opens `url` in the local Chrome and snapshots its main document; see
    /// `crate::render` for the network and process policy. A changed final
    /// URL fails with `page_changed`; a verification page is returned as is.
    pub fn render_page(&self, url: &str) -> std::result::Result<RenderedPage, FetchFailure> {
        let fail = |code: &'static str| FetchFailure::from(anyhow::anyhow!(code));
        let parsed = url::Url::parse(url).map_err(|_| fail("url_malformed"))?;
        if parsed.scheme() != "https"
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.fragment().is_some()
        {
            return Err(fail("url_policy_rejected"));
        }
        if parsed.port().is_some_and(|port| port != 443) {
            return Err(fail("url_port_forbidden"));
        }
        let host = parsed.host_str().ok_or_else(|| fail("host_missing"))?;
        let addresses = self
            .resolver
            .resolve(host, std::time::Duration::from_secs(5))
            .map_err(FetchFailure::from)?;
        if addresses.is_empty()
            || addresses.len() > crate::fetch::MAX_RESOLVED_ADDRESSES
            || addresses.iter().any(|a| !crate::fetch::is_global(*a))
        {
            return Err(fail("address_not_public"));
        }
        let captured_at = chrono::Utc::now().to_rfc3339();
        let deadline = std::time::Instant::now() + crate::render::RENDER_DEADLINE;
        let document = self
            .renderer
            .render(&parsed, self.resolver, deadline)
            .map_err(FetchFailure::from)?;
        if document.final_url != parsed.as_str() {
            return Err(fail("page_changed"));
        }
        Ok(RenderedPage {
            bytes: document.html,
            media_type: "text/html".into(),
            captured_at,
            final_url: document.final_url,
        })
    }
    /// Fetches one image the handler's `exact_image_url` admits. Redirects
    /// cannot change its identity; no cookies or Referer are sent.
    pub fn fetch_exact_image(&self, url: &str) -> std::result::Result<Fetched, FetchFailure> {
        if !self.handler.exact_image_url(url) {
            return Err(FetchFailure::from(anyhow::anyhow!("url_policy_rejected")));
        }
        crate::fetch::fetch_exact_image(url, self.resolver, self.transport)
    }
    /// Library items whose source or canonical URL is exactly `url`.
    pub fn existing_items(&self, url: &str) -> Result<Vec<String>> {
        memex_store::with_consistent_snapshot(&self.cfg.library, |root| {
            self.source_index
                .lock()
                .map_err(|_| anyhow::anyhow!("source_index_unavailable"))?
                .matching(root, url)
        })
    }
    /// Commits `prepared` under the queue's lease-checked commit transaction.
    /// With `existing_error`, an item already holding the job URL fails the
    /// commit with that error instead. Index contention yields up to three
    /// times, then fails with `SourceIndexNotReady`.
    pub fn commit_new_capture(
        &mut self,
        prepared: PreparedCapture,
        existing_error: Option<&'static str>,
    ) -> Result<IngestReceipt> {
        let library = self.cfg.library.clone();
        let index = self.source_index.clone();
        let url = self.record.url.clone();
        let mut prepared = Some(prepared);
        for _ in 0..3 {
            self.queue.renew_episode(&self.record)?;
            // Wait/refresh only outside the queue transaction; renew again if a
            // competing index builder took time. An expired/cancelled lease stops.
            memex_store::with_consistent_snapshot(&library, |root| {
                index
                    .lock()
                    .map_err(|_| anyhow::anyhow!("source_index_unavailable"))?
                    .refresh(root)
            })?;
            self.queue.renew_episode(&self.record)?;
            #[cfg(test)]
            hooks::commit_step(hooks::CommitStep::Warmed);
            let result = self.queue.episode_commit(&self.record, || {
                if let Some(existing_error) = existing_error {
                    let matches = memex_store::with_consistent_snapshot(&library, |root| {
                        #[cfg(test)]
                        hooks::commit_step(hooks::CommitStep::Ready);
                        let mut index = match index.try_lock() {
                            Ok(index) => index,
                            Err(std::sync::TryLockError::WouldBlock) => {
                                return Err(SourceIndexNotReady.into());
                            }
                            Err(_) => anyhow::bail!("source_index_unavailable"),
                        };
                        index.matching_ready(root, &url)
                    })?;
                    anyhow::ensure!(matches.is_empty(), existing_error);
                }
                commit_capture(
                    &library,
                    prepared.take().expect("capture is committed at most once"),
                )
            });
            match result {
                Ok(receipt) => return Ok(receipt),
                Err(error) if error.is::<SourceIndexNotReady>() => {
                    // episode_commit has already rolled back its IMMEDIATE tx.
                    #[cfg(test)]
                    hooks::commit_step(hooks::CommitStep::Yielded);
                }
                Err(error) => return Err(error),
            }
        }
        Err(SourceIndexNotReady.into())
    }
}

/// A content-addressed file in the agent spool. Its serialized form is part
/// of persisted work journals.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpoolArtifact {
    pub digest: String,
    pub size: u64,
    pub media: String,
}
impl SpoolArtifact {
    pub fn save(spool: &Path, bytes: &[u8], media: &str) -> Result<Self> {
        let digest = hex::encode(Sha256::digest(bytes));
        install(spool, &digest, bytes)?;
        Ok(Self {
            digest,
            size: bytes.len() as u64,
            media: media.into(),
        })
    }
    pub fn read(&self, spool: &Path) -> Result<Vec<u8>> {
        anyhow::ensure!(
            self.digest.len() == 64
                && self
                    .digest
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            "artifact_mismatch"
        );
        read_exact_path(
            &spool.join("cas").join(&self.digest[..2]).join(&self.digest),
            &self.digest,
            self.size,
        )
    }
}

#[cfg(test)]
pub(crate) mod hooks {
    use std::cell::RefCell;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum CommitStep {
        Warmed,
        Ready,
        Yielded,
    }
    thread_local! {
        pub(crate) static COMMIT_HOOK: RefCell<Option<Box<dyn FnMut(CommitStep)>>> = RefCell::new(None);
    }
    pub(crate) fn commit_step(step: CommitStep) {
        COMMIT_HOOK.with(|hook| {
            if let Some(hook) = hook.borrow_mut().as_mut() {
                hook(step);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{coordinator::Coordinator, fetch::RawResponse};
    use memex_capture::{prepare_capture, AdmissionContext, CaptureSubmission};
    use std::{
        net::{IpAddr, SocketAddr},
        sync::mpsc,
        time::Duration,
    };

    const URL: &str = "https://source.example/article";
    const PAGE: &str = "<html><head><title>Synthetic interview</title></head><body><article>\
      <h1>Synthetic interview</h1><p>Question: Why preserve evidence when a source page may change \
      later?</p><p>Answer: It makes later verification possible, because the stored copy keeps the \
      exact words that were read at capture time.</p></article></body></html>";

    /// A minimal registered source: one page, committed as a new item.
    struct Article;
    impl SourceHandler for Article {
        fn claims(&self, url: &str) -> bool {
            url == URL
        }
        fn process(&self, job: &mut SourceJob<'_>) -> Result<()> {
            let result = (|| -> Result<()> {
                job.renew()?;
                let page = job.fetch_page(URL)?;
                let digest = hex::encode(Sha256::digest(&page.body));
                let capture = web_capture(
                    job.job_id(),
                    "2026-01-01T00:00:00Z",
                    URL,
                    &page.media_type,
                    &digest,
                    page.body,
                );
                let prepared = prepare_capture(
                    CaptureSubmission {
                        capture,
                        metadata: Default::default(),
                        attachments: vec![],
                    },
                    AdmissionContext {
                        http_status: Some(200),
                        preview_approval: None,
                    },
                )?;
                job.stage(Stage::Commit)?;
                let receipt = job.commit_new_capture(prepared, Some("existing_item"))?;
                job.finish_stored(&receipt.item_ids[0], &receipt.revision_ids[0])
            })();
            match result {
                Err(error) if error.is::<SourceIndexNotReady>() => {
                    job.finish(State::RetryWait, Stage::Commit, "polite_wait", 1)
                }
                other => other,
            }
        }
    }

    #[derive(Clone, Default)]
    struct Site {
        requests: Arc<Mutex<Vec<String>>>,
    }
    impl Resolver for Site {
        fn resolve(&self, _: &str, _: Duration) -> Result<Vec<IpAddr>> {
            Ok(vec!["93.184.216.34".parse()?])
        }
    }
    impl HttpTransport for Site {
        fn get(&self, url: &url::Url, _: &[SocketAddr], _: Duration) -> Result<RawResponse> {
            self.requests.lock().unwrap().push(url.to_string());
            anyhow::ensure!(url.as_str() == URL, "unexpected_url");
            Ok(RawResponse {
                status: 200,
                headers: vec![("content-type".into(), "text/html".into())],
                body: PAGE.as_bytes().to_vec(),
            })
        }
    }
    fn registered(cfg: AgentConfig, site: Site) -> Coordinator<Site, Site> {
        Coordinator::new(cfg, site.clone(), site)
            .unwrap()
            .with_source_handler(Arc::new(Article))
    }
    fn cfg(root: &Path) -> AgentConfig {
        AgentConfig {
            library: root.join("library"),
            spool: root.join("spool"),
            database: root.join("agent.db"),
            socket: root.join("agent.sock"),
            legacy_translation: None,
            intake: None,
            metadata_lookup_contact: None,
            chrome_executable: None,
        }
    }

    fn exercise_index_race(busy: bool) {
        let temp = tempfile::tempdir().unwrap();
        let cfg = cfg(&temp.path().canonicalize().unwrap());
        let site = Site::default();
        let mut worker = registered(cfg.clone(), site.clone());
        let job = worker
            .queue
            .enqueue("local_cli", "index-race", URL)
            .unwrap();
        let index = worker.source_index.clone();
        let mut other = Queue::open(&cfg.database).unwrap();
        let (events, observed) = mpsc::channel();
        let (resume, pauses) = mpsc::channel();
        let worker_id = job.job_id.clone();
        let running = std::thread::spawn(move || {
            let mut seen = Vec::new();
            hooks::COMMIT_HOOK.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move |step| {
                    if !seen.contains(&step) {
                        seen.push(step);
                        events.send(step).unwrap();
                        pauses.recv().unwrap();
                    }
                }))
            });
            let result = worker.process_one();
            let state = worker.queue.get(&worker_id).unwrap();
            (result, state)
        });
        assert_eq!(
            observed.recv_timeout(Duration::from_secs(10)).unwrap(),
            hooks::CommitStep::Warmed
        );
        let held = busy.then(|| index.lock().unwrap());
        if !busy {
            // A real Store writer invalidates the negative cached match after
            // warm-up, without changing any source in this synthetic library.
            let before = memex_store::catalog_generation(&cfg.library).unwrap();
            assert!(memex_store::lookup_ingest_receipt(
                &cfg.library,
                "synthetic",
                "unrelated-writer",
                None,
            )
            .unwrap()
            .is_none());
            assert_ne!(
                before,
                memex_store::catalog_generation(&cfg.library).unwrap()
            );
        }
        resume.send(()).unwrap();
        assert_eq!(
            observed.recv_timeout(Duration::from_secs(10)).unwrap(),
            hooks::CommitStep::Ready
        );
        let (written, writes) = mpsc::channel();
        let cancel_id = job.job_id.clone();
        let writer = std::thread::spawn(move || {
            let result =
                other.enqueue("local_cli", "other-writer", "https://example.invalid/other");
            if result.is_ok() {
                // A yielded transaction does not surrender the job's lease;
                // the existing API still rejects cancellation of leased jobs.
                assert_eq!(
                    other.cancel(&cancel_id).unwrap_err().to_string(),
                    "job_not_cancellable"
                );
            }
            written.send(result.is_ok()).unwrap();
        });
        resume.send(()).unwrap();
        let yielded =
            observed.recv_timeout(Duration::from_secs(2)) == Ok(hooks::CommitStep::Yielded);
        let writable_before_release = writes.recv_timeout(Duration::from_millis(500)) == Ok(true);
        // Always release/join before asserting, including the intentionally red
        // blocking implementation. A failed regression must not leave a thread.
        drop(held);
        if yielded {
            resume.send(()).unwrap();
        }
        let (result, done) = running.join().unwrap();
        writer.join().unwrap();
        assert!(
            yielded,
            "commit did not yield while the index was unavailable"
        );
        assert!(
            writable_before_release,
            "another Queue could not write during the yield"
        );
        result.unwrap();
        assert_eq!(done.state, State::Succeeded);
        assert_eq!(done.job_id, job.job_id);
        assert_eq!(done.attempt_count, 1);
        assert_eq!(memex_store::list_items(&cfg.library).unwrap().len(), 1);
        assert_eq!(site.requests.lock().unwrap().len(), 1);
    }

    #[test]
    fn busy_index_yields_queue_before_waiting_and_completes_same_job() {
        exercise_index_race(true);
    }

    #[test]
    fn stale_index_refreshes_outside_queue_and_completes_same_job() {
        exercise_index_race(false);
    }

    #[test]
    fn index_contention_preserves_last_business_attempt_and_resume_or_cancel() {
        for cancel in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let cfg = cfg(&temp.path().canonicalize().unwrap());
            let site = Site::default();
            let mut worker = registered(cfg.clone(), site.clone());
            let job = worker
                .queue
                .enqueue("local_cli", "last-business-attempt", URL)
                .unwrap();
            // Synthetic setup: four previous business failures, with the final
            // allowed attempt due. Only scheduling contention is injected next.
            rusqlite::Connection::open(&cfg.database)
                .unwrap()
                .execute(
                    "UPDATE jobs SET state='retry_wait',attempt_count=4 WHERE job_id=?1",
                    [&job.job_id],
                )
                .unwrap();
            let index = worker.source_index.clone();
            let checks = Arc::new(Mutex::new(0));
            let count = checks.clone();
            hooks::COMMIT_HOOK.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move |step| {
                    if step == hooks::CommitStep::Warmed {
                        *index.lock().unwrap() = crate::coordinator::SourceIndex::default();
                        *count.lock().unwrap() += 1;
                    }
                }))
            });
            let result = worker.process_one();
            hooks::COMMIT_HOOK.with(|hook| hook.borrow_mut().take());
            result.unwrap();
            assert_eq!(*checks.lock().unwrap(), 3);
            let waiting = worker.queue.get(&job.job_id).unwrap();
            assert_eq!(waiting.state, State::RetryWait);
            assert_eq!(waiting.reason_code.as_deref(), Some("polite_wait"));
            assert_eq!(waiting.attempt_count, 4);
            assert!(memex_store::list_items(&cfg.library).unwrap().is_empty());
            assert_eq!(site.requests.lock().unwrap().len(), 1);
            if cancel {
                worker.queue.cancel(&job.job_id).unwrap();
            } else {
                worker.queue.make_retry_due(&job.job_id).unwrap();
            }
            drop(worker);
            let mut worker = registered(cfg.clone(), site.clone());
            assert_eq!(worker.process_one().unwrap(), !cancel);
            let done = worker.queue.get(&job.job_id).unwrap();
            assert_eq!(done.job_id, job.job_id);
            assert_eq!(
                done.state,
                if cancel {
                    State::Cancelled
                } else {
                    State::Succeeded
                }
            );
            assert_eq!(done.attempt_count, if cancel { 4 } else { 5 });
            // This synthetic source keeps no work journal, so a resumed job refetches.
            assert_eq!(
                site.requests.lock().unwrap().len(),
                if cancel { 1 } else { 2 }
            );
            assert_eq!(
                memex_store::list_items(&cfg.library).unwrap().len(),
                usize::from(!cancel)
            );
        }
    }

    /// Records what it was asked to load and answers with a fixed final URL.
    struct Rendered {
        final_url: &'static str,
        calls: Arc<Mutex<Vec<String>>>,
    }
    impl crate::render::PageRenderer for Rendered {
        fn render(
            &self,
            url: &url::Url,
            _: &dyn Resolver,
            _: std::time::Instant,
        ) -> Result<crate::render::RenderedDocument> {
            self.calls.lock().unwrap().push(url.to_string());
            Ok(crate::render::RenderedDocument {
                html: PAGE.as_bytes().to_vec(),
                final_url: self.final_url.into(),
            })
        }
    }

    /// Renders its target URL and records the outcome as the job's reason.
    struct Renders(&'static str);
    impl SourceHandler for Renders {
        fn claims(&self, _: &str) -> bool {
            true
        }
        fn process(&self, job: &mut SourceJob<'_>) -> Result<()> {
            let url = self.0;
            match job.render_page(url) {
                Ok(page) => {
                    assert_eq!(page.media_type, "text/html");
                    assert_eq!(page.final_url, url);
                    assert_eq!(page.bytes, PAGE.as_bytes());
                    job.finish(State::Failed, Stage::Fetch, "unsupported_content", 0)
                }
                Err(error) => job.finish(State::Failed, Stage::Fetch, error.reason_code(), 0),
            }
        }
    }

    #[derive(Clone)]
    struct Addresses(&'static str);
    impl Resolver for Addresses {
        fn resolve(&self, _: &str, _: Duration) -> Result<Vec<IpAddr>> {
            Ok(vec![self.0.parse()?])
        }
    }
    impl HttpTransport for Addresses {
        fn get(&self, _: &url::Url, _: &[SocketAddr], _: Duration) -> Result<RawResponse> {
            anyhow::bail!("render_page must not use the HTTP transport")
        }
    }

    #[test]
    fn render_page_checks_scheme_address_and_final_url_around_the_renderer() {
        for (url, address, final_url, reason, rendered) in [
            (URL, "93.184.216.34", URL, "unsupported_content", true),
            (
                URL,
                "93.184.216.34",
                "https://source.example/login",
                "page_changed",
                true,
            ),
            (
                "http://source.example/article",
                "93.184.216.34",
                URL,
                "network_policy_rejected",
                false,
            ),
            (
                "https://source.example:8443/article",
                "93.184.216.34",
                URL,
                "network_policy_rejected",
                false,
            ),
            (URL, "10.1.2.3", URL, "address_not_public", false),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let cfg = cfg(&temp.path().canonicalize().unwrap());
            let calls = Arc::new(Mutex::new(Vec::new()));
            let mut worker = Coordinator::new(cfg, Addresses(address), Addresses(address))
                .unwrap()
                .with_page_renderer(Arc::new(Rendered {
                    final_url,
                    calls: calls.clone(),
                }))
                .with_source_handler(Arc::new(Renders(url)));
            let job = worker.queue.enqueue("local_cli", "render", URL).unwrap();
            assert!(worker.process_one().unwrap());
            let done = worker.queue.get(&job.job_id).unwrap();
            assert_eq!(
                done.reason_code.as_deref(),
                Some(reason),
                "{url} {address} {final_url}"
            );
            assert_eq!(calls.lock().unwrap().len(), usize::from(rendered), "{url}");
        }
    }

    #[test]
    fn spool_artifact_round_trips_an_image_larger_than_a_page() {
        let temp = tempfile::tempdir().unwrap();
        let spool = temp.path().canonicalize().unwrap();
        let mut gif = b"GIF89a".to_vec();
        gif.resize(crate::fetch::MAX_BODY + 1024 * 1024, 7);
        let artifact = SpoolArtifact::save(&spool, &gif, "image/gif").unwrap();
        assert_eq!(artifact.read(&spool).unwrap(), gif);
    }
}
