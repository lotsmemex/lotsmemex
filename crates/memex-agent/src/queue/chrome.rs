use super::*;
use memex_intake::url_jobs::{DestinationMode, GenerationPolicy, QualityMode, UrlEnqueue, UrlJobBackend, UrlJobCommand, UrlJobError};
use serde_json::{json, Value};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChromeOptions {
    pub include_comments: bool,
    pub quality_check: QualityMode,
    // Omission preserves the exact legacy request digest and stored JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_policy: Option<GenerationPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_mode: Option<DestinationMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_group_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove_from_group_ids: Vec<String>,
}

impl ChromeOptions {
    pub(crate) fn is_podcast(&self, url: &str) -> bool {
        if self.destination_mode.is_some() { memex_intake::url_jobs::podcast::is_episode_url(url) }
        else { self.target_group_id.is_some() }
    }
}

/// Assembled by the CLI/fixture, never a path accepted from an extension payload.
pub struct ChromeIntake {
    database: PathBuf,
    library: PathBuf,
    pub(crate) source_index: std::sync::Arc<std::sync::Mutex<crate::coordinator::SourceIndex>>,
}
impl ChromeIntake {
    pub fn new(database: PathBuf, library: PathBuf) -> Result<Self> {
        Queue::open(&database)?;
        Ok(Self { database, library, source_index: Default::default() })
    }
}
impl UrlJobBackend for ChromeIntake {
    fn features(&self) -> &'static [&'static str] {
        &["url-jobs-no-generation-v1", "url-jobs-enrich-v1", "url-jobs-group-picker-v1", "url-jobs-active-group-v1", "url-jobs-destination-v1"]
    }

    fn execute(
        &self,
        registration: &str,
        command: UrlJobCommand,
    ) -> std::result::Result<Value, UrlJobError> {
        if matches!(&command, UrlJobCommand::Groups) {
            return memex_store::with_consistent_snapshot(&self.library, |root| {
                let groups = memex_store::read_visible_groups(root, memex_core::Sensitivity::Restricted)?;
                let mut nodes: Vec<_> = groups.nodes.iter()
                    .filter(|node| node.owner_id == "owner_local")
                    .map(|node| serde_json::json!({
                        "group_id": node.group_id, "name": node.name, "kind": node.kind,
                        "parent_id": node.parent_id, "root_group_id": node.root_group_id,
                        "writable": !node.read_only && groups.nodes.iter().any(|parent|
                            parent.group_id == node.root_group_id && parent.owner_id == "owner_local" && !parent.read_only)
                    })).collect();
                nodes.sort_by(|a, b| a["group_id"].as_str().cmp(&b["group_id"].as_str()));
                let version = hex::encode(Sha256::digest(serde_json::to_vec(&nodes)?));
                Ok(serde_json::json!({"schema_version": 1, "groups_version": version, "nodes": nodes}))
            }).map_err(|_| UrlJobError::Unavailable);
        }
        let mut queue = Queue::open(&self.database).map_err(|_| UrlJobError::Unavailable)?;
        let id = match command {
            UrlJobCommand::ActiveGroup => return queue.active_group(&self.library).map_err(|_| UrlJobError::Unavailable),
            UrlJobCommand::Groups => unreachable!(),
            UrlJobCommand::Enqueue(request) => {
                queue.chrome_enqueue_checked(registration, &request, || {
                    if request.destination_mode.is_some() || request.target_group_id.is_some() {
                        crate::coordinator::existing_podcast(&self.library, &request.url, request.target_group_id.as_deref(), &request.remove_from_group_ids, request.destination_mode.is_some(), &self.source_index)?;
                    }
                    Ok(())
                }).map(|job| job.job_id)
            },
            UrlJobCommand::Status(id) => queue.authorize_chrome(registration, &id).map(|_| id),
            UrlJobCommand::Retry(id) => queue
                .authorize_chrome(registration, &id)
                .and_then(|_| queue.retry(&id))
                .map(|job| job.job_id),
            UrlJobCommand::Cancel(id) => queue
                .authorize_chrome(registration, &id)
                .and_then(|_| queue.cancel(&id))
                .map(|job| job.job_id),
        }
        .map_err(map_error)?;
        let mut receipt = queue.chrome_receipt(registration, &id).map_err(map_error)?;
        if receipt["state"] == "succeeded" && receipt["quality_check"]["state"] == "pending" {
            let job = queue.record(&id).map_err(map_error)?;
            if let Some(external_id) = job.external_capture_id {
                if let Some(quality) = memex_analysis::capture_quality::capture_quality_result(
                    &self.library,
                    "memex-agent",
                    &external_id,
                )
                .map_err(|_| UrlJobError::Unavailable)?
                {
                    receipt["quality_check"] = quality;
                }
            }
        }
        Ok(receipt)
    }
}

