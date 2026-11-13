//! Single-flight index/mutation owner with a separate bounded Store reader lane.
use super::*;
use std::sync::{mpsc, Arc, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};

const MAX_STORE_READS: usize = 4;

struct StoreReadPermit(Arc<AtomicUsize>);

impl Drop for StoreReadPermit {
    fn drop(&mut self) { self.0.fetch_sub(1, Ordering::AcqRel); }
}

// Positive allowlist: queue-backed status/preview operations may mutate jobs or
// quotes, and must never acquire independent admission alongside writes.
pub(super) fn is_store_read(request: &ReaderRequest) -> bool {
    matches!(request, ReaderRequest::Toc { .. } | ReaderRequest::Chunk { .. }
        | ReaderRequest::Alignment { .. } | ReaderRequest::TranslateMetadata { .. }
        | ReaderRequest::TranslateChunk { .. } | ReaderRequest::TranslateAlignment { .. }
        | ReaderRequest::RevisionChunk { .. } | ReaderRequest::State { .. } | ReaderRequest::Resume
        | ReaderRequest::PassesList { .. } | ReaderRequest::PassesStats { .. }
        | ReaderRequest::AnnotationsList { annotation_space: Some(_), .. })
}

pub(super) enum QueryAccess<'a> {
    Inline(&'a mut memex_library::LibraryQuery),
    Background(&'a mut BackgroundQueries),
}

impl<'a> From<&'a mut memex_library::LibraryQuery> for QueryAccess<'a> {
    fn from(query: &'a mut memex_library::LibraryQuery) -> Self { Self::Inline(query) }
}
impl<'a> From<&'a mut BackgroundQueries> for QueryAccess<'a> {
    fn from(query: &'a mut BackgroundQueries) -> Self { Self::Background(query) }
}

pub(super) struct BackgroundQueries {
    library: PathBuf,
    idle: Arc<Mutex<Option<memex_library::LibraryQuery>>>,
    store_reads: Arc<AtomicUsize>,
    pub(super) translation_generation: fn(&Path, &memex_models::Snapshot) -> Result<u64>,
    pub(super) format_generation: fn(&Path, &memex_models::Snapshot) -> Result<u64>,
}

impl BackgroundQueries {
    pub(super) fn new(library: PathBuf) -> Result<Self> {
        Ok(Self {
            idle: Arc::new(Mutex::new(Some(memex_library::LibraryQuery::new(&library)?))),
            store_reads: Arc::new(AtomicUsize::new(0)),
            library,
            translation_generation: crate::queue::reader_translation::credential_generation,
            format_generation: crate::queue::content_format::credential_generation,
        })
    }

    pub(super) fn serve(&mut self, mut stream: UnixStream, command: Command, database: &Path) -> Result<()> {
        if matches!(&command, Command::Reader { request, .. } if is_store_read(request))
            || matches!(&command, Command::ItemCoverBatch { .. } | Command::ItemCoverChunk { .. }) {
            return self.serve_store_read(stream, command);
        }
        let query = self.idle.lock().map_err(|_| anyhow::anyhow!("library_query_failed"))?.take();
        let Some(mut query) = query else {
            // No mutation has been admitted. Never queue duplicate rebuilds or writes.
            return write_response_frame(&mut stream, &response_body(&Response::error("library_loading"))?);
        };
        let read_only = !matches!(&command, Command::GroupEdit { .. } | Command::MarkdownEdit { .. } | Command::ReadingAnnotationsSave { .. }
            | Command::GroupDelete { .. } | Command::GroupDeleteRestore { .. }
            | Command::Reader { request: ReaderRequest::Save { .. } | ReaderRequest::AlignRequest { .. }
                | ReaderRequest::PassesEdit { .. } | ReaderRequest::PassesTimingStart { .. } | ReaderRequest::PassesTimingReport { .. }
                | ReaderRequest::TranslateRequest { .. }
                | ReaderRequest::FormatRequest { .. } | ReaderRequest::FormatRestore { .. } | ReaderRequest::FormatResume { .. }
                | ReaderRequest::EditBegin { .. } | ReaderRequest::EditChunk { .. } | ReaderRequest::EditCommit { .. }, .. });
        let idle = self.idle.clone();
        let library = self.library.clone();
        let database = database.to_owned();
        let generation = self.translation_generation;
        let format_generation = self.format_generation;
        let (send, receive) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let operation = diagnostics::Operation::for_command(&command);
            let result = if matches!(&command, Command::Reader { request: ReaderRequest::AlignRequest { .. } | ReaderRequest::AlignStatus { .. } | ReaderRequest::EditCommit { .. }
                | ReaderRequest::FormatPreview(_) | ReaderRequest::FormatTargets { .. } | ReaderRequest::FormatRequest { .. }
                | ReaderRequest::FormatStatus { .. } | ReaderRequest::FormatResults { .. } | ReaderRequest::FormatChanges(_)
                | ReaderRequest::FormatRestore { .. } | ReaderRequest::FormatResume { .. }
                | ReaderRequest::TranslatePreview { .. } | ReaderRequest::TranslateRequest { .. } | ReaderRequest::TranslateStatus { .. }, .. }) {
                Queue::open(&database).and_then(|mut q| reader::dispatch_job(&mut q, &library, command, generation, format_generation))
            } else {
                dispatch_query(&library, &mut query, command)
            }
                .and_then(|result| result.map_err(|_| anyhow::anyhow!("invalid_request")));
            let response = result.unwrap_or_else(|e| diagnostics::error_response_for_operation(&library, operation, &e));
            // Reuse the fully synchronized index, not the previous request's response.
            // Every new request still performs its own freshness and ceiling checks.
            if let Ok(mut slot) = idle.lock() { *slot = Some(query); }
            let _ = send.send(response);
        });
        std::thread::spawn(move || {
            let response = match receive.recv_timeout(Duration::from_secs(3)) {
                Ok(response) => response,
                Err(mpsc::RecvTimeoutError::Timeout) if read_only => Response::error("library_loading"),
                // A write may have committed: close without a success/rejection so
                // clients retain their replay identity and treat the outcome as unknown.
                Err(_) => return,
            };
            if let Ok(body) = response_body(&response) {
                let _ = write_response_frame(&mut stream, &body);
            }
        });
        Ok(())
    }

    fn serve_store_read(&self, mut stream: UnixStream, command: Command) -> Result<()> {
        if self.store_reads.fetch_update(Ordering::AcqRel, Ordering::Acquire,
            |active| (active < MAX_STORE_READS).then_some(active + 1)).is_err() {
            return write_response_frame(&mut stream, &response_body(&Response::error("library_loading"))?);
        }
        // Hold admission until BOTH Store work and response delivery finish. A
        // timeout does not cancel Store work or allow another worker to replace it.
        let permit = Arc::new(StoreReadPermit(self.store_reads.clone()));
        let worker_permit = permit.clone();
        let library = self.library.clone();
        let (send, receive) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let _permit = worker_permit;
            let operation = diagnostics::Operation::for_command(&command);
            let result = match command {
                Command::Reader { request, max_sensitivity, principal } =>
                    reader::dispatch(&library, request, max_sensitivity, principal),
                command => dispatch_cover(&library, command),
            };
            let response = result.unwrap_or_else(|e| diagnostics::error_response_for_operation(&library, operation, &e));
            let _ = send.send(response);
        });
        std::thread::spawn(move || {
            let _permit = permit;
            let response = match receive.recv_timeout(Duration::from_secs(3)) {
                Ok(response) => response,
                Err(mpsc::RecvTimeoutError::Timeout) => Response::error("library_loading"),
                Err(_) => return,
            };
            if let Ok(body) = response_body(&response) {
                let _ = write_response_frame(&mut stream, &body);
            }
        });
        Ok(())
    }
}

