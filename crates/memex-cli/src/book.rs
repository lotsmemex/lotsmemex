use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use memex_core::*;
use memex_store::reader::*;
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

mod alignment;

#[derive(Subcommand)]
pub enum BookCommands {
    /// Preserve complete local sources and raw-byte coverage; never writes a Library.
    PrepareSources {
        #[arg(long)] source: PathBuf,
        #[arg(long)] out: PathBuf,
        #[arg(long, value_parser = ["utf-8", "gb18030"])] encoding: String,
        #[arg(long)] python: PathBuf,
    },
    /// Verify a self-contained source bundle after backup/relocation.
    VerifySources {
        #[arg(long)] bundle: PathBuf,
        #[arg(long)] python: PathBuf,
    },
    /// Selected-chapter plan; offline by default, never applies Library edits.
    RepairChapter {
        #[arg(long)] library: PathBuf,
        #[arg(long)] item: String,
        #[arg(long)] revision: String,
        #[arg(long)] chapter: String,
        #[arg(long, value_enum)] max_sensitivity: crate::CliSensitivity,
        #[arg(long)] out: PathBuf,
        #[arg(long, requires_all=["database", "request_id"])] allow_model: bool,
        #[arg(long, requires="allow_model")] database: Option<PathBuf>,
        #[arg(long, requires="allow_model")] request_id: Option<String>,
    },
    /// Explicitly apply a source/revision/index-bound chapter plan under CAS.
    ApplyChapterRepair {
        #[arg(long)] library: PathBuf,
        #[arg(long)] plan: PathBuf,
        #[arg(long, value_enum)] max_sensitivity: crate::CliSensitivity,
    },
    /// Preview or process all explicitly paired chapters with a fixed request cap.
    AlignAll(alignment::BatchArgs),
    /// Validate and atomically install a complete derived index on an existing book.
    SaveAlignment {
        #[arg(long)] library: PathBuf,
        #[arg(long)] bundle: PathBuf,
        #[arg(long)] expected_version: String,
        #[arg(long, value_enum)] max_sensitivity: crate::CliSensitivity,
    },
    /// Sparse navigation anchors for ONE revision-bound chapter. No text rewrites.
    Align {
        #[arg(long)] library: PathBuf,
        #[arg(long)] item: String,
        #[arg(long)] revision: String,
        #[arg(long)] chapter: String,
        #[arg(long, value_enum)] max_sensitivity: crate::CliSensitivity,
        #[arg(long)] out: PathBuf,
        #[arg(long, default_value_t=3, value_parser=clap::value_parser!(u8).range(1..=7))] samples: u8,
        #[arg(long, requires="database")] allow_model: bool,
        #[arg(long, requires="allow_model")] database: Option<PathBuf>,
    },
    /// Build a portable paired book from two verified offline conversion bundles. No Store writes.
    Prepare {
        #[arg(long)]
        original: PathBuf,
        #[arg(long)]
        translation: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        title: String,
        #[arg(long, default_value = "en")]
        original_language: String,
        #[arg(long, default_value = "zh")]
        translation_language: String,
        #[arg(long)]
        python: Option<PathBuf>,
    },
    /// Explicitly create ONE private book. Exact retries replay the same ingest.
    Import {
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long)]
        library: PathBuf,
        /// Correct the translation of an existing book instead of creating an item.
        #[arg(long, requires = "expected_revision")]
        update_item: Option<String>,
        #[arg(long, requires = "update_item")]
        expected_revision: Option<String>,
    },
}

#[derive(Args)]
pub struct ReaderArgs {
    #[arg(long)]
    library: PathBuf,
    #[arg(long, value_enum)]
    max_sensitivity: crate::CliSensitivity,
    #[command(subcommand)]
    command: ReaderCommands,
}

#[derive(Subcommand)]
enum ReaderCommands {
    /// Export one exact historical text chunk to a new private local JSON file.
    RevisionChunk {
        item: String,
        #[arg(long)] revision: String,
        #[arg(long, requires = "chapter", value_parser = ["original", "translation"])] side: Option<String>,
        #[arg(long, requires = "side")] chapter: Option<String>,
        #[arg(long, default_value_t = 0)] start_byte: usize,
        #[arg(long, default_value_t = 8192)] limit_bytes: usize,
        #[arg(long, default_value = "any")] expected_sha256: String,
        #[arg(long)] out: PathBuf,
    },
    Alignment {
        item: String,
        #[arg(long)] revision: String,
    },
    Toc {
        item: String,
        #[arg(long)]
        revision: String,
    },
    Chunk {
        item: String,
        #[arg(long)]
        revision: String,
        #[arg(long)]
        side: String,
        #[arg(long)]
        chapter: String,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long, default_value_t=READER_CHUNK_BYTES)]
        limit: usize,
    },
    State {
        item: String,
    },
    Save {
        item: String,
        #[arg(long)]
        expected_version: String,
        #[arg(long)]
        state: PathBuf,
    },
    Resume,
}

