use crate::{
    config::{ensure_owner_directory, AgentConfig},
    fetch::{fetch, fetch_scheduled, FetchDisposition, HttpTransport, OriginDeferred, Resolver},
    queue::{JobRecord, Queue, Stage, State},
    url_input::extract_url,
};

pub const MAX_AUTOMATIC_ATTEMPTS: u32 = 5;
mod podcast;
mod source_index;
pub(crate) use podcast::existing as existing_podcast;
pub(crate) use source_index::SourceIndex;
pub use source_index::SourceIndexNotReady;
use anyhow::{bail, Context, Result};
use memex_capture::{
    admission_decision, commit_capture, prepare_capture, AdmissionClass, AdmissionContext,
    CaptureSubmission, PreviewApproval,
};
use memex_core::{
    BlobDescriptor, CaptureEnvelope, ItemMetadata, Sensitivity, SourceProfile, VerifiedBlob,
    VerifiedCapture,
};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::{
        fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        io::AsRawFd,
    },
    path::{Path, PathBuf},
};

pub struct Coordinator<R, T> {
    pub queue: Queue,
    cfg: AgentConfig,
    resolver: R,
    transport: T,
    pub(crate) source_index: std::sync::Arc<std::sync::Mutex<SourceIndex>>,
    quality_model:
        Option<std::sync::Arc<dyn memex_analysis::capture_quality::CaptureQualityModel>>,
    sources: Vec<std::sync::Arc<dyn crate::source::SourceHandler>>,
    renderer: std::sync::Arc<dyn crate::render::PageRenderer>,
}
impl<R: Resolver, T: HttpTransport> Coordinator<R, T> {
    pub fn new(cfg: AgentConfig, resolver: R, transport: T) -> Result<Self> {
        cfg.validate()?;
        let renderer = std::sync::Arc::new(crate::render::ChromeRenderer::new(
            cfg.chrome_executable.clone(),
        ));
        for p in [&cfg.library, &cfg.spool] {
            ensure_owner_directory(p)?;
        }
        memex_store::recover_pending_ingests(&cfg.library)?;
        Ok(Self {
            queue: Queue::open(&cfg.database)?,
            cfg,
            resolver,
            transport,
            source_index: Default::default(),
            quality_model: None,
            sources: Vec::new(),
            renderer,
        })
    }
    /// Replaces the page renderer used by `SourceJob::render_page`.
    pub fn with_page_renderer(
        mut self,
        renderer: std::sync::Arc<dyn crate::render::PageRenderer>,
    ) -> Self {
        self.renderer = renderer;
        self
    }
    /// Registers a compile-time source handler; earlier registrations win.
    pub fn with_source_handler(
        mut self,
        handler: std::sync::Arc<dyn crate::source::SourceHandler>,
    ) -> Self {
        self.sources.push(handler);
        self
    }
    pub fn with_quality_model(
        mut self,
        model: std::sync::Arc<dyn memex_analysis::capture_quality::CaptureQualityModel>,
    ) -> Self {
        self.quality_model = Some(model);
        self
    }
    pub fn process_one(&mut self) -> Result<bool> {
        let Some(mut j) = self.queue.lease(60)? else {
            return Ok(false);
        };
        if self.queue.chrome_options(&j.view.job_id)?.is_none() {
            if let Some(handler) = self.sources.iter().find(|h| h.claims(&j.url)).cloned() {
                let mut job = crate::source::SourceJob {
                    queue: &mut self.queue,
                    cfg: &self.cfg,
                    resolver: &self.resolver,
                    transport: &self.transport,
                    source_index: &self.source_index,
                    handler: handler.as_ref(),
                    renderer: self.renderer.as_ref(),
                    record: j,
                };
                handler.process(&mut job)?;
                return Ok(true);
            }
        }
        if self.queue.chrome_options(&j.view.job_id)?.is_some_and(|o| o.is_podcast(&j.url)) {
            return self.process_podcast(j);
        }
        if j.artifact_digest.is_none() {
            self.queue.stage(&j, Stage::Resolve)?;
            self.queue.stage(&j, Stage::Fetch)?;
            let response = if self.queue.chrome_options(&j.view.job_id)?.is_some() {
                fetch_scheduled(&j.url, &self.resolver, &self.transport, &mut |url, hops| {
                    let origin = url.origin().ascii_serialization();
                    let started = std::time::Instant::now();
                    loop {
                        let now = chrono::Utc::now().timestamp();
                        let Some(until) =
                            self.queue
                                .reserve_capture_hop(&j, &origin, hops == 0, now)?
                        else {
                            return Ok(());
                        };
                        let wait = std::time::Duration::from_secs(until.saturating_sub(now) as u64);
                        if started.elapsed() + wait > std::time::Duration::from_secs(25) {
                            return Err(OriginDeferred {
                                origin,
                                not_before: until,
                            }
                            .into());
                        }
                        std::thread::sleep(wait);
                    }
                })
            } else {
                fetch(&j.url, &self.resolver, &self.transport)
            };
            let fetched = match response {
                Ok(x) => x,
                Err(error) => {
                    match error.disposition() {
                        FetchDisposition::FatalWorker => bail!("fetch_worker_failed"),
                        FetchDisposition::InteractiveLogin => self.queue.finish(
                            &j,
                            State::NeedsUser,
                            Stage::Fetch,
                            Some("interactive_login_required"),
                            0,
                        )?,
                        FetchDisposition::Unsupported => self.queue.finish(
                            &j,
                            State::Failed,
                            Stage::Validate,
                            Some(error.reason_code()),
                            0,
                        )?,
                        FetchDisposition::Retryable => {
                            let scheduled_wait = error.reason_code() == "polite_wait";
                            let exhausted =
                                !scheduled_wait && j.view.attempt_count >= MAX_AUTOMATIC_ATTEMPTS;
                            let exponent = j.view.attempt_count.saturating_sub(1).min(5);
                            let now = chrono::Utc::now().timestamp();
                            let retry_at =
                                error.retry_after().map(|(_, at)| at).unwrap_or(now).max(
                                    now.saturating_add(if scheduled_wait {
                                        1
                                    } else {
                                        30_i64 << exponent
                                    }),
                                );
                            if let Some((origin, _)) = error.retry_after() {
                                if !scheduled_wait
                                    && self.queue.chrome_options(&j.view.job_id)?.is_some()
                                {
                                    // Only server backoff is shared. A competing
                                    // local lease must not synchronize both jobs
                                    // into an endlessly renewed shared delay.
                                    self.queue.defer_capture_origin(origin, retry_at)?;
                                    let initial_origin =
                                        url::Url::parse(&j.url)?.origin().ascii_serialization();
                                    self.queue.defer_capture_origin(&initial_origin, retry_at)?;
                                }
                            }
                            self.queue.finish(
                                &j,
                                if exhausted {
                                    State::Failed
                                } else {
                                    State::RetryWait
                                },
                                Stage::Fetch,
                                Some(if exhausted {
                                    "retry_exhausted"
                                } else {
                                    error.reason_code()
                                }),
                                if exhausted {
                                    0
                                } else {
                                    retry_at.saturating_sub(now)
                                },
                            )?;
                        }
                    }
                    return Ok(true);
                }
            };
            let captured = chrono::Utc::now().to_rfc3339();
            let digest = hex::encode(Sha256::digest(&fetched.body));
            let external_capture_id = j
                .external_capture_id
                .clone()
                .unwrap_or_else(|| format!("agent_{}", uuid::Uuid::now_v7().simple()));
            let cap = web_capture(
                &external_capture_id,
                &captured,
                fetched.final_url.as_str(),
                &fetched.media_type,
                &digest,
                fetched.body.clone(),
            );
            let decision = admission_decision(&cap, Some(fetched.status));
            let class = admission_class_text(decision.class);
            if matches!(
                decision.class,
                AdmissionClass::InteractiveLoginRequired
                    | AdmissionClass::ChallengeDetected
                    | AdmissionClass::UnsupportedContent
            ) {
                let reason = match decision.class {
                    AdmissionClass::InteractiveLoginRequired => "interactive_login_required",
                    AdmissionClass::ChallengeDetected => "challenge_detected",
                    _ => "unsupported_content",
                };
                self.queue
                    .finish(&j, State::NeedsUser, Stage::Validate, Some(reason), 0)?;
                return Ok(true);
            }
            let summary = extract_url(fetched.final_url.as_str())?.1;
            let path = install(&self.cfg.spool, &digest, &fetched.body)?;
            self.queue.freeze(
                &j,
                &external_capture_id,
                &digest,
                &path,
                &fetched.media_type,
                fetched.body.len() as u64,
                &captured,
                fetched.final_url.as_str(),
                fetched.status,
                class,
                decision.reason_code,
                decision.policy_version,
                Some((&summary.host, &summary.path_class, summary.query_present)),
            )?;
            j = self.queue.record(&j.view.job_id)?;
            if decision.class == AdmissionClass::NeedsPreview {
                return Ok(true);
            }
        }
        let bytes = match read_verified(&self.cfg.spool, &j) {
            Ok(b) => b,
            Err(_) => {
                self.queue.finish(
                    &j,
                    State::Failed,
                    Stage::Validate,
                    Some("artifact_verification_failed"),
                    0,
                )?;
                return Ok(true);
            }
        };
        let external_capture_id = j
            .external_capture_id
            .as_deref()
            .context("frozen_metadata_missing")?;
        let cap = web_capture(
            external_capture_id,
            j.captured_at
                .as_deref()
                .context("frozen_metadata_missing")?,
            j.final_url.as_deref().context("frozen_metadata_missing")?,
            j.artifact_media
                .as_deref()
                .context("frozen_metadata_missing")?,
            j.artifact_digest
                .as_deref()
                .context("frozen_metadata_missing")?,
            bytes,
        );
        let approval = match (&j.approval_digest, &j.approval_policy) {
            (Some(d), Some(p)) => Some(PreviewApproval {
                artifact_sha256: d.clone(),
                policy_version: p.clone(),
            }),
            _ => None,
        };
        self.queue.stage(&j, Stage::Translate)?;
        let mut prepared = match prepare_capture(
            CaptureSubmission {
                capture: cap,
                metadata: ItemMetadata::default(),
                attachments: vec![],
            },
            AdmissionContext {
                http_status: j.http_status,
                preview_approval: approval,
            },
        ) {
            Ok(x) => x,
            Err(_) => {
                self.queue.finish(
                    &j,
                    State::Failed,
                    Stage::Translate,
                    Some("translation_failed"),
                    0,
                )?;
                return Ok(true);
            }
        };
        let chrome_options = self.queue.chrome_options(&j.view.job_id)?;
        let mut chrome_result = None;
        if let Some(options) = &chrome_options {
            let comments = if options.include_comments {
                match prepared.extract_comments() {
                    Ok(observation) => {
                        let receipt = serde_json::json!({"state":observation.state,"reason":null});
                        prepared = prepared.with_comments(observation, "agent-cas")?;
                        for blob in prepared.source_blobs() {
                            install(&self.cfg.spool, &blob.descriptor.sha256, &blob.bytes)?;
                        }
                        receipt
                    }
                    Err(_) => {
                        serde_json::json!({"state":"unavailable","reason":"comments_rejected"})
                    }
                }
            } else {
                serde_json::json!({"state":"not_requested","reason":null})
            };
            let result = serde_json::json!({
                "comments":comments,
                "quality_check": {"state": if options.quality_check == memex_intake::url_jobs::QualityMode::Off { "not_requested" } else { "running" },
                    "reason":null, "analysis_id":null},
            });
            self.queue.set_chrome_result(&j, &result)?;
            chrome_result = Some(result);
        }
        self.queue.stage(&j, Stage::Commit)?;
        let committed = if let Some(options) = chrome_options {
            memex_capture::commit_capture_checked(
                &self.cfg.library,
                prepared,
                options.quality_check == memex_intake::url_jobs::QualityMode::Configured,
                self.quality_model.as_deref(),
            )
            .map(|(receipt, quality)| (receipt, Some(quality)))
        } else {
            commit_capture(&self.cfg.library, prepared).map(|receipt| (receipt, None))
        };
        match committed {
            Ok((receipt, quality)) => {
                if let (Some(mut result), Some(quality)) = (chrome_result, quality) {
                    result["quality_check"] = quality;
                    self.queue.set_chrome_result(&j, &result)?;
                }
                let item_id = receipt.item_ids.first().context("invalid_item_receipt")?;
                let revision_id = receipt
                    .revision_ids
                    .first()
                    .context("invalid_item_receipt")?;
                if self.archive_capture_destination(&j, item_id).is_err() {
                    self.queue.finish(&j, State::NeedsUser, Stage::Commit, Some("destination_conflict"), 0)?;
                    return Ok(true);
                }
                // Generated summary and translation are post-Store derivatives. The capture queue
                // durably owns one idempotent hook intent; a separate worker acknowledges both so
                // Analysis I/O and model work cannot block or misreport the canonical capture.
                self.queue.finish_stored(&j, item_id, revision_id)?;
            }
            Err(_) => self.queue.finish(
                &j,
                State::RetryWait,
                Stage::Commit,
                Some("commit_retry"),
                30,
            )?,
        }
        Ok(true)
    }

