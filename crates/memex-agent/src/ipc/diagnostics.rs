//! Local, finite-vocabulary diagnostics. Never format commands or error chains into logs.
use super::{Command, ReaderRequest, Response};
use std::{
    ffi::CString,
    fs::File,
    io::{self, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path},
};

const LIMIT: u64 = 64 * 1024;
const CURRENT: &str = "ipc-diagnostics.log";
const PREVIOUS: &str = "ipc-diagnostics.log.1";

/// Captured before dispatch consumes a command. The string cannot be caller-supplied.
#[derive(Clone, Copy)]
pub(super) struct Operation(&'static str);

impl Operation {
    pub(super) fn for_command(command: &Command) -> Self {
        Self(match command {
            Command::LibraryQuery(_) => "library_query",
            Command::ContextRead(_) => "context_read",
            Command::Reader { request, .. } => match request {
                ReaderRequest::AlignBatchPreview(_) => "reader_align_batch_preview",
                ReaderRequest::AlignBatchStart { .. } => "reader_align_batch_start",
                ReaderRequest::AlignBatchStatus { .. } => "reader_align_batch_status",
                ReaderRequest::AlignBatchStop { .. } => "reader_align_batch_stop",
                ReaderRequest::TranslateMapPreview(_) => "reader_translate_map_preview",
                ReaderRequest::TranslateMapRequest(_) => "reader_translate_map_request",
                ReaderRequest::TranslateMapStatus { .. } => "reader_translate_map_status",
                ReaderRequest::TranslateMapStop { .. } => "reader_translate_map_stop",
                ReaderRequest::TranslateMapRead { .. } => "reader_translate_map_read",
                ReaderRequest::FormatPreview(_) => "reader_format_preview",
                ReaderRequest::FormatTargets { .. } => "reader_format_targets",
                ReaderRequest::FormatRequest { .. } => "reader_format_request",
                ReaderRequest::FormatStatus { .. } => "reader_format_status",
                ReaderRequest::FormatResults { .. } => "reader_format_results",
                ReaderRequest::FormatChanges(_) => "reader_format_changes",
                ReaderRequest::FormatRestore { .. } => "reader_format_restore",
                ReaderRequest::FormatResume { .. } => "reader_format_resume",
                ReaderRequest::RevisionChunk { .. } => "reader_revision_chunk",
                ReaderRequest::TranslatePreview { .. } => "reader_translate_preview",
                ReaderRequest::TranslateRequest { .. } => "reader_translate_request",
                ReaderRequest::TranslateStatus { .. } => "reader_translate_status",
                ReaderRequest::TranslateChunk { .. } => "reader_translate_chunk",
                ReaderRequest::TranslateMetadata { .. } => "reader_translate_metadata",
                ReaderRequest::TranslateAlignment { .. } => "reader_translate_alignment",
                ReaderRequest::EditBegin { .. } => "reader_edit_begin",
                ReaderRequest::EditChunk { .. } => "reader_edit_chunk",
                ReaderRequest::EditCommit { .. } => "reader_edit_commit",
                ReaderRequest::AlignRequest { .. } => "reader_align_request",
                ReaderRequest::AlignStatus { .. } => "reader_align_status",
                ReaderRequest::Toc { .. } => "reader_toc",
                ReaderRequest::Alignment { .. } => "reader_alignment",
                ReaderRequest::Chunk { .. } => "reader_chunk",
                ReaderRequest::State { .. } => "reader_state",
                ReaderRequest::Save { .. } => "reader_save",
                ReaderRequest::Resume => "reader_resume",
                ReaderRequest::AnnotationsList { .. } => "reader_annotations_list",
                ReaderRequest::AnnotationsEdit { .. } => "reader_annotations_edit",
                ReaderRequest::PassesList { .. } => "reader_passes_list",
                ReaderRequest::PassesEdit { .. } => "reader_passes_edit",
                ReaderRequest::PassesStats { .. } => "reader_passes_stats",
                ReaderRequest::PassesTimingStart { .. } => "reader_passes_timing_start",
                ReaderRequest::PassesTimingReport { .. } => "reader_passes_timing_report",
            },
            Command::GroupTreeRead { .. } => "group_tree_read",
            Command::GroupEdit { .. } => "group_edit",
            Command::MarkdownEdit { .. } => "markdown_edit",
            Command::ReadingAnnotationsRead { .. } => "reading_annotations_read",
            Command::ReadingAnnotationsSave { .. } => "reading_annotations_save",
            _ => "other_command",
        })
    }
}

#[allow(dead_code)] // For callers that still own the command after dispatch.
pub(super) fn error_response(library: &Path, command: &Command, error: &anyhow::Error) -> Response {
    error_response_for_operation(library, Operation::for_command(command), error)
}

pub(super) fn error_response_for_operation(
    library: &Path,
    operation: Operation,
    error: &anyhow::Error,
) -> Response {
    // Preserve the existing client contract, including field-scoped validation errors.
    let response = Response::error(&error.to_string());
    if response.reason_code.as_deref() == Some("internal_error") {
        // Logging failure must not replace the request error, recurse, or expose raw stderr.
        let _ = append(library, operation, category(error));
    }
    response
}

fn category(error: &anyhow::Error) -> &'static str {
    if let Some(error) = error.downcast_ref::<io::Error>() {
        return match error.kind() {
            io::ErrorKind::NotFound => "io_not_found",
            io::ErrorKind::PermissionDenied => "io_permission_denied",
            io::ErrorKind::TimedOut => "io_timed_out",
            io::ErrorKind::InvalidData => "io_invalid_data",
            _ => "io_other",
        };
    }
    if let Some(error) = error.downcast_ref::<serde_json::Error>() {
        return match error.classify() {
            serde_json::error::Category::Io => "json_io",
            serde_json::error::Category::Syntax => "json_syntax",
            serde_json::error::Category::Data => "json_data",
            serde_json::error::Category::Eof => "json_eof",
        };
    }
    if error.downcast_ref::<rusqlite::Error>().is_some() {
        return "sqlite";
    }
    // Exact finite codes only. In particular, sanitize's field-prefix acceptance is NOT
    // an authorization to log arbitrary strings made of lowercase letters/underscores.
    match error.to_string().as_str() {
        "library_query_failed" => "library_query_failed",
        "internal_error" => "explicit_internal_error",
        _ => "unknown",
    }
}