fn read(path: &Path, max: usize) -> Result<Vec<u8>> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > max as u64 {
        anyhow::bail!("invalid_book_file")
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > max {
        anyhow::bail!("book_file_limit")
    }
    Ok(bytes)
}
fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

pub fn run(command: BookCommands) -> Result<()> {
    match command {
        BookCommands::PrepareSources { source, out, encoding, python } => {
            let status = Command::new(python)
                .args(["-I", "-c", include_str!("book/sources.py"), "prepare", "--source"])
                .arg(source).arg("--out").arg(out).arg("--encoding").arg(encoding)
                .stdin(Stdio::null()).status()?;
            anyhow::ensure!(status.success(), "book_source_operation_failed");
            Ok(())
        }
        BookCommands::VerifySources { bundle, python } => {
            let status = Command::new(python)
                .args(["-I", "-c", include_str!("book/sources.py"), "verify", "--bundle"])
                .arg(bundle).stdin(Stdio::null()).status()?;
            anyhow::ensure!(status.success(), "book_source_operation_failed");
            Ok(())
        }
        BookCommands::RepairChapter { library, item, revision, chapter, max_sensitivity, out, allow_model, database, request_id } => {
            anyhow::ensure!(!out.exists(), "book_output_exists");
            let ceiling: memex_core::Sensitivity = max_sensitivity.into();
            let mut plan = repair::plan(&library, &item, &revision, &chapter, ceiling)?;
            let mut job = None;
            if allow_model {
                use memex_agent::queue::{Queue, reader_repair as jobs};
                let database = database.context("no_low_dial_provider")?;
                anyhow::ensure!(database.is_file(), "no_low_dial_provider");
                let mut q = Queue::open(&database)?;
                let admitted = jobs::request(&mut q, &library, jobs::Request { item_id: item.clone(), revision_id: revision.clone(),
                    chapter: chapter.clone(), request_id: request_id.context("invalid_reader_alignment_request")?,
                    expected_alignment_version: plan.expected_alignment_version.clone() }, ceiling, false)?;
                jobs::step(&mut q, &library, Some(&admitted.job_id))?;
                let observed = jobs::status(&q, &library, &item, &admitted.job_id, ceiling)?;
                if observed.state != "succeeded" {
                    println!("{}", serde_json::json!({"job":observed,"plan_written":false,"library_written":false}));
                    anyhow::bail!("reader_repair_not_completed");
                }
                plan = jobs::planned(&q, &library, &item, &admitted.job_id, ceiling)?;
                job = Some(observed);
            }
            let bytes = serde_json::to_vec_pretty(&plan)?;
            if out.exists() {
                anyhow::ensure!(allow_model && read(&out, 8 * 1024 * 1024)? == bytes, "book_output_exists");
            } else { write(&out, &bytes)?; }
            println!("{}", serde_json::json!({"item_id":item,"revision_id":revision,
                "chapter":chapter,"model_requests":job.as_ref().map_or(0, |j| j.usage.requests_dispatched),"job":job,"library_written":false,
                "formatting_edits":0,"pair_rows":plan.pairs.len()}));
            Ok(())
        },
        BookCommands::ApplyChapterRepair { library, plan, max_sensitivity } => {
            let plan = serde_json::from_slice(&read(&plan, 8 * 1024 * 1024)?)?;
            let receipt = repair::apply(&library, plan, max_sensitivity.into())?;
            println!("{}", serde_json::to_string(&receipt)?);
            Ok(())
        },
        BookCommands::AlignAll(args) => alignment::batch(args),
        BookCommands::SaveAlignment { library, bundle, expected_version, max_sensitivity } => {
            let next = serde_json::from_slice(&read(&bundle, 4 * 1024 * 1024)?)?;
            let saved = memex_store::reader::alignment::save(&library, &expected_version, next, max_sensitivity.into())?;
            println!("{}", serde_json::json!({"item_id":saved.item_id,"revision_id":saved.revision_id,
                "version":saved.version()?,"paired_chapters":saved.chapters.len(),
                "anchors":saved.chapters.iter().map(|c| c.anchors.len()).sum::<usize>(),"source_text_changed":false}));
            Ok(())
        },
        BookCommands::Align { library, item, revision, chapter, max_sensitivity, out, samples, allow_model, database } => {
            let result = alignment::chapter(&library, &item, &revision, &chapter,
                max_sensitivity.into(), samples, &out, if allow_model { database.as_deref() } else { None }, None)?;
            println!("{result}");
            Ok(())
        },
        BookCommands::Prepare {
            original,
            translation,
            out,
            title,
            original_language,
            translation_language,
            python,
        } => {
            if title.trim().is_empty() || title.len() > 512 || title.contains('\0') {
                anyhow::bail!("invalid_book_title")
            }
            if out.try_exists()? {
                anyhow::bail!("book_output_exists")
            }
            let scratch = tempfile::tempdir()?;
            for (side, dir) in [("original", original), ("translation", translation)] {
                let receipt = read(&dir.join("conversion.json"), 65536)?;
                let meta: serde_json::Value = serde_json::from_slice(&receipt)?;
                if meta["schema_version"] != 1 || !crate::file_conversion::valid_engine(&meta["engine"]) {
                    anyhow::bail!("unsupported_conversion_bundle")
                }
                for (input, output, field, max) in [
                    (
                        "original.pdf",
                        format!("{side}.pdf"),
                        "input_sha256",
                        100 * 1024 * 1024,
                    ),
                    (
                        "raw.md",
                        format!("{side}.raw.md"),
                        "raw_sha256",
                        16 * 1024 * 1024,
                    ),
                    (
                        "document.md",
                        format!("{side}.converted.md"),
                        "markdown_sha256",
                        16 * 1024 * 1024,
                    ),
                ] {
                    let bytes = read(&dir.join(input), max)?;
                    if meta[field] != reader_sha256(&bytes) {
                        anyhow::bail!("conversion_bundle_integrity_mismatch")
                    }
                    write(&scratch.path().join(output), &bytes)?;
                }
                write(
                    &scratch.path().join(format!("{side}.conversion.json")),
                    &receipt,
                )?;
            }
            let python = python
                .or_else(|| {
                    std::env::var_os("HOME").map(|p| {
                        PathBuf::from(p).join(".local/share/memex/file-converter/bin/python")
                    })
                })
                .context("provide_converter_python")?;
            #[cfg(not(target_os = "macos"))]
            {
                let _ = python;
                anyhow::bail!("book_preparation_requires_macos_sandbox")
            }
            #[cfg(target_os = "macos")]
            {
                let errors = scratch.path().join("errors");
                let mut child = Command::new("/usr/bin/sandbox-exec")
                    .args(["-p", "(version 1) (allow default) (deny network*)"])
                    .arg(python)
                    .args(["-I", "-c", concat!(include_str!("file_conversion/pdf_text.py"), "\n", include_str!("book.py"))])
                    .arg(scratch.path())
                    .arg(original_language)
                    .arg(translation_language)
                    .env_clear()
                    .env("PATH", "/usr/bin:/bin")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(fs::File::create(&errors)?)
                    .spawn()?;
                let deadline = Instant::now() + Duration::from_secs(180);
                loop {
                    if let Some(status) = child.try_wait()? {
                        if !status.success() {
                            anyhow::bail!("book_preparation_failed")
                        }
                        break;
                    }
                    if Instant::now() >= deadline {
                        child.kill()?;
                        child.wait()?;
                        anyhow::bail!("book_preparation_timeout")
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            let manifest: BookManifest = serde_json::from_slice(&read(
                &scratch.path().join("manifest.json"),
                2 * 1024 * 1024,
            )?)?;
            for edition in &manifest.editions {
                let text = String::from_utf8(read(
                    &scratch.path().join(format!("{}.md", edition.side)),
                    16 * 1024 * 1024,
                )?)?;
                validate_edition(edition, &text)?;
            }
            let timestamp = std::process::Command::new("date")
                .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
                .output()?;
            if !timestamp.status.success() {
                anyhow::bail!("book_timestamp_failed")
            }
            let mut files = std::collections::BTreeMap::new();
            for name in bundle_names().into_iter().filter(|n| *n != "book.json") {
                files.insert(
                    name,
                    reader_sha256(&read(&scratch.path().join(name), 100 * 1024 * 1024)?),
                );
            }
            let metadata = serde_json::json!({"schema_version":1,"title":title,"captured_at":String::from_utf8(timestamp.stdout)?.trim(),"files":files});
            write(
                &scratch.path().join("book.json"),
                &serde_json::to_vec(&metadata)?,
            )?;
            fs::create_dir(&out)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&out, fs::Permissions::from_mode(0o700))?;
            }
            for name in bundle_names() {
                write(
                    &out.join(name),
                    &read(&scratch.path().join(name), 100 * 1024 * 1024)?,
                )?;
            }
            fs::File::open(&out)?.sync_all()?;
            println!(
                "{}",
                serde_json::json!({"prepared":true,"editions":manifest.editions.iter().map(|e|serde_json::json!({"side":e.side,"bytes":e.bytes,"chapters":e.chapters.len(),"paired":e.chapters.iter().filter(|c|c.counterpart.is_some()).count(),"sha256":e.sha256})).collect::<Vec<_>>() })
            );
            Ok(())
        }
        BookCommands::Import { bundle, library, update_item, expected_revision } => {
            let mut blobs = Vec::new();
            for name in bundle_names() {
                let limit = if name.ends_with(".pdf") {
                    100 * 1024 * 1024
                } else if name.ends_with(".md") {
                    16 * 1024 * 1024
                } else {
                    2 * 1024 * 1024
                };
                let bytes = read(&bundle.join(name), limit)?;
                let sha = reader_sha256(&bytes);
                let media_type = if name == "manifest.json" {
                    BOOK_MANIFEST
                } else if name.ends_with(".pdf") {
                    "application/pdf"
                } else if name.ends_with(".md") {
                    "text/markdown"
                } else {
                    "application/json"
                };
                blobs.push(VerifiedBlob {
                    descriptor: BlobDescriptor {
                        schema_version: 1,
                        locator: name.into(),
                        media_type: media_type.into(),
                        byte_size: bytes.len() as u64,
                        sha256: sha.clone(),
                        source_url: None,
                    },
                    blob_id: format!("sha256:{sha}"),
                    source_backend_ref: None,
                    bytes,
                });
            }
            let get = |name: &str| {
                blobs
                    .iter()
                    .find(|b| b.descriptor.locator == name)
                    .unwrap()
                    .bytes
                    .as_slice()
            };
            let meta: serde_json::Value = serde_json::from_slice(get("book.json"))?;
            if meta["schema_version"] != 1 {
                anyhow::bail!("invalid_book_receipt")
            }
            for blob in &blobs {
                if blob.descriptor.locator != "book.json"
                    && meta["files"][&blob.descriptor.locator] != blob.descriptor.sha256
                {
                    anyhow::bail!("book_bundle_integrity_mismatch")
                }
            }
            for side in ["original", "translation"] {
                let conversion: serde_json::Value =
                    serde_json::from_slice(get(&format!("{side}.conversion.json")))?;
                if conversion["schema_version"] != 1
                    || !crate::file_conversion::valid_engine(&conversion["engine"])
                {
                    anyhow::bail!("unsupported_conversion_bundle")
                }
                for (suffix, field) in [
                    ("pdf", "input_sha256"),
                    ("raw.md", "raw_sha256"),
                    ("converted.md", "markdown_sha256"),
                ] {
                    if conversion[field] != reader_sha256(get(&format!("{side}.{suffix}"))) {
                        anyhow::bail!("conversion_bundle_integrity_mismatch")
                    }
                }
            }
            let manifest: BookManifest = serde_json::from_slice(get("manifest.json"))?;
            if manifest.editions.len() != 2 {
                anyhow::bail!("invalid_book_manifest")
            }
            for (edition, side) in manifest.editions.iter().zip(["original", "translation"]) {
                if edition.side != side
                    || edition.sha256 != reader_sha256(get(&format!("{side}.md")))
                {
                    anyhow::bail!("book_bundle_integrity_mismatch")
                }
            }
            let title = meta["title"]
                .as_str()
                .filter(|v| !v.trim().is_empty() && v.len() <= 512)
                .context("invalid_book_title")?
                .to_string();
            let timestamp = meta["captured_at"]
                .as_str()
                .context("invalid_book_timestamp")?
                .to_string();
            let body = String::from_utf8(get("translation.md").to_vec())?;
            let identity = reader_sha256(&serde_json::to_vec(
                &blobs
                    .iter()
                    .filter(|b| b.descriptor.media_type == "application/pdf")
                    .map(|b| &b.descriptor.sha256)
                    .collect::<Vec<_>>(),
            )?);
            let url = format!("urn:memex:book:{identity}");
            let source = SourceProfile {
                schema_version: 1,
                source_id: "memex_parallel_books_v1".into(),
                name: "Local paired books".into(),
                source_kind: "local_file".into(),
                canonical_uris: vec![],
                evidence_roles: vec!["original_file".into(), "human_translation".into()],
                default_sensitivity: Sensitivity::Private,
                preferred_extractor: None,
            };
            let capture = VerifiedCapture {
                capture: CaptureEnvelope {
                    schema_version: 1,
                    adapter_id: "memex.parallel-book-import".into(),
                    adapter_version: "1".into(),
                    external_capture_id: identity,
                    source_id: source.source_id.clone(),
                    captured_at: timestamp,
                    capture_method: "owner_authorized_offline_import".into(),
                    sensitivity: Sensitivity::Private,
                    route_summary: None,
                    title: Some(title.clone()),
                    source_url: url.clone(),
                    canonical_url: None,
                    artifacts: blobs.iter().map(|b| b.descriptor.clone()).collect(),
                },
                source,
                blobs,
            };
            let draft = ItemDraft {
                item_type: "document".into(),
                title,
                source_url: url,
                canonical_url: None,
                content_markdown: body,
                sensitivity: Sensitivity::Private,
                translator_id: "memex.parallel-book-import".into(),
                translator_version: "1".into(),
                metadata: ItemMetadata::default(),
                attachments: vec![],
            };
            if let Some(item) = update_item {
                let receipt = refresh_book_translation(&library, &item,
                    expected_revision.as_deref().context("missing_expected_revision")?, &capture, &draft)?;
                println!("{}", serde_json::to_string(&receipt)?);
                return Ok(());
            }
            let receipt = ingest_reader_book(&library, capture, draft)?;
            println!("{}", serde_json::to_string(&receipt)?);
            Ok(())
        }
    }
}

fn bundle_names() -> [&'static str; 12] {
    [
        "original.pdf",
        "translation.pdf",
        "original.raw.md",
        "translation.raw.md",
        "original.converted.md",
        "translation.converted.md",
        "original.conversion.json",
        "translation.conversion.json",
        "original.md",
        "translation.md",
        "manifest.json",
        "book.json",
    ]
}

pub fn reader(args: ReaderArgs) -> Result<()> {
    let root = &args.library;
    let ceiling = args.max_sensitivity.into();
    let value = match args.command {
        ReaderCommands::RevisionChunk { item, revision, side, chapter, start_byte, limit_bytes, expected_sha256, out } => {
            let target = match (side, chapter) {
                (None, None) => revision_text::Target::Body {},
                (Some(side), Some(chapter)) => revision_text::Target::Chapter { side, chapter },
                _ => anyhow::bail!("invalid_reader_request"),
            };
            let chunk = revision_text::chunk(root, &item, &revision, target,
                start_byte, limit_bytes, &expected_sha256, ceiling)?;
            write(&out, &serde_json::to_vec(&chunk)?)?;
            serde_json::json!({"sha256":chunk.sha256,"total_bytes":chunk.total_bytes,
                "start_byte":chunk.start_byte,"end_byte":chunk.end_byte,"next_offset":chunk.next_offset})
        }
        ReaderCommands::Alignment { item, revision } => {
            let value = memex_store::reader::alignment::read(root, &item, &revision, ceiling)?;
            serde_json::json!({"version":value.as_ref().map(memex_store::reader::alignment::BookAlignment::version).transpose()?.unwrap_or_else(|| "missing".into()),"alignment":value})
        },
        ReaderCommands::Toc { item, revision } => {
            serde_json::to_value(reader_document(root, &item, &revision, ceiling)?)?
        }
        ReaderCommands::Chunk {
            item,
            revision,
            side,
            chapter,
            offset,
            limit,
        } => serde_json::to_value(reader_chunk(
            root, &item, &revision, &side, &chapter, offset, limit, ceiling,
        )?)?,
        ReaderCommands::State { item } => {
            let state = reader_state(root, &item, ceiling)?;
            serde_json::json!({"version":state.version()?,"state":state})
        }
        ReaderCommands::Save {
            item,
            expected_version,
            state,
        } => {
            let next = serde_json::from_slice(&read(&state, 4 * 1024 * 1024)?)?;
            let saved = save_reader_state(root, &item, &expected_version, next, ceiling)?;
            serde_json::json!({"version":saved.version()?,"state":saved})
        }
        ReaderCommands::Resume => serde_json::to_value(reader_resume(root, ceiling)?)?,
    };
    println!("{}", value);
    Ok(())
}