pub(super) fn dispatch_query(
    library: &Path,
    queries: &mut memex_library::LibraryQuery,
    command: Command,
) -> Result<std::result::Result<Response, Command>> {
    match command {
        Command::Reader { max_sensitivity, request, principal } => Ok(Ok(reader::dispatch(library, request, max_sensitivity, principal)?)),
        Command::MarkdownEdit { max_sensitivity, edit } => {
            let mut response = Response::empty_ok();
            response.item_update = Some(memex_store::edit_markdown(library, edit, max_sensitivity)?);
            Ok(Ok(response))
        }
        Command::ReadingAnnotationsRead { item_id, revision_id, max_sensitivity } => {
            let mut response = Response::empty_ok();
            response.reading_annotations = Some(memex_store::read_annotations(library, &item_id, &revision_id, max_sensitivity)?);
            Ok(Ok(response))
        }
        Command::ReadingAnnotationsSave { expected_version, annotations, max_sensitivity } => {
            let mut response = Response::empty_ok();
            response.reading_annotations = Some(memex_store::save_annotations(library, &expected_version, annotations, max_sensitivity)?);
            Ok(Ok(response))
        }
        Command::LibraryQuery(request) => {
            let mut page = queries.query(&request)?;
            crate::translation_request::fill_translated_titles(library, &mut page);
            let mut response = Response::empty_ok();
            response.library_query = Some(page);
            Ok(Ok(response))
        }
        command => crate::groups::dispatch(library, queries, command),
    }
}