    #[cfg(test)]
    pub(crate) fn reconcile_one_translation_intent(&mut self) -> Result<bool> {
        let Some(intent) = self.queue.pending_translation_intent()? else {
            return Ok(false);
        };
        memex_analysis::enqueue_item_translation(
            &self.cfg.library,
            &intent.item_id,
            &intent.revision_id,
        )?;
        memex_analysis::enqueue_item_summary(
            &self.cfg.library,
            &intent.item_id,
            &intent.revision_id,
        )?;
        self.queue.complete_translation_intent(&intent)?;
        Ok(true)
    }
}
pub fn admission_class_text(c: AdmissionClass) -> &'static str {
    match c {
        AdmissionClass::Article => "article",
        AdmissionClass::NeedsPreview => "needs_preview",
        AdmissionClass::InteractiveLoginRequired => "interactive_login_required",
        AdmissionClass::ChallengeDetected => "challenge_detected",
        AdmissionClass::UnsupportedContent => "unsupported_content",
    }
}
/// The capture of one fetched page, as the agent's web adapter records it.
pub fn web_capture(
    external_capture_id: &str,
    captured: &str,
    url: &str,
    media: &str,
    digest: &str,
    bytes: Vec<u8>,
) -> VerifiedCapture {
    let descriptor = BlobDescriptor {
        schema_version: 1,
        locator: format!("sha256:{digest}"),
        media_type: media.into(),
        byte_size: bytes.len() as u64,
        sha256: digest.into(),
        source_url: (media == "application/json")
            .then(|| lots_scout_extract::LinkresearcherArticleExtractor::api_url(url))
            .flatten(),
    };
    VerifiedCapture {
        source: SourceProfile {
            schema_version: 1,
            source_id: "memex-agent-web".into(),
            name: "MemeX Agent Web Capture".into(),
            source_kind: "web".into(),
            canonical_uris: vec![],
            evidence_roles: vec![],
            default_sensitivity: Sensitivity::Internal,
            preferred_extractor: None,
        },
        capture: CaptureEnvelope {
            schema_version: 1,
            adapter_id: "memex-agent".into(),
            adapter_version: "1".into(),
            external_capture_id: external_capture_id.into(),
            source_id: "memex-agent-web".into(),
            captured_at: captured.into(),
            capture_method: if descriptor.source_url.is_some() {
                "public_article_api_fetch"
            } else {
                "safe_http_fetch"
            }
            .into(),
            sensitivity: Sensitivity::Internal,
            route_summary: None,
            title: None,
            source_url: url.into(),
            canonical_url: Some(url.into()),
            artifacts: vec![descriptor.clone()],
        },
        blobs: vec![VerifiedBlob {
            descriptor,
            blob_id: format!("sha256:{digest}"),
            source_backend_ref: Some(format!("agent-cas:sha256:{digest}")),
            bytes,
        }],
    }
}
pub(crate) fn install(root: &Path, digest: &str, bytes: &[u8]) -> Result<PathBuf> {
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || hex::encode(Sha256::digest(bytes)) != digest
    {
        bail!("invalid_digest")
    }
    ensure_owner_directory(root)?;
    let cas = root.join("cas");
    ensure_owner_directory(&cas)?;
    let dir = cas.join(&digest[..2]);
    ensure_owner_directory(&dir)?;
    let path = dir.join(digest);
    let temporary = dir.join(format!(".{digest}.{}.tmp", uuid::Uuid::now_v7().simple()));
    let result = (|| -> Result<()> {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temporary)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        match fs::hard_link(&temporary, &path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                read_exact_path(&path, digest, bytes.len() as u64).map(|_| ())
            }
            Err(error) => Err(error.into()),
        }
    })();
    let _ = fs::remove_file(&temporary);
    result?;
    read_exact_path(&path, digest, bytes.len() as u64)?;
    let d = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(&dir)?;
    if unsafe { libc::fsync(d.as_raw_fd()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(path)
}

pub(crate) fn read_exact_path(path: &Path, digest: &str, size: u64) -> Result<Vec<u8>> {
    if size > crate::fetch::MAX_BODY.max(crate::fetch::MAX_EXACT_IMAGE) as u64 {
        bail!("artifact_mismatch")
    }
    let mut f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let m = f.metadata()?;
    if !m.is_file()
        || m.uid() != unsafe { libc::geteuid() }
        || m.len() != size
        || m.permissions().mode() & 0o077 != 0
    {
        bail!("artifact_mismatch")
    }
    let mut b = Vec::new();
    f.read_to_end(&mut b)?;
    if hex::encode(Sha256::digest(&b)) != digest {
        bail!("artifact_mismatch")
    }
    Ok(b)
}
pub(crate) fn read_verified(spool: &Path, j: &JobRecord) -> Result<Vec<u8>> {
    let digest = j
        .artifact_digest
        .as_deref()
        .context("frozen_metadata_missing")?;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("artifact_mismatch")
    }
    let expected = spool.join("cas").join(&digest[..2]).join(digest);
    if j.artifact_path.as_deref() != Some(expected.as_path()) {
        bail!("artifact_mismatch")
    }
    read_exact_path(
        &expected,
        digest,
        j.byte_size.context("frozen_metadata_missing")?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{fetch::RawResponse, queue::State};
    use std::{
        collections::VecDeque,
        net::{IpAddr, SocketAddr},
        os::unix::fs::symlink,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Mutex,
        },
        time::Duration,
    };
    use url::Url;

    struct PublicResolver;
    impl Resolver for PublicResolver {
        fn resolve(&self, _: &str, _: Duration) -> Result<Vec<IpAddr>> {
            Ok(vec!["93.184.216.34".parse().unwrap()])
        }
    }
    struct PoisonedResolver;
    impl Resolver for PoisonedResolver {
        fn resolve(&self, _: &str, _: Duration) -> Result<Vec<IpAddr>> {
            bail!("dns_timeout")
        }
    }
    struct NonPublicResolver;
    impl Resolver for NonPublicResolver {
        fn resolve(&self, _: &str, _: Duration) -> Result<Vec<IpAddr>> {
            Ok(vec!["127.0.0.1".parse().unwrap()])
        }
    }

    struct ScriptTransport {
        responses: Mutex<VecDeque<RawResponse>>,
        calls: AtomicUsize,
    }

    struct RetryTransport(AtomicUsize);
    impl HttpTransport for RetryTransport {
        fn get(&self, _: &Url, _: &[SocketAddr], _: Duration) -> Result<RawResponse> {
            self.0.fetch_add(1, Ordering::SeqCst);
            bail!("transport_error")
        }
    }
    impl ScriptTransport {
        fn new(responses: Vec<RawResponse>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                calls: AtomicUsize::new(0),
            }
        }
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }
    impl HttpTransport for ScriptTransport {
        fn get(&self, _: &Url, _: &[SocketAddr], _: Duration) -> Result<RawResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .context("missing_scripted_response")
        }
    }

    fn config(root: &Path) -> AgentConfig {
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

    fn response(status: u16, body: &str) -> RawResponse {
        RawResponse {
            status,
            headers: vec![
                ("content-type".into(), "text/html; charset=utf-8".into()),
                ("content-length".into(), body.len().to_string()),
            ],
            body: body.as_bytes().to_vec(),
        }
    }

    fn readable(container: &str) -> String {
        format!(
            "<{container}><h1>Synthetic Capture</h1><p>This synthetic public article contains enough deterministic prose for the Readability translator to extract a stable document without any network access or private data.</p></{container}>"
        )
    }

    #[test]
    fn public_article_api_uses_safe_fetch_and_freezes_original_response() {
        use memex_intake::url_jobs::{QualityMode, UrlEnqueue};
        const ID: &str = "11111111-2222-3333-4444-555555555555";
        struct ApiTransport;
        impl HttpTransport for ApiTransport {
            fn get(&self, url: &Url, pinned: &[SocketAddr], _: Duration) -> Result<RawResponse> {
                assert_eq!(
                    url.as_str(),
                    format!("https://www.linkresearcher.com/api/theses/{ID}")
                );
                assert!(!pinned.is_empty());
                Ok(RawResponse {
                    status: 200,
                    headers: vec![("content-type".into(), "application/json; charset=utf-8".into())],
                    body: serde_json::to_vec(&serde_json::json!({"id":ID,"title":"Synthetic source API", "content":"<p>Original public article text.</p><div itemprop=comment>COMMENT-ONLY</div>"})).unwrap(),
                })
            }
        }
        let root = tempfile::tempdir().unwrap();
        let cfg = config(root.path());
        let mut c = Coordinator::new(cfg.clone(), PublicResolver, ApiTransport).unwrap();
        let url = format!("https://www.linkresearcher.com/theses/{ID}");
        let owner = "a".repeat(64);
        let request = UrlEnqueue {
            schema_version: 1,
            request_key: format!("chrome_capture_{}", "1".repeat(32)),
            url: url.clone(),
            include_comments: true,
            quality_check: QualityMode::Off,
            generation_policy: None,
            destination_mode: None,
            target_group_id: None,
            remove_from_group_ids: vec![],
            source_hints: None,
        };
        let job = c.queue.chrome_enqueue(&owner, &request).unwrap();
        fs::remove_dir(&cfg.library).unwrap();
        fs::write(&cfg.library, b"temporary Store failure").unwrap();
        c.process_one().unwrap();
        assert_eq!(
            c.queue.record(&job.job_id).unwrap().view.state,
            State::RetryWait
        );
        fs::remove_file(&cfg.library).unwrap();
        drop(c);
        // Reopen with a resolver that cannot make a new request: frozen API bytes
        // and supplemental comments must suffice for recovery.
        let mut c = Coordinator::new(cfg.clone(), PoisonedResolver, ApiTransport).unwrap();
        c.queue.retry(&job.job_id).unwrap();
        c.process_one().unwrap();
        let record = c.queue.record(&job.job_id).unwrap();
        assert_eq!(record.view.state, State::Succeeded);
        assert_eq!(record.artifact_media.as_deref(), Some("application/json"));
        assert_eq!(record.final_url.as_deref(), Some(url.as_str()));
        let bytes = read_verified(&cfg.spool, &record).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["id"],
            ID
        );
        let items = memex_store::list_items(&cfg.library).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].1.content_markdown,
            "Original public article text.\n"
        );
        let receipt = c.queue.chrome_receipt(&owner, &job.job_id).unwrap();
        assert_eq!(receipt["comments"]["state"], "captured");
        assert_eq!(
            c.queue.chrome_enqueue(&owner, &request).unwrap().job_id,
            job.job_id
        );
        assert!(!c.process_one().unwrap());
        // Even this known source must still pass DNS/SSRF checks.
        assert!(fetch(&url, &NonPublicResolver, &ApiTransport).is_err());
        let redirect = ScriptTransport::new(vec![RawResponse {
            status: 302,
            headers: vec![("location".into(), "https://other.test/private".into())],
            body: vec![],
        }]);
        assert!(fetch(&url, &PublicResolver, &redirect).is_err());
        assert_eq!(redirect.calls(), 1);
    }

    #[test]
    fn chrome_cross_redirect_workers_do_not_turn_local_contention_into_shared_backoff() {
        use memex_intake::url_jobs::{QualityMode, UrlEnqueue};
        // A and B each hold their initial origin, then redirect into the other's origin.
        // A hops first and must yield to B's live lease; B hops only after A has returned,
        // into A's origin whose polite reservation the test fixes (the product draws it at
        // random, which made the old "first round succeeds" assertion timing-dependent).
        struct CrossRedirect {
            barrier: std::sync::Arc<std::sync::Barrier>,
            db: PathBuf,
            delay: i64,
        }
        impl CrossRedirect {
            fn conn(&self) -> rusqlite::Connection {
                let c = rusqlite::Connection::open(&self.db).unwrap();
                c.busy_timeout(Duration::from_secs(5)).unwrap();
                c
            }
        }
        impl HttpTransport for CrossRedirect {
            fn get(&self, url: &Url, _: &[SocketAddr], _: Duration) -> Result<RawResponse> {
                if url.path() != "/start" {
                    return Ok(response(200, &readable("article")));
                }
                let a = url.host_str() == Some("a.test");
                // Only the first round is choreographed; recovery hops run unhindered.
                let first_round: bool = self.conn().query_row(
                    "SELECT count(*)=0 FROM jobs WHERE state IN ('retry_wait','succeeded')",
                    [],
                    |r| r.get(0),
                )?;
                if first_round {
                    self.barrier.wait(); // Both initial origins are leased.
                    let c = self.conn();
                    if a {
                        let epoch: i64 = c.query_row(
                            "SELECT lease_until-240 FROM jobs WHERE fetch_origin='https://a.test'",
                            [],
                            |r| r.get(0),
                        )?;
                        c.execute(
                            "UPDATE capture_origins SET next_fetch_at=?1 WHERE origin='https://a.test'",
                            [epoch + self.delay],
                        )?;
                    } else {
                        let deadline = std::time::Instant::now() + Duration::from_secs(10);
                        while c.query_row(
                            "SELECT state FROM jobs WHERE fetch_origin='https://a.test'",
                            [],
                            |r| r.get::<_, String>(0),
                        )? != "retry_wait"
                        {
                            assert!(std::time::Instant::now() < deadline, "A never yielded");
                            std::thread::sleep(Duration::from_millis(10));
                        }
                    }
                }
                let destination = if a { "b.test" } else { "a.test" };
                Ok(RawResponse {
                    status: 302,
                    headers: vec![("location".into(), format!("https://{destination}/finish"))],
                    body: vec![],
                })
            }
        }
        // 30s exceeds the 25s per-attempt wait budget, so B must defer too; 1s fits, so B
        // waits and finishes in the first round. Either way nothing may become shared backoff.
        for (delay, b_first_round) in [(30, "retry_wait"), (1, "succeeded")] {
            let root = tempfile::tempdir().unwrap();
            let cfg = config(root.path());
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let transport = || CrossRedirect {
                barrier: barrier.clone(),
                db: cfg.database.clone(),
                delay,
            };
            let mut first = Coordinator::new(cfg.clone(), PublicResolver, transport()).unwrap();
            let mut second = Coordinator::new(cfg.clone(), PublicResolver, transport()).unwrap();
            let owner = "a".repeat(64);
            let jobs: Vec<_> = [('A', "a.test"), ('B', "b.test")]
                .into_iter()
                .map(|(key, host)| {
                    first
                        .queue
                        .chrome_enqueue(
                            &owner,
                            &UrlEnqueue {
                                schema_version: 1,
                                request_key: format!("chrome_capture_{}", key.to_string().repeat(32)),
                                url: format!("https://{host}/start"),
                                include_comments: false,
                                quality_check: QualityMode::Off,
                                generation_policy: None,
                                destination_mode: None,
                                target_group_id: None,
                                remove_from_group_ids: vec![],
                                source_hints: None,
                            },
                        )
                        .unwrap()
                        .job_id
                })
                .collect();
            std::thread::scope(|scope| {
                let one = scope.spawn(|| first.process_one().unwrap());
                let two = scope.spawn(|| second.process_one().unwrap());
                assert!(one.join().unwrap());
                assert!(two.join().unwrap());
            });
            let receipt = |id: &String| first.queue.chrome_receipt(&owner, id).unwrap();
            assert_eq!(receipt(&jobs[0])["state"], "retry_wait", "delay {delay}");
            assert_eq!(receipt(&jobs[1])["state"], b_first_round, "delay {delay}");
            let db = transport().conn();
            // Every deferral is a polite wait that returned its attempt and lease.
            let unreturned: i64 = db
                .query_row(
                    "SELECT count(*) FROM jobs WHERE state='retry_wait' AND NOT (reason_code='polite_wait' AND attempt_count=0 AND lease_owner IS NULL AND lease_until IS NULL)",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(unreturned, 0, "delay {delay}");
            // Neither origin inherited the other's 240s lease as server backoff; only the fixed
            // reservation and the ordinary 8..=25s politeness gap after a fetch may remain.
            let latest: i64 = db
                .query_row("SELECT MAX(next_fetch_at) FROM capture_origins", [], |r| r.get(0))
                .unwrap();
            assert!(
                latest <= chrono::Utc::now().timestamp() + delay.max(25) + 1,
                "delay {delay}: shared backoff until {latest}"
            );
            // Simulate the clock reaching the reservations on this synthetic DB only: the
            // same jobs resume without a manual retry and finish with one business attempt.
            db.execute_batch(
                "UPDATE jobs SET next_attempt_at=0 WHERE state='retry_wait'; UPDATE capture_origins SET next_fetch_at=0;",
            )
            .unwrap();
            while db
                .query_row("SELECT count(*) FROM jobs WHERE state!='succeeded'", [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap()
                > 0
            {
                assert!(first.process_one().unwrap(), "delay {delay}: no progress");
                db.execute("UPDATE capture_origins SET next_fetch_at=0", []).unwrap();
            }
            let attempts: Vec<i64> = db
                .prepare("SELECT attempt_count FROM jobs ORDER BY fetch_origin")
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            assert_eq!(attempts, [1, 1], "delay {delay}");
            // One item per job: the resumed jobs neither duplicated nor dropped a capture.
            assert_eq!(memex_store::list_items(&cfg.library).unwrap().len(), 2);
        }
    }

    #[test]
    fn chrome_redirect_backoff_blocks_destination_and_survives_manual_retry_restart() {
        use memex_intake::url_jobs::{QualityMode, UrlEnqueue};
        for status in [429, 503] {
            let root = tempfile::tempdir().unwrap();
            let cfg = config(root.path());
            let mut coordinator = Coordinator::new(
                cfg.clone(),
                PublicResolver,
                ScriptTransport::new(vec![
                    RawResponse {
                        status: 302,
                        headers: vec![(
                            "location".into(),
                            "https://destination.test/article".into(),
                        )],
                        body: vec![],
                    },
                    RawResponse {
                        status,
                        headers: vec![("retry-after".into(), "3600".into())],
                        body: vec![],
                    },
                ]),
            )
            .unwrap();
            let owner = "a".repeat(64);
            let job = coordinator
                .queue
                .chrome_enqueue(
                    &owner,
                    &UrlEnqueue {
                        schema_version: 1,
                        request_key: format!("chrome_capture_{}", "A".repeat(32)),
                        url: "https://initial.test/article".into(),
                        include_comments: false,
                        quality_check: QualityMode::Off,
                        generation_policy: None,
                        destination_mode: None,
                        target_group_id: None,
                        remove_from_group_ids: vec![],
                        source_hints: None,
                    },
                )
                .unwrap();
            let before = chrono::Utc::now().timestamp();
            assert!(coordinator.process_one().unwrap());
            assert_eq!(coordinator.transport.calls(), 2);
            let receipt = coordinator
                .queue
                .chrome_receipt(&owner, &job.job_id)
                .unwrap();
            assert_eq!(receipt["state"], "retry_wait");
            assert_eq!(receipt["reason"], "rate_limited");
            assert!(receipt["retry_at_ms"].as_i64().unwrap() >= (before + 3600) * 1000);
            coordinator.queue.retry(&job.job_id).unwrap();
            drop(coordinator);
            let mut restarted =
                Coordinator::new(cfg.clone(), PublicResolver, ScriptTransport::new(vec![]))
                    .unwrap();
            assert!(!restarted.process_one().unwrap());
            assert_eq!(restarted.transport.calls(), 0);
            // A different initial origin must also honor the destination's backoff.
            restarted
                .queue
                .chrome_enqueue(
                    &owner,
                    &UrlEnqueue {
                        schema_version: 1,
                        request_key: format!("chrome_capture_{}", "B".repeat(32)),
                        url: "https://other.test/article".into(),
                        include_comments: false,
                        quality_check: QualityMode::Off,
                        generation_policy: None,
                        destination_mode: None,
                        target_group_id: None,
                        remove_from_group_ids: vec![],
                        source_hints: None,
                    },
                )
                .unwrap();
            restarted.transport = ScriptTransport::new(vec![RawResponse {
                status: 302,
                headers: vec![("location".into(), "https://destination.test/other".into())],
                body: vec![],
            }]);
            assert!(restarted.process_one().unwrap());
            assert_eq!(
                restarted.transport.calls(),
                1,
                "deferred hop must never reach the transport"
            );
            assert!(memex_store::list_items(&cfg.library).unwrap().is_empty());
        }
    }

    #[test]
    fn article_fetch_freezes_and_commits_one_store_item() {
        let root = tempfile::tempdir().unwrap();
        let html = readable("article");
        let mut coordinator = Coordinator::new(
            config(root.path()),
            PublicResolver,
            ScriptTransport::new(vec![response(200, &html)]),
        )
        .unwrap();
        let job = coordinator
            .queue
            .enqueue("test", "article", "https://article.test/one")
            .unwrap();

        assert!(coordinator.process_one().unwrap());
        let stored = coordinator.queue.get(&job.job_id).unwrap();
        assert_eq!(stored.state, State::Succeeded);
        assert!(stored
            .item_id
            .as_deref()
            .is_some_and(|id| id.starts_with("item_")));
        assert_eq!(coordinator.transport.calls(), 1);
        assert_eq!(
            memex_store::list_items(root.path().join("library"))
                .unwrap()
                .len(),
            1
        );
        assert!(coordinator.reconcile_one_translation_intent().unwrap());
        let item_id = stored.item_id.as_deref().unwrap();
        let (_, revision) = memex_store::read_item(root.path().join("library"), item_id).unwrap();
        let translation = memex_analysis::translation_status(
            root.path().join("library"),
            item_id,
            &revision.revision_id,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            translation.state,
            memex_analysis::TranslationJobState::Queued
        );
        let record = coordinator.queue.record(&job.job_id).unwrap();
        assert!(record.artifact_path.unwrap().is_file());
        assert_eq!(record.byte_size, Some(html.len() as u64));
    }

    #[test]
    fn post_store_translation_intent_retries_after_restart_without_refetch_or_duplicate_item() {
        let root = tempfile::tempdir().unwrap();
        let cfg = config(root.path());
        let html = readable("article");
        let mut coordinator = Coordinator::new(
            cfg.clone(),
            PublicResolver,
            ScriptTransport::new(vec![response(200, &html)]),
        )
        .unwrap();
        let job = coordinator
            .queue
            .enqueue("test", "durable-hook", "https://article.test/durable")
            .unwrap();
        coordinator.process_one().unwrap();
        assert_eq!(coordinator.transport.calls(), 1);
        let stored = coordinator.queue.get(&job.job_id).unwrap();
        assert_eq!(stored.state, State::Succeeded);
        let item_id = stored.item_id.unwrap();
        let intent = coordinator
            .queue
            .pending_translation_intent()
            .unwrap()
            .unwrap();
        assert_eq!(intent.item_id, item_id);

        fs::write(
            cfg.library.join("analysis"),
            b"blocks Analysis queue creation",
        )
        .unwrap();
        assert!(coordinator.reconcile_one_translation_intent().is_err());
        assert_eq!(
            coordinator.queue.pending_translation_intent().unwrap(),
            Some(intent.clone())
        );
        assert_eq!(memex_store::list_items(&cfg.library).unwrap().len(), 1);
        drop(coordinator);

        fs::remove_file(cfg.library.join("analysis")).unwrap();
        let mut restarted =
            Coordinator::new(cfg, PublicResolver, ScriptTransport::new(Vec::new())).unwrap();
        assert!(restarted.reconcile_one_translation_intent().unwrap());
        assert!(restarted
            .queue
            .pending_translation_intent()
            .unwrap()
            .is_none());
        assert_eq!(restarted.transport.calls(), 0);
        assert_eq!(
            memex_store::list_items(root.path().join("library"))
                .unwrap()
                .len(),
            1
        );
        let status = memex_analysis::translation_status(
            root.path().join("library"),
            &intent.item_id,
            &intent.revision_id,
        )
        .unwrap()
        .unwrap();
        assert_eq!(status.state, memex_analysis::TranslationJobState::Queued);
    }

    #[test]
    fn preview_approval_and_commit_retry_reuse_frozen_bytes_without_refetch() {
        let root = tempfile::tempdir().unwrap();
        let html = readable("main");
        let mut coordinator = Coordinator::new(
            config(root.path()),
            PublicResolver,
            ScriptTransport::new(vec![response(200, &html)]),
        )
        .unwrap();
        let preview = coordinator
            .queue
            .enqueue("test", "preview", "https://preview.test/one")
            .unwrap();
        coordinator.process_one().unwrap();
        let waiting = coordinator.queue.get(&preview.job_id).unwrap();
        assert_eq!(waiting.state, State::NeedsUser);
        assert_eq!(waiting.reason_code.as_deref(), Some("needs_preview"));
        assert_eq!(waiting.preview.as_ref().unwrap().host, "preview.test");
        assert_eq!(
            waiting.preview.as_ref().unwrap().path_class,
            "single_segment"
        );
        coordinator.queue.approve(&preview.job_id).unwrap();
        coordinator.process_one().unwrap();
        assert_eq!(
            coordinator.queue.get(&preview.job_id).unwrap().state,
            State::Succeeded
        );
        assert_eq!(coordinator.transport.calls(), 1);

        let root = tempfile::tempdir().unwrap();
        let cfg = config(root.path());
        let library = cfg.library.clone();
        let mut coordinator = Coordinator::new(
            cfg,
            PublicResolver,
            ScriptTransport::new(vec![response(200, &readable("article"))]),
        )
        .unwrap();
        fs::remove_dir(&library).unwrap();
        fs::write(&library, b"blocks Store root creation").unwrap();
        let job = coordinator
            .queue
            .enqueue("test", "retry", "https://retry.test/one")
            .unwrap();
        coordinator.process_one().unwrap();
        assert_eq!(
            coordinator.queue.get(&job.job_id).unwrap().state,
            State::RetryWait
        );
        assert_eq!(coordinator.transport.calls(), 1);
        fs::remove_file(&library).unwrap();
        fs::create_dir(&library).unwrap();
        fs::set_permissions(&library, fs::Permissions::from_mode(0o700)).unwrap();
        coordinator.queue.retry(&job.job_id).unwrap();
        coordinator.process_one().unwrap();
        assert_eq!(
            coordinator.queue.get(&job.job_id).unwrap().state,
            State::Succeeded
        );
        assert_eq!(coordinator.transport.calls(), 1);
    }

    #[test]
    fn login_and_auth_responses_write_neither_cas_nor_store() {
        for (case, response) in [
            (
                "jike",
                response(200, "<html><body>即刻App 二维码登录 verify</body></html>"),
            ),
            (
                "login-marker",
                response(200, "<html><body>Please log in to continue.</body></html>"),
            ),
            ("auth", response(403, "authentication required")),
            (
                "password-control",
                response(
                    200,
                    "<article><input type = 'password'><p>Synthetic login prose.</p></article>",
                ),
            ),
            (
                "password-quoted-marker",
                response(
                    200,
                    "<article><input data-marker='>' type='password'><p>Synthetic login prose.</p></article>",
                ),
            ),
            (
                "password-entity-unquoted",
                response(
                    200,
                    "<article><input type=pass&#119;ord><p>Synthetic login prose.</p></article>",
                ),
            ),
            (
                "otp-control",
                response(
                    200,
                    "<article><input autocomplete='one-time-code'><p>Synthetic login prose.</p></article>",
                ),
            ),
            (
                "standalone-textarea",
                response(
                    200,
                    "<article><textarea>Synthetic value.</textarea><p>Synthetic prose.</p></article>",
                ),
            ),
            (
                "standalone-input",
                response(
                    200,
                    "<article><input type=text><p>Synthetic prose.</p></article>",
                ),
            ),
            (
                "standalone-select",
                response(
                    200,
                    "<article><select><option>Synthetic value.</option></select><p>Synthetic prose.</p></article>",
                ),
            ),
            (
                "contenteditable",
                response(
                    200,
                    "<article><div contenteditable>Synthetic value.</div><p>Synthetic prose.</p></article>",
                ),
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            let mut coordinator = Coordinator::new(
                config(root.path()),
                PublicResolver,
                ScriptTransport::new(vec![response]),
            )
            .unwrap();
            let job = coordinator
                .queue
                .enqueue("test", case, &format!("https://{case}.test/one"))
                .unwrap();
            coordinator.process_one().unwrap();
            let waiting = coordinator.queue.get(&job.job_id).unwrap();
            assert_eq!(waiting.state, State::NeedsUser);
            assert_eq!(
                waiting.reason_code.as_deref(),
                Some("interactive_login_required")
            );
            assert!(!root.path().join("spool/cas").exists());
            assert_eq!(
                fs::read_dir(root.path().join("library")).unwrap().count(),
                0
            );
            assert!(coordinator.queue.approve(&job.job_id).is_err());
        }
    }

    #[test]
    fn fake_articles_and_large_malformed_html_write_nothing() {
        let mut malformed = String::from("<article>");
        malformed.push_str(&"<input ".repeat(20_000));
        malformed.push_str("><p>visible body</p></article>");
        for (case, body) in [
            (
                "comment-fake-article",
                "<!-- <article>comment-only fake body</article> -->".to_string(),
            ),
            (
                "script-fake-article",
                "<script>const fake = '<article>script-only fake body</article>';</script>"
                    .to_string(),
            ),
            ("malformed-large-tag", malformed),
        ] {
            let root = tempfile::tempdir().unwrap();
            let mut coordinator = Coordinator::new(
                config(root.path()),
                PublicResolver,
                ScriptTransport::new(vec![response(200, &body)]),
            )
            .unwrap();
            let job = coordinator
                .queue
                .enqueue("test", case, &format!("https://{case}.test/one"))
                .unwrap();
            coordinator.process_one().unwrap();
            let waiting = coordinator.queue.get(&job.job_id).unwrap();
            assert_eq!(waiting.state, State::NeedsUser);
            assert_eq!(waiting.reason_code.as_deref(), Some("unsupported_content"));
            assert!(!root.path().join("spool/cas").exists());
            assert_eq!(
                fs::read_dir(root.path().join("library")).unwrap().count(),
                0
            );
            assert!(coordinator.queue.approve(&job.job_id).is_err());
        }
    }

    #[test]
    fn unsupported_fetch_is_terminal_and_transient_fetch_has_a_bounded_budget() {
        let root = tempfile::tempdir().unwrap();
        let mut coordinator = Coordinator::new(
            config(root.path()),
            NonPublicResolver,
            ScriptTransport::new(vec![]),
        )
        .unwrap();
        let job = coordinator
            .queue
            .enqueue("test", "policy", "https://policy.test/one")
            .unwrap();
        coordinator.process_one().unwrap();
        let rejected = coordinator.queue.get(&job.job_id).unwrap();
        assert_eq!(rejected.state, State::Failed);
        assert_eq!(rejected.reason_code.as_deref(), Some("address_not_public"));
        assert_eq!(coordinator.transport.calls(), 0);

        let root = tempfile::tempdir().unwrap();
        let unsupported = RawResponse {
            status: 200,
            headers: vec![("content-type".into(), "text/plain".into())],
            body: b"not an article".to_vec(),
        };
        let mut coordinator = Coordinator::new(
            config(root.path()),
            PublicResolver,
            ScriptTransport::new(vec![unsupported]),
        )
        .unwrap();
        let job = coordinator
            .queue
            .enqueue("test", "unsupported", "https://unsupported.test/one")
            .unwrap();
        coordinator.process_one().unwrap();
        let failed = coordinator.queue.get(&job.job_id).unwrap();
        assert_eq!(failed.state, State::Failed);
        assert_eq!(failed.reason_code.as_deref(), Some("unsupported_content"));
        assert!(!root.path().join("spool/cas").exists());

        let root = tempfile::tempdir().unwrap();
        let mut coordinator = Coordinator::new(
            config(root.path()),
            PublicResolver,
            RetryTransport(AtomicUsize::new(0)),
        )
        .unwrap();
        let job = coordinator
            .queue
            .enqueue("test", "bounded-retry", "https://retry.test/one")
            .unwrap();
        for attempt in 1..=MAX_AUTOMATIC_ATTEMPTS {
            coordinator.process_one().unwrap();
            let view = coordinator.queue.get(&job.job_id).unwrap();
            if attempt == MAX_AUTOMATIC_ATTEMPTS {
                assert_eq!(view.state, State::Failed);
                assert_eq!(view.reason_code.as_deref(), Some("retry_exhausted"));
            } else {
                assert_eq!(view.state, State::RetryWait);
                coordinator.queue.make_retry_due(&job.job_id).unwrap();
            }
        }
        assert_eq!(
            coordinator.transport.0.load(Ordering::SeqCst),
            MAX_AUTOMATIC_ATTEMPTS as usize
        );

        let root = tempfile::tempdir().unwrap();
        let mut coordinator = Coordinator::new(
            config(root.path()),
            PoisonedResolver,
            ScriptTransport::new(vec![]),
        )
        .unwrap();
        let job = coordinator
            .queue
            .enqueue("test", "poisoned-worker", "https://worker.test/one")
            .unwrap();
        assert_eq!(
            coordinator.process_one().unwrap_err().to_string(),
            "fetch_worker_failed"
        );
        assert_eq!(
            coordinator.queue.get(&job.job_id).unwrap().state,
            State::Leased
        );
        assert!(!root.path().join("spool/cas").exists());
    }

    #[test]
    fn cas_rejects_symlinks_mismatches_and_oversized_claims() {
        let root = tempfile::tempdir().unwrap();
        let spool = root.path().join("spool");
        ensure_owner_directory(&spool).unwrap();
        let bytes = b"synthetic artifact";
        let digest = hex::encode(Sha256::digest(bytes));
        let path = install(&spool, &digest, bytes).unwrap();
        assert_eq!(install(&spool, &digest, bytes).unwrap(), path);
        assert!(read_exact_path(&path, &digest, bytes.len() as u64 + 1).is_err());
        fs::write(&path, b"mismatched bytes!").unwrap();
        assert!(install(&spool, &digest, bytes).is_err());

        let other = b"other synthetic artifact";
        let other_digest = hex::encode(Sha256::digest(other));
        let dir = spool.join("cas").join(&other_digest[..2]);
        ensure_owner_directory(&dir).unwrap();
        let outside = root.path().join("outside");
        fs::write(&outside, other).unwrap();
        symlink(&outside, dir.join(&other_digest)).unwrap();
        assert!(install(&spool, &other_digest, other).is_err());
    }
}