fn unsafe_target() -> io::Error {
    io::Error::from(io::ErrorKind::PermissionDenied)
}

fn open_at(parent: &File, name: &std::ffi::OsStr, flags: i32) -> io::Result<File> {
    let name = CString::new(name.as_bytes()).map_err(|_| unsafe_target())?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn directory(library: &Path) -> io::Result<File> {
    // Walk using pinned directory descriptors: no symlink at any component, including
    // ancestors; no canonicalize-then-open race. Do not create or inspect library data.
    if !library.is_absolute() {
        return Err(unsafe_target());
    }
    let mut directory = File::open("/")?;
    for component in library.components() {
        match component {
            Component::RootDir => (),
            Component::Normal(name) => {
                directory = open_at(&directory, name, libc::O_RDONLY | libc::O_DIRECTORY)?;
            }
            _ => return Err(unsafe_target()),
        }
    }
    let metadata = directory.metadata()?;
    if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o022 != 0 {
        return Err(unsafe_target());
    }
    Ok(directory)
}

fn log_file(directory: &File, name: &str) -> io::Result<File> {
    let file = open_at(
        directory,
        name.as_ref(),
        libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND | libc::O_NONBLOCK,
    )?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
        || metadata.mode() & 0o777 != 0o600
    {
        return Err(unsafe_target());
    }
    Ok(file)
}

fn append(library: &Path, operation: Operation, category: &'static str) -> io::Result<()> {
    let directory = directory(library)?;
    let lock = log_file(&directory, "ipc-diagnostics.lock")?;
    // Bound contention too: diagnostics must never stall the listener for another process.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut file = log_file(&directory, CURRENT)?;
    let line = format!(
        "{} op={} category={}\n",
        chrono::Utc::now().to_rfc3339(),
        operation.0,
        category
    );
    if file.metadata()?.len() + line.len() as u64 > LIMIT {
        // Validate an existing rotation target without following symlinks or hard links.
        let previous = log_file(&directory, PREVIOUS)?;
        if file.metadata()?.len() > LIMIT {
            file.set_len(0)?;
        }
        drop(previous);
        let current = CString::new(CURRENT).unwrap();
        let previous = CString::new(PREVIOUS).unwrap();
        if unsafe {
            libc::renameat(
                directory.as_raw_fd(),
                current.as_ptr(),
                directory.as_raw_fd(),
                previous.as_ptr(),
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        file = log_file(&directory, CURRENT)?;
    }
    file.write_all(line.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{symlink, PermissionsExt},
    };

    fn fixture() -> tempfile::TempDir {
        tempfile::tempdir_in("/private/tmp").unwrap()
    }

    fn entries(root: &Path) -> String {
        fs::read_to_string(root.join(CURRENT))
            .unwrap()
            .lines()
            .map(|line| {
                let (timestamp, entry) = line.split_once(' ').unwrap();
                chrono::DateTime::parse_from_rfc3339(timestamp).unwrap();
                format!("{entry}\n")
            })
            .collect()
    }

    #[test]
    fn reader_operations_are_fixed_and_distinct_without_request_content() {
        let root = fixture();
        let secret = "https://canary.test/private?credential=BODY";
        let cases = [
            (
                ReaderRequest::Toc {
                    item_id: secret.into(),
                    revision_id: secret.into(),
                },
                "reader_toc",
            ),
            (
                ReaderRequest::Chunk {
                    item_id: secret.into(),
                    revision_id: secret.into(),
                    side: secret.into(),
                    chapter: secret.into(),
                    offset: 0,
                    limit: 1,
                },
                "reader_chunk",
            ),
            (
                ReaderRequest::FormatStatus {
                    batch_id: secret.into(),
                },
                "reader_format_status",
            ),
            (
                ReaderRequest::FormatPreview(super::super::reader::FormatPreviewRequest::Status {
                    preview_id: secret.into(),
                }),
                "reader_format_preview",
            ),
            (
                ReaderRequest::EditChunk {
                    item_id: secret.into(),
                    edit_id: secret.into(),
                    offset: 0,
                    markdown: secret.into(),
                },
                "reader_edit_chunk",
            ),
            (ReaderRequest::Resume, "reader_resume"),
        ];
        let mut expected_log = String::new();
        for (request, expected) in cases {
            let command = Command::Reader {
                max_sensitivity: memex_core::Sensitivity::Public,
                principal: Some(secret.into()),
                request,
            };
            assert_eq!(Operation::for_command(&command).0, expected);
            error_response(root.path(), &command, &anyhow::anyhow!(secret));
            expected_log.push_str(&format!("op={expected} category=unknown\n"));
        }
        assert_eq!(entries(root.path()), expected_log);
    }

    #[test]
    fn errors_log_only_finite_vocabulary_and_preserve_client_codes() {
        let root = fixture();
        let command = Command::LibraryItemRead {
            item_id: "secret-id".into(),
        };
        for canary in [
            "https://secret.test/?token=credential",
            "/private/body/path",
            "BODY CANARY",
            "unrecognized_lowercase_code",
        ] {
            let response = error_response(root.path(), &command, &anyhow::anyhow!(canary));
            assert_eq!(response.reason_code.as_deref(), Some("internal_error"));
        }
        let expected = "op=other_command category=unknown\n".repeat(4);
        assert_eq!(entries(root.path()), expected);
        for code in ["invalid_cursor", "invalid_metadata_field:title"] {
            assert_eq!(
                error_response(root.path(), &command, &anyhow::anyhow!(code))
                    .reason_code
                    .as_deref(),
                Some(code)
            );
        }
        assert_eq!(entries(root.path()), expected);
    }

    #[test]
    fn typed_sources_ignore_sensitive_context() {
        let error = anyhow::Error::new(io::Error::new(io::ErrorKind::NotFound, "secret/path"))
            .context("https://secret.test/body");
        assert_eq!(category(&error), "io_not_found");
        let json = serde_json::from_str::<bool>("\"secret body\"").unwrap_err();
        assert_eq!(category(&json.into()), "json_data");
        assert_eq!(
            category(&rusqlite::Error::InvalidParameterName("secret".into()).into()),
            "sqlite"
        );
        assert_eq!(
            category(&anyhow::anyhow!("library_query_failed")),
            "library_query_failed"
        );
        assert_eq!(
            category(&anyhow::anyhow!("library_query_failed secret")),
            "unknown"
        );
    }

    #[test]
    fn rotation_is_bounded_and_private() {
        let root = fixture();
        for _ in 0..5000 {
            append(root.path(), Operation("reader"), "unknown").unwrap();
        }
        for name in [CURRENT, PREVIOUS, "ipc-diagnostics.lock"] {
            let metadata = fs::metadata(root.path().join(name)).unwrap();
            assert_eq!(metadata.mode() & 0o777, 0o600);
            assert!(metadata.len() <= LIMIT);
        }
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 3);
    }

    #[test]
    fn unsafe_permissions_and_contention_preserve_response() {
        let root = fixture();
        let command = Command::GroupTreeRead {
            max_sensitivity: memex_core::Sensitivity::Public,
            offset: 0,
            limit: 1,
            expected_view: None,
        };
        let path = root.path().join(CURRENT);
        fs::write(&path, "untouched").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(append(root.path(), Operation("reader"), "unknown").is_err());
        assert_eq!(
            error_response(root.path(), &command, &anyhow::anyhow!("secret"))
                .reason_code
                .as_deref(),
            Some("internal_error")
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "untouched");
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o644);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let lock = log_file(&directory(root.path()).unwrap(), "ipc-diagnostics.lock").unwrap();
        assert_eq!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        assert!(append(root.path(), Operation("reader"), "unknown").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "untouched");
    }

    #[test]
    fn unsafe_targets_are_not_followed_or_modified() {
        let root = fixture();
        let outside = fixture();
        let target = outside.path().join("canary");
        fs::write(&target, "unchanged").unwrap();
        for name in [CURRENT, PREVIOUS, "ipc-diagnostics.lock"] {
            let library = root.path().join(name.replace('.', "_"));
            fs::create_dir(&library).unwrap();
            symlink(&target, library.join(name)).unwrap();
            if name == PREVIOUS {
                let file = log_file(&directory(&library).unwrap(), CURRENT).unwrap();
                file.set_len(LIMIT).unwrap();
            }
            assert!(append(&library, Operation("reader"), "unknown").is_err());
            assert_eq!(fs::read_to_string(&target).unwrap(), "unchanged");
        }
        let link = root.path().join("linked");
        symlink(outside.path(), &link).unwrap();
        assert!(append(&link, Operation("reader"), "unknown").is_err());
        let library = root.path().join("hardlink");
        fs::create_dir(&library).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        fs::hard_link(&target, library.join(CURRENT)).unwrap();
        assert!(append(&library, Operation("reader"), "unknown").is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "unchanged");
    }
}