fn map_error(error: anyhow::Error) -> UrlJobError {
    if error.is::<IdentityConflict>() {
        return UrlJobError::IdentityConflict;
    }
    match error.to_string().as_str() {
        "invalid_request" | "library_resource_not_found" | "invalid_podcast_target" => UrlJobError::InvalidRequest,
        "podcast_duplicate_conflict" => UrlJobError::PodcastDuplicateConflict,
        "remote_resource_not_found" => UrlJobError::NotFound,
        "job_not_retryable" => UrlJobError::NotRetryable,
        "job_not_cancellable" => UrlJobError::NotCancellable,
        _ => UrlJobError::Unknown,
    }
}

fn producer(registration: &str) -> Result<String> {
    // Share syntax validation, not ownership, with the mobile principal.
    scoped_producer(registration)?;
    Ok(format!("chrome_{registration}"))
}

impl Queue {
    pub fn chrome_enqueue(&mut self, registration: &str, request: &UrlEnqueue) -> Result<JobView> {
        self.chrome_enqueue_checked(registration, request, || Ok(()))
    }

    fn chrome_enqueue_checked(&mut self, registration: &str, request: &UrlEnqueue, admit_new_request: impl FnOnce() -> Result<()>) -> Result<JobView> {
        let producer = producer(registration)?;
        let key = request
            .request_key
            .strip_prefix("chrome_capture_")
            .context("invalid_request")?;
        if request.schema_version != 1
            || (request.generation_policy == Some(GenerationPolicy::None)
                && request.quality_check != QualityMode::Off)
            || key.len() != 32
            || !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            || !request.url.starts_with("https://")
        {
            bail!("invalid_request");
        }
        let canonical = crate::url_input::extract_url(&request.url)
            .map_err(|_| anyhow::anyhow!("invalid_request"))?
            .0;
        if canonical != request.url {
            bail!("invalid_request");
        }
        if let Some(mode) = request.destination_mode {
            anyhow::ensure!(request.generation_policy == Some(GenerationPolicy::None) && request.quality_check == QualityMode::Off && !request.include_comments, "invalid_request");
            match mode {
                DestinationMode::Inbox => anyhow::ensure!(request.target_group_id.is_none() && request.remove_from_group_ids.is_empty(), "invalid_request"),
                DestinationMode::SelectedGroup => anyhow::ensure!(request.target_group_id.is_some(), "invalid_request"),
            }
        }
        if request.target_group_id.is_some() {
            let url = url::Url::parse(&request.url)?;
            if request.generation_policy != Some(GenerationPolicy::None)
                || request.include_comments
                || (request.destination_mode.is_none() && url.host_str() != Some("ampcode.com"))
                || request.remove_from_group_ids.len() > 2
                || request.target_group_id.as_ref().is_some_and(|target| request.remove_from_group_ids.contains(target))
                || request.source_hints.as_ref().is_some_and(|v| v.to_string().len() > 16_384)
                || request.target_group_id.iter().chain(request.remove_from_group_ids.iter())
                    .any(|id| !id.starts_with("group_") || id.len() > 128
                        || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
            { bail!("invalid_request"); }
        } else if !request.remove_from_group_ids.is_empty() || request.source_hints.is_some() {
            bail!("invalid_request");
        }
        let mut remove_from_group_ids = request.remove_from_group_ids.clone();
        remove_from_group_ids.sort();
        remove_from_group_ids.dedup();
        self.enqueue_options(
            &producer,
            &request.request_key,
            &request.url,
            Some(&ChromeOptions {
                include_comments: request.include_comments,
                quality_check: request.quality_check,
                generation_policy: request.generation_policy,
                destination_mode: request.destination_mode,
                target_group_id: request.target_group_id.clone(),
                remove_from_group_ids,
            }),
            admit_new_request,
        )
    }

    fn authorize_chrome(&self, registration: &str, id: &str) -> Result<()> {
        self.conn
            .query_row(
                "SELECT 1 FROM jobs WHERE job_id=?1 AND producer=?2 AND chrome_options IS NOT NULL",
                params![id, producer(registration)?],
                |_| Ok(()),
            )
            .optional()?
            .context("remote_resource_not_found")
    }

    pub(crate) fn chrome_options(&self, id: &str) -> Result<Option<ChromeOptions>> {
        let value: Option<String> = self.conn.query_row(
            "SELECT chrome_options FROM jobs WHERE job_id=?1",
            [id],
            |row| row.get(0),
        )?;
        value
            .map(|value| serde_json::from_str(&value).map_err(Into::into))
            .transpose()
    }

    pub(crate) fn episode_work<T: serde::de::DeserializeOwned>(&self, id: &str) -> Result<Option<T>> {
        let value: Option<String> = self.conn.query_row(
            "SELECT payload FROM chrome_episode_work WHERE job_id=?1", [id], |r| r.get(0)
        ).optional()?;
        value.map(|v| serde_json::from_str(&v).map_err(Into::into)).transpose()
    }

    pub(crate) fn save_episode_work(&mut self, job: &JobRecord, value: &impl Serialize) -> Result<()> {
        let encoded = serde_json::to_string(value)?;
        let changed = self.conn.execute("INSERT INTO chrome_episode_work(job_id,payload) SELECT job_id,?2 FROM jobs WHERE job_id=?1 AND state='leased' AND lease_owner=?3 AND lease_until>?4 ON CONFLICT(job_id) DO UPDATE SET payload=excluded.payload",
            params![job.view.job_id,encoded,job.lease_owner,Utc::now().timestamp()])?;
        anyhow::ensure!(changed == 1, "lease_lost");
        self.secure()
    }

    pub(crate) fn renew_episode(&mut self, job: &JobRecord) -> Result<()> {
        let now = Utc::now().timestamp();
        let changed = self.conn.execute("UPDATE jobs SET lease_until=?4 WHERE job_id=?1 AND state='leased' AND lease_owner=?2 AND lease_until>?3",
            params![job.view.job_id,job.lease_owner,now,now+240])?;
        anyhow::ensure!(changed == 1, "lease_lost");
        Ok(())
    }

    /// Serializes the last identity check and canonical commit across request
    /// keys/registrations on this queue. No network or generation in the closure.
    pub(crate) fn episode_commit<T>(&mut self, job: &JobRecord, commit: impl FnOnce() -> Result<T>) -> Result<T> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let owned: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM jobs WHERE job_id=?1 AND state='leased' AND lease_owner=?2 AND lease_until>?3)",
            params![job.view.job_id,job.lease_owner,Utc::now().timestamp()], |r| r.get(0))?;
        anyhow::ensure!(owned, "lease_lost");
        let result = commit()?;
        tx.commit()?;
        Ok(result)
    }

    pub(crate) fn set_chrome_result(&mut self, job: &JobRecord, result: &Value) -> Result<()> {
        let changed = self.conn.execute("UPDATE jobs SET chrome_result=?2 WHERE job_id=?1 AND state='leased' AND lease_owner=?3 AND chrome_options IS NOT NULL",
            params![job.view.job_id, serde_json::to_string(result)?, job.lease_owner])?;
        if changed != 1 {
            bail!("lease_lost");
        }
        self.secure()
    }

    pub(crate) fn reserve_capture_hop(
        &mut self,
        job: &JobRecord,
        origin: &str,
        first: bool,
        now: i64,
    ) -> Result<Option<i64>> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let owned: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM jobs WHERE job_id=?1 AND state='leased' AND lease_owner=?2 AND lease_until>?3)", params![job.view.job_id,job.lease_owner,now], |row| row.get(0))?;
        if !owned {
            bail!("lease_lost")
        }
        // First origin was reserved atomically with this job's lease.
        if first {
            return Ok(None);
        }
        let active: Option<i64> = tx.query_row("SELECT j.lease_until FROM capture_origin_leases l JOIN jobs j ON j.job_id=l.job_id WHERE l.origin=?1 AND j.job_id<>?2 AND j.state='leased' AND j.lease_until>?3", params![origin,job.view.job_id,now], |row| row.get(0)).optional()?;
        let next: Option<i64> = tx
            .query_row(
                "SELECT next_fetch_at FROM capture_origins WHERE origin=?1",
                [origin],
                |row| row.get(0),
            )
            .optional()?;
        let until = active.unwrap_or(0).max(next.unwrap_or(0));
        if until > now {
            return Ok(Some(until));
        }
        let delay = 8 + i64::from(uuid::Uuid::now_v7().as_bytes()[15] % 18);
        tx.execute("INSERT INTO capture_origins(origin,next_fetch_at) VALUES(?1,?2) ON CONFLICT(origin) DO UPDATE SET next_fetch_at=MAX(next_fetch_at,excluded.next_fetch_at)", params![origin,now.saturating_add(delay)])?;
        tx.execute("INSERT INTO capture_origin_leases(origin,job_id) VALUES(?1,?2) ON CONFLICT(origin) DO UPDATE SET job_id=excluded.job_id", params![origin,job.view.job_id])?;
        tx.commit()?;
        self.secure()?;
        Ok(None)
    }

    pub(crate) fn defer_capture_origin(&mut self, origin: &str, not_before: i64) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("INSERT INTO capture_origins(origin,next_fetch_at) VALUES(?1,?2) ON CONFLICT(origin) DO UPDATE SET next_fetch_at=MAX(next_fetch_at,excluded.next_fetch_at)", params![origin,not_before])?;
        tx.execute("UPDATE jobs SET next_attempt_at=MAX(COALESCE(next_attempt_at,0),?2) WHERE fetch_origin=?1 AND artifact_digest IS NULL AND state IN ('queued','retry_wait')", params![origin,not_before])?;
        tx.commit()?;
        self.secure()
    }

    pub fn chrome_receipt(&self, registration: &str, id: &str) -> Result<Value> {
        self.authorize_chrome(registration, id)?;
        let job = self.record(id)?;
        let options = self.chrome_options(id)?.context("invalid_request")?;
        let (next, result): (Option<i64>, Option<String>) = self.conn.query_row(
            "SELECT next_attempt_at,chrome_result FROM jobs WHERE job_id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let result: Option<Value> = result.map(|text| serde_json::from_str(&text)).transpose()?;
        let terminal = matches!(
            job.view.state,
            State::Succeeded | State::Failed | State::Cancelled | State::NeedsUser
        );
        let comments = result.as_ref().and_then(|value| value.get("comments")).cloned().unwrap_or_else(|| json!({
            "state": if !options.include_comments { "not_requested" } else if terminal { "unavailable" } else { "pending" },
            "reason": null,
        }));
        let mut quality = result.as_ref().and_then(|value| value.get("quality_check")).cloned().unwrap_or_else(|| json!({
            "state": if options.quality_check == QualityMode::Off { "not_requested" } else if matches!(job.view.state, State::Failed | State::Cancelled) { "unavailable" } else { "pending" },
            "reason": if options.quality_check == QualityMode::Off { None } else if matches!(job.view.state, State::Failed | State::Cancelled) { Some("capture_not_committed") } else { Some("awaiting_capture") },
            "analysis_id": null,
        }));
        if quality["state"] == "running" && job.view.state != State::Leased {
            let stopped = matches!(job.view.state, State::Failed | State::Cancelled);
            quality["state"] = if stopped { "unavailable" } else { "pending" }.into();
            quality["reason"] = if stopped {
                "capture_not_committed"
            } else {
                "awaiting_capture"
            }
            .into();
        }
        let idle = job.lease_owner.is_none();
        let mut receipt = json!({
            "schema_version": 1, "job_id": id, "state": job.view.state, "stage":job.view.stage,
            "reason":job.view.reason_code, "item_id":job.view.item_id, "revision_id":job.result_revision_id,
            "retry_at_ms": if matches!(job.view.state, State::RetryWait | State::Queued) { next.filter(|n| *n > 0).and_then(|n| n.checked_mul(1000)) } else { None },
            "can_retry":idle && (matches!(job.view.state, State::Failed | State::RetryWait) || job.view.state == State::NeedsUser && job.artifact_digest.is_none()),
            "can_cancel":idle && matches!(job.view.state, State::Queued | State::ArtifactReady | State::NeedsUser | State::RetryWait | State::Failed),
            "comments":comments, "quality_check":quality,
        });
        if options.is_podcast(&job.url) {
            receipt["enrichment"] = result.as_ref().and_then(|r| r.get("enrichment")).cloned()
                .unwrap_or_else(|| json!({"state":"pending","archived":false}));
        }
        Ok(receipt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chrome_groups_filters_owner_and_root_write_access_without_queue_writes() {
        let temp = tempfile::tempdir().unwrap();
        let library = temp.path().join("Library");
        let database = temp.path().join("agent.db");
        std::fs::create_dir_all(&library).unwrap();
        let root = memex_store::edit_groups(&library, "picker-root", memex_store::GroupEditOp::CreateRoot {
            name: "Synthetic".into(), sensitivity: memex_core::Sensitivity::Restricted,
        }, memex_core::Sensitivity::Restricted).unwrap().group_id;
        let path = library.join("catalog/groups/state.json");
        let mut state: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let original = state["nodes"].as_array().unwrap().iter().find(|n| n["group_id"] == root).unwrap().clone();
        let mut foreign = original.clone();
        let foreign_id = format!("group_{}", "b".repeat(32));
        foreign["group_id"] = foreign_id.clone().into();
        foreign["root_group_id"] = foreign_id.clone().into();
        foreign["owner_id"] = "foreign_owner".into();
        state["nodes"].as_array_mut().unwrap().push(foreign);
        std::fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
        let backend = ChromeIntake::new(database.clone(), library).unwrap();
        let before = backend.execute("owner", UrlJobCommand::Groups).unwrap();
        assert!(before["nodes"].as_array().unwrap().iter().all(|n| n["group_id"] != foreign_id));
        assert_eq!(before["nodes"].as_array().unwrap().iter().find(|n| n["group_id"] == root).unwrap()["writable"], true);
        for node in state["nodes"].as_array_mut().unwrap() {
            if node["group_id"] == root { node["read_only"] = true.into(); }
        }
        std::fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
        let after = backend.execute("owner", UrlJobCommand::Groups).unwrap();
        assert_ne!(before["groups_version"], after["groups_version"]);
        assert_eq!(after["nodes"].as_array().unwrap().iter().find(|n| n["group_id"] == root).unwrap()["writable"], false);
        assert_eq!(rusqlite::Connection::open(database).unwrap().query_row("SELECT count(*) FROM jobs", [], |r| r.get::<_,u64>(0)).unwrap(), 0);
        let conflict = map_error(anyhow::anyhow!("podcast_duplicate_conflict"));
        assert_eq!(conflict.status(), 409);
        assert_eq!(conflict.code(), "podcast_duplicate_conflict");
    }

    fn request(key: char, url: &str) -> UrlEnqueue {
        UrlEnqueue {
            schema_version: 1,
            request_key: format!("chrome_capture_{}", key.to_string().repeat(32)),
            url: url.into(),
            include_comments: true,
            quality_check: QualityMode::Off,
            generation_policy: None,
            destination_mode: None,
            target_group_id: None,
            remove_from_group_ids: vec![],
            source_hints: None,
        }
    }

    #[test]
    fn slow_admission_does_not_block_worker_and_concurrent_acceptance_wins() {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("agent.db");
        let mut worker = Queue::open(&database).unwrap();
        let mut intake = Queue::open(&database).unwrap();
        let (entered, ready) = std::sync::mpsc::channel();
        let (resume, blocked) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            intake.chrome_enqueue_checked(&"a".repeat(64), &request('S', "https://example.org/slow"), || {
                entered.send(()).unwrap();
                blocked.recv().unwrap();
                anyhow::bail!("invalid_podcast_target")
            })
        });
        ready.recv().unwrap();
        let recovery = worker.recover_leases();
        let accepted = if recovery.is_ok() {
            Some(worker.chrome_enqueue(&"a".repeat(64), &request('S', "https://example.org/slow")).unwrap())
        } else { None };
        resume.send(()).unwrap();
        let replay = thread.join().unwrap();
        assert!(recovery.is_ok(), "slow admission blocked worker: {recovery:?}");
        assert_eq!(replay.unwrap().job_id, accepted.unwrap().job_id);
    }

    #[test]
    fn chrome_replay_is_durable_atomic_options_bound_and_registration_owned() {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("agent.db");
        Queue::open(&database).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let database = database.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let library = database.parent().unwrap().join("Library");
                    let backend = ChromeIntake::new(database, library).unwrap();
                    barrier.wait();
                    backend
                        .execute(
                            &"a".repeat(64),
                            UrlJobCommand::Enqueue(request('A', "https://example.invalid/article")),
                        )
                        .unwrap()
                })
            })
            .collect();
        let values: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert_eq!(values[0]["job_id"], values[1]["job_id"]);
        let id = values[0]["job_id"].as_str().unwrap().to_owned();
        let reopened = ChromeIntake::new(database.clone(), root.path().join("Library")).unwrap();
        assert_eq!(
            reopened
                .execute(
                    &"a".repeat(64),
                    UrlJobCommand::Enqueue(request('A', "https://example.invalid/article"))
                )
                .unwrap()["job_id"],
            id
        );
        let stored = Queue::open(&database).unwrap().chrome_options(&id).unwrap().unwrap();
        assert_eq!(serde_json::to_string(&stored).unwrap(), r#"{"include_comments":true,"quality_check":"off"}"#);
        let mut contradictory = request('Z', "https://example.invalid/article");
        contradictory.generation_policy = Some(GenerationPolicy::None);
        contradictory.quality_check = QualityMode::Configured;
        assert!(matches!(reopened.execute(&"a".repeat(64), UrlJobCommand::Enqueue(contradictory)), Err(UrlJobError::InvalidRequest)));
        for change in 0..4 {
            let mut changed = request('A', "https://example.invalid/article");
            match change {
                0 => changed.url.push_str("-other"),
                1 => changed.include_comments = false,
                2 => changed.quality_check = QualityMode::Configured,
                _ => changed.generation_policy = Some(GenerationPolicy::None),
            }
            assert!(matches!(
                reopened.execute(&"a".repeat(64), UrlJobCommand::Enqueue(changed)),
                Err(UrlJobError::IdentityConflict)
            ));
        }
        for command in [
            UrlJobCommand::Status(id.clone()),
            UrlJobCommand::Retry(id.clone()),
            UrlJobCommand::Cancel(id.clone()),
            UrlJobCommand::Enqueue(request('A', "https://example.invalid/article")),
        ] {
            assert!(matches!(
                reopened.execute(&"b".repeat(64), command),
                Err(UrlJobError::NotFound)
            ));
        }
        let mut queue = Queue::open(&database).unwrap();
        for producer in [
            "remote_iphone",
            "mobile_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ] {
            let old = queue
                .enqueue(producer, "legacy-job", "https://example.invalid/old")
                .unwrap();
            for command in [
                UrlJobCommand::Status(old.job_id.clone()),
                UrlJobCommand::Retry(old.job_id.clone()),
                UrlJobCommand::Cancel(old.job_id),
            ] {
                assert!(matches!(
                    reopened.execute(&"a".repeat(64), command),
                    Err(UrlJobError::NotFound)
                ));
            }
        }
        assert!(matches!(
            reopened.execute(&"a".repeat(64), UrlJobCommand::Retry(id.clone())),
            Err(UrlJobError::NotRetryable)
        ));
        let cancelled = reopened
            .execute(&"a".repeat(64), UrlJobCommand::Cancel(id.clone()))
            .unwrap();
        assert_eq!(cancelled["state"], "cancelled");
        assert_eq!(cancelled["can_retry"], false);
        assert!(matches!(
            reopened.execute(&"a".repeat(64), UrlJobCommand::Cancel(id)),
            Err(UrlJobError::NotCancellable)
        ));
    }

    #[test]
    fn episode_cross_request_concurrency_and_exact_options() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("agent.db");
        Queue::open(&path).unwrap();
        let episode_request = |key| {
            let mut input = request(key, "https://ampcode.com/podcast/episode-1");
            input.include_comments = false;
            input.generation_policy = Some(GenerationPolicy::None);
            input.target_group_id = Some(format!("group_{}", "1".repeat(32)));
            input.remove_from_group_ids = vec![format!("group_{}", "2".repeat(32))];
            input
        };
        let original = episode_request('A');
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let handles: Vec<_> = ['A', 'B', 'C'].into_iter().map(|key| {
            let path = path.clone();
            let barrier = barrier.clone();
            let input = episode_request(key);
            std::thread::spawn(move || {
                let mut queue = Queue::open(&path).unwrap();
                barrier.wait();
                queue.chrome_enqueue(&"a".repeat(64), &input).unwrap().job_id
            })
        }).collect();
        let ids: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert!(ids.iter().all(|id| id == &ids[0]));
        let mut queue = Queue::open(&path).unwrap();
        assert_eq!(queue.chrome_enqueue(&"a".repeat(64), &original).unwrap().job_id, ids[0]);
        let mut changed = episode_request('D');
        changed.remove_from_group_ids.clear();
        assert!(queue.chrome_enqueue(&"a".repeat(64), &changed).unwrap_err().is::<IdentityConflict>());
        let mut invalid = episode_request('E');
        invalid.remove_from_group_ids = vec![original.remove_from_group_ids[0].clone(); 3];
        assert_eq!(queue.chrome_enqueue(&"a".repeat(64), &invalid).unwrap_err().to_string(), "invalid_request");
        invalid.remove_from_group_ids = vec![original.target_group_id.clone().unwrap()];
        assert_eq!(queue.chrome_enqueue(&"a".repeat(64), &invalid).unwrap_err().to_string(), "invalid_request");
        assert_eq!(queue.conn.query_row("SELECT count(*) FROM jobs", [], |r| r.get::<_, u64>(0)).unwrap(), 1);
    }

    #[test]
    fn changed_episode_request_after_failure_starts_a_new_job_but_not_while_live_or_after_success() {
        let root = tempfile::tempdir().unwrap();
        let mut queue = Queue::open(&root.path().join("agent.db")).unwrap();
        let owner = "a".repeat(64);
        let episode_request = |key, group: char| {
            let mut input = request(key, "https://ampcode.com/podcast/episode-1");
            input.include_comments = false;
            input.generation_policy = Some(GenerationPolicy::None);
            input.target_group_id = Some(format!("group_{}", group.to_string().repeat(32)));
            input
        };
        let set_state = |queue: &Queue, id: &str, state: &str| {
            queue.conn.execute("UPDATE jobs SET state=?2 WHERE job_id=?1", params![id, state]).unwrap();
        };
        let first = queue.chrome_enqueue(&owner, &episode_request('A', '1')).unwrap().job_id;
        // Live: a changed request still conflicts.
        assert!(queue.chrome_enqueue(&owner, &episode_request('B', '2')).unwrap_err().is::<IdentityConflict>());
        set_state(&queue, &first, "failed");
        let second = queue.chrome_enqueue(&owner, &episode_request('B', '2')).unwrap().job_id;
        assert_ne!(second, first);
        // Both keys keep answering with their own job; the old failed job is untouched.
        assert_eq!(queue.chrome_enqueue(&owner, &episode_request('A', '1')).unwrap().job_id, first);
        assert_eq!(queue.chrome_enqueue(&owner, &episode_request('B', '2')).unwrap().job_id, second);
        assert_eq!(queue.conn.query_row("SELECT state FROM jobs WHERE job_id=?1", [&first], |r| r.get::<_, String>(0)).unwrap(), "failed");
        // The new job owns the episode slot: an unchanged third key joins it, a changed one conflicts while it is live.
        assert_eq!(queue.chrome_enqueue(&owner, &episode_request('C', '2')).unwrap().job_id, second);
        assert!(queue.chrome_enqueue(&owner, &episode_request('D', '3')).unwrap_err().is::<IdentityConflict>());
        // Success also keeps the conflict.
        set_state(&queue, &second, "succeeded");
        assert!(queue.chrome_enqueue(&owner, &episode_request('D', '3')).unwrap_err().is::<IdentityConflict>());
        // Cancelled counts as ended.
        set_state(&queue, &second, "cancelled");
        let third = queue.chrome_enqueue(&owner, &episode_request('D', '3')).unwrap().job_id;
        assert!(third != first && third != second);
        assert_eq!(queue.conn.query_row("SELECT count(*) FROM jobs", [], |r| r.get::<_, u64>(0)).unwrap(), 3);
    }

    #[test]
    fn redirect_origins_keep_spacing_and_exclusive_leases_after_reopen() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("agent.db");
        let owner = "a".repeat(64);
        let now = Utc::now().timestamp();
        let mut queue = Queue::open(&path).unwrap();
        queue
            .chrome_enqueue(&owner, &request('A', "https://a.example.invalid/one"))
            .unwrap();
        let first = queue.lease_at(60, now).unwrap().unwrap();
        assert_eq!(
            queue
                .reserve_capture_hop(&first, "https://b.example.invalid", false, now)
                .unwrap(),
            None
        );
        let next = queue
            .reserve_capture_hop(&first, "https://b.example.invalid", false, now)
            .unwrap()
            .unwrap();
        assert!((now + 8..=now + 25).contains(&next));
        queue
            .chrome_enqueue(&owner, &request('B', "https://b.example.invalid/two"))
            .unwrap();
        drop(queue);
        let mut queue = Queue::open(&path).unwrap();
        assert!(
            queue.lease_at(60, next).unwrap().is_none(),
            "redirect destination is still leased, not merely spaced"
        );
        assert_eq!(
            queue
                .reserve_capture_hop(&first, "https://b.example.invalid", false, next - 1)
                .unwrap(),
            Some(next)
        );
        assert_eq!(
            queue
                .reserve_capture_hop(&first, "https://b.example.invalid", false, next)
                .unwrap(),
            None
        );
        queue
            .finish(
                &first,
                State::Failed,
                Stage::Fetch,
                Some("synthetic_stop"),
                0,
            )
            .unwrap();
        assert!(queue.lease_at(60, next + 25).unwrap().is_some());
    }

    #[test]
    fn chrome_origin_backoff_survives_manual_retry_reopen_and_active_leases() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("agent.db");
        let owner = "a".repeat(64);
        let mut queue = Queue::open(&path).unwrap();
        let now = Utc::now().timestamp();
        let one = queue
            .chrome_enqueue(&owner, &request('A', "https://a.example.invalid/one"))
            .unwrap();
        queue
            .chrome_enqueue(&owner, &request('B', "https://a.example.invalid/two"))
            .unwrap();
        let first = queue.lease_at(60, now).unwrap().unwrap();
        assert_eq!(first.view.job_id, one.job_id);
        assert!(
            queue.lease_at(60, now + 30).unwrap().is_none(),
            "same-origin active job forbids a second lease even after spacing"
        );
        queue
            .defer_capture_origin("https://a.example.invalid", now + 3600)
            .unwrap();
        queue
            .finish(
                &first,
                State::RetryWait,
                Stage::Fetch,
                Some("rate_limited"),
                3600,
            )
            .unwrap();
        queue.retry(&one.job_id).unwrap();
        drop(queue);
        let mut queue = Queue::open(&path).unwrap();
        let receipt = queue.chrome_receipt(&owner, &one.job_id).unwrap();
        assert_eq!(receipt["retry_at_ms"], (now + 3600) * 1000);
        assert!(queue.lease_at(60, now + 3599).unwrap().is_none());
        assert!(queue.lease_at(60, now + 3600).unwrap().is_some());
    }
}
