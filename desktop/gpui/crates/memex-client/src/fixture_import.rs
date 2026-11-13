//! Synthetic file-import backend (`file_import`), same semantics as the
//! placeholder contract: a capability gate, one receipt per `request_id`
//! (an exact replay returns it, another body under the same id conflicts),
//! `status` reads by the original id, and switches for the states the tray
//! must show. It never reads a file: only the request's name, type and size.

use crate::file_import::{FileKind, ImportBackend, ImportPhase, ImportReceipt, ImportRequest, FILE_IMPORT_SCHEMA};
use crate::fixture::FixtureBackend;
use crate::ClientError;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// Switches for the import states.
#[derive(Debug, Clone, Default)]
pub struct ImportControls {
    /// The agent does not advertise the import feature.
    pub capability_missing: bool,
    /// Files whose name contains one of these fail during processing.
    pub fail_names: Vec<String>,
    /// Files whose name contains one of these are accepted, but the submit
    /// reply is lost (a timeout: the outcome is unknown to the client).
    pub lose_receipt_names: Vec<String>,
    /// Slow upload and processing (to show 上传中 / 处理中).
    pub slow: bool,
    /// No upload or processing time at all (tests).
    pub instant: bool,
}

impl ImportControls {
    fn upload(&self) -> Duration {
        if self.instant { Duration::ZERO } else if self.slow { Duration::from_secs(4) } else { Duration::from_millis(150) }
    }

    fn processing(&self) -> Duration {
        if self.instant { Duration::ZERO } else if self.slow { Duration::from_secs(8) } else { Duration::from_millis(900) }
    }
}

struct Job {
    request: ImportRequest,
    accepted: Instant,
    /// Decided once processing ends: (phase, item, reason).
    done: Option<(ImportPhase, Option<String>, Option<String>)>,
}

#[derive(Default)]
struct State {
    controls: ImportControls,
    jobs: HashMap<String, Job>,
    submits: Vec<String>,
    status_calls: usize,
}

#[derive(Clone)]
pub struct FixtureImport {
    state: Arc<Mutex<State>>,
    /// New items go into this synthetic library (so 打开 finds them).
    library: Option<FixtureBackend>,
}

impl FixtureImport {
    pub fn new(library: Option<FixtureBackend>) -> Self {
        Self { state: Default::default(), library }
    }

    pub fn set_controls(&self, controls: ImportControls) {
        self.state.lock().unwrap().controls = controls;
    }

    pub fn controls(&self) -> ImportControls {
        self.state.lock().unwrap().controls.clone()
    }

    /// Every `submit` that reached the backend, in order (request ids).
    pub fn submits(&self) -> Vec<String> {
        self.state.lock().unwrap().submits.clone()
    }

    pub fn status_calls(&self) -> usize {
        self.state.lock().unwrap().status_calls
    }

    fn receipt(request_id: &str, phase: ImportPhase, item_id: Option<String>, reason_code: Option<String>) -> ImportReceipt {
        ImportReceipt { schema_version: FILE_IMPORT_SCHEMA, request_id: request_id.into(), phase, item_id, reason_code }
    }
}

fn matches(name: &str, needles: &[String]) -> bool {
    needles.iter().any(|n| !n.is_empty() && name.contains(n.as_str()))
}

fn item_type(kind: FileKind) -> &'static str {
    match kind {
        FileKind::Pdf => "PDF",
        FileKind::Epub => "书籍",
        FileKind::Markdown | FileKind::Text | FileKind::Html => "文档",
    }
}

impl ImportBackend for FixtureImport {
    fn supported(&self) -> Result<bool, ClientError> {
        Ok(!self.state.lock().unwrap().controls.capability_missing)
    }

    fn submit(&self, request: &ImportRequest) -> Result<ImportReceipt, ClientError> {
        let upload = {
            let mut state = self.state.lock().unwrap();
            state.submits.push(request.request_id.clone());
            if state.controls.capability_missing {
                return Err(ClientError::Unsupported);
            }
            if let Some(job) = state.jobs.get(&request.request_id) {
                // An exact replay returns the first receipt; another body conflicts.
                return if job.request == *request { Ok(Self::receipt(&request.request_id, ImportPhase::Accepted, None, None)) } else { Err(ClientError::Daemon("request_id_conflict".into())) };
            }
            state.controls.upload()
        };
        std::thread::sleep(upload);
        let mut state = self.state.lock().unwrap();
        state.jobs.insert(request.request_id.clone(), Job { request: request.clone(), accepted: Instant::now(), done: None });
        if matches(&request.file_name, &state.controls.lose_receipt_names) {
            return Err(ClientError::Timeout);
        }
        Ok(Self::receipt(&request.request_id, ImportPhase::Accepted, None, None))
    }

    fn status(&self, request_id: &str) -> Result<ImportReceipt, ClientError> {
        let mut state = self.state.lock().unwrap();
        state.status_calls += 1;
        let controls = state.controls.clone();
        let Some(job) = state.jobs.get_mut(request_id) else {
            return Err(ClientError::Daemon("file_import_not_found".into()));
        };
        let processing = controls.processing();
        let elapsed = job.accepted.elapsed();
        if job.done.is_none() && elapsed < processing {
            let phase = if elapsed < processing / 4 { ImportPhase::Accepted } else { ImportPhase::Processing };
            return Ok(Self::receipt(request_id, phase, None, None));
        }
        if job.done.is_none() {
            let request = job.request.clone();
            job.done = Some(if matches(&request.file_name, &controls.fail_names) {
                let code = if request.kind == FileKind::Pdf { "file_import_pdf_no_text" } else { "file_import_unreadable" };
                (ImportPhase::Failed, None, Some(code.to_string()))
            } else {
                let title = std::path::Path::new(&request.file_name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| request.file_name.clone());
                let item = self
                    .library
                    .as_ref()
                    .and_then(|l| l.add_imported_item(&title, item_type(request.kind), request.target_group_id.as_deref()))
                    .unwrap_or_else(|| format!("item_fixture_import_{request_id}"));
                (ImportPhase::Succeeded, Some(item), None)
            });
        }
        let (phase, item, reason) = job.done.clone().expect("decided above");
        Ok(Self::receipt(request_id, phase, item, reason))
    }
}