#[cfg(test)]
#[path = "../../examples/fixtures/reader_book.rs"]
mod fixture;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn start(queries: &mut BackgroundQueries, request: serde_json::Value) -> UnixStream {
        let command = serde_json::from_value(json!({"command":"reader",
            "max_sensitivity":"public", "request":request})).unwrap();
        let (server, client) = UnixStream::pair().unwrap();
        client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let database = queries.library.join("must-not-open.db");
        queries.serve(server, command, &database).unwrap();
        client
    }

    fn response(mut client: UnixStream) -> Response {
        serde_json::from_slice(&read_response_frame(&mut client).unwrap()).unwrap()
    }

    fn drained(queries: &BackgroundQueries) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while queries.store_reads.load(Ordering::Acquire) != 0 {
            assert!(std::time::Instant::now() < deadline, "Store readers did not release admission");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn background_reader_internal_error_records_only_fixed_operation_and_category() {
        let temp = tempfile::tempdir_in("/private/tmp").unwrap();
        let library = temp.path().join("Library");
        fixture::seed(&library).unwrap();
        let mut queries = BackgroundQueries::new(library.clone()).unwrap();
        fs::write(library.join("catalog/reader-state.json"), b"PRIVATE-BODY-CANARY").unwrap();
        let result = response(start(&mut queries, json!({"op":"resume"})));
        assert_eq!(result.reason_code.as_deref(), Some("internal_error"));
        drained(&queries);
        let log = fs::read_to_string(library.join("ipc-diagnostics.log")).unwrap();
        assert!(log.contains("op=reader_resume category=json_syntax"));
        assert!(!log.contains("PRIVATE-BODY-CANARY"));
        assert!(!log.contains(temp.path().to_str().unwrap()));
    }

    #[test]
    fn store_reads_succeed_with_index_slot_occupied_and_keep_validation() {
        let temp = tempfile::tempdir().unwrap();
        let library = temp.path().join("Library");
        let doc = fixture::seed(&library).unwrap();
        let mut queries = BackgroundQueries::new(library.clone()).unwrap();
        // Deterministically model an in-flight cold index, without holding the
        // Store lock too (which would legitimately block both lanes).
        let index = queries.idle.lock().unwrap().take().unwrap();
        for op in ["toc", "chunk", "alignment", "translate_metadata", "revision_chunk", "state", "resume"] {
            let request = match op {
                "resume" => json!({"op":op}),
                "state" => json!({"op":op,"item_id":doc.item_id}),
                "toc" => json!({"op":op,"item_id":doc.item_id,"revision_id":doc.revision_id}),
                "translate_metadata" => json!({"op":op,"item_id":doc.item_id,"revision_id":doc.revision_id,"chapter":"appendix"}),
                "revision_chunk" => json!({"op":op,"item_id":doc.item_id,"revision_id":doc.revision_id,
                    "target":{"kind":"chapter","side":"original","chapter":"chapter-1"},
                    "start_byte":0,"limit_bytes":64,"expected_sha256":"any"}),
                "alignment" => json!({"op":op,"item_id":doc.item_id,"revision_id":doc.revision_id,
                    "chapter":"chapter-1","offset":0,"limit":10,"expected_version":"any"}),
                _ => json!({"op":op,"item_id":doc.item_id,"revision_id":doc.revision_id,
                    "side":"original","chapter":"chapter-1","offset":0,"limit":64}),
            };
            let result = response(start(&mut queries, request));
            assert!(result.ok, "{op}: {:?}", result.reason_code);
            if op == "chunk" {
                assert!(result.reader.unwrap()["markdown"].as_str().unwrap().contains("One morning."));
            }
            drained(&queries);
        }
        for op in ["translate_chunk", "translate_alignment"] {
            let result = response(start(&mut queries, json!({"op":op,"item_id":doc.item_id,
                "revision_id":doc.revision_id,"chapter":"appendix","expected_version":"missing","offset":0,"limit":8})));
            assert_eq!(result.reason_code.as_deref(), Some("reader_translation_unavailable"));
            drained(&queries);
        }
        let invalid = response(start(&mut queries, json!({"op":"chunk","item_id":doc.item_id,
            "revision_id":doc.revision_id,"side":"original","chapter":"chapter-1","offset":0,"limit":1})));
        assert_eq!(invalid.reason_code.as_deref(), Some("invalid_reader_limit"));
        drained(&queries);
        // Even nominally read-like job operations stay on the original slot.
        for request in [json!({"op":"edit_commit","item_id":doc.item_id,"edit_id":"synthetic"}),
            json!({"op":"translate_preview","item_id":doc.item_id,"revision_id":doc.revision_id,"chapter":"chapter-1"}),
            json!({"op":"align_status","item_id":doc.item_id,"job_id":"synthetic"})] {
            assert_eq!(response(start(&mut queries, request)).reason_code.as_deref(), Some("library_loading"));
        }
        assert!(!library.join("must-not-open.db").exists());
        *queries.idle.lock().unwrap() = Some(index);
    }

    #[test]
    fn saturated_store_reads_reject_and_timeouts_do_not_release_running_workers() {
        let temp = tempfile::tempdir().unwrap();
        let library = temp.path().join("Library");
        let doc = fixture::seed(&library).unwrap();
        let mut queries = BackgroundQueries::new(library.clone()).unwrap();
        let lock = fs::OpenOptions::new().read(true).write(true)
            .open(library.join("catalog/.ingest.lock")).unwrap();
        lock.lock().unwrap();
        let request = json!({"op":"toc","item_id":doc.item_id,"revision_id":doc.revision_id});
        let pending: Vec<_> = (0..MAX_STORE_READS).map(|_| start(&mut queries, request.clone())).collect();
        for _ in 0..20 {
            assert_eq!(response(start(&mut queries, request.clone())).reason_code.as_deref(), Some("library_loading"));
            assert_eq!(queries.store_reads.load(Ordering::Acquire), MAX_STORE_READS);
        }
        for client in pending {
            assert_eq!(response(client).reason_code.as_deref(), Some("library_loading"));
        }
        assert_eq!(queries.store_reads.load(Ordering::Acquire), MAX_STORE_READS);
        assert_eq!(response(start(&mut queries, request.clone())).reason_code.as_deref(), Some("library_loading"));
        assert!(queries.idle.lock().unwrap().is_some(), "reader saturation consumed index admission");
        lock.unlock().unwrap();
        drained(&queries);
        assert!(response(start(&mut queries, request)).ok);
        drained(&queries);
        assert!(!library.join("must-not-open.db").exists());
    }
}
