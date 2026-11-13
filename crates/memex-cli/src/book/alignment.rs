use super::*;
use memex_store::reader::alignment::{BookAlignment, ChapterAlignment};
use serde_json::{json, Value};

#[derive(Args)]
pub struct BatchArgs {
    #[arg(long)]
    library: PathBuf,
    #[arg(long)]
    item: String,
    #[arg(long)]
    revision: String,
    #[arg(long, value_enum)]
    max_sensitivity: crate::CliSensitivity,
    #[arg(long)]
    out: PathBuf,
    #[arg(long, default_value_t=3, value_parser=clap::value_parser!(u8).range(1..=7))]
    samples: u8,
    #[arg(long, requires = "database")]
    allow_model: bool,
    #[arg(long, requires = "allow_model")]
    database: Option<PathBuf>,
    /// Upper bound on chapter tasks. Required explicitly for model-enabled batches.
    #[arg(long, default_value_t = 0)]
    max_requests: usize,
}

pub fn chapter(
    library: &Path,
    item: &str,
    revision: &str,
    chapter: &str,
    ceiling: Sensitivity,
    samples: u8,
    out: &Path,
    database: Option<&Path>,
    expected_settings: Option<&Value>,
) -> Result<Value> {
    let document = reader_document(library, item, revision, ceiling)?;
    let original = document
        .editions
        .iter()
        .find(|e| e.side == "original")
        .context("missing_original_edition")?;
    let translation = document
        .editions
        .iter()
        .find(|e| e.side == "translation")
        .context("missing_translation_edition")?;
    let left = original
        .chapters
        .iter()
        .find(|c| c.key == chapter)
        .context("unknown_reader_chapter")?;
    let right_key = left
        .counterpart
        .as_ref()
        .context("chapter_has_no_counterpart")?;
    let right = translation
        .chapters
        .iter()
        .find(|c| &c.key == right_key)
        .context("chapter_has_no_counterpart")?;
    let mut bodies = Vec::new();
    for (side, ch) in [("original", left), ("translation", right)] {
        let mut body = String::new();
        let mut offset = 0;
        loop {
            let chunk = reader_chunk(
                library,
                item,
                revision,
                side,
                &ch.key,
                offset,
                READER_CHUNK_BYTES,
                ceiling,
            )?;
            body.push_str(&chunk.markdown);
            if let Some(next) = chunk.next_offset {
                offset = next;
            } else {
                break;
            }
        }
        anyhow::ensure!(
            body.len() == ch.end - ch.start,
            "incomplete_alignment_chapter"
        );
        bodies.push(body);
    }
    let binding = json!({"item_id":item,"revision_id":revision,
        "canonical_sha256":document.canonical_sha256,
        "original":{"chapter":left.key,"chapter_start":left.start,"sha256":original.sha256},
        "translation":{"chapter":right.key,"chapter_start":right.start,"sha256":translation.sha256}});
    let result = crate::file_conversion::alignment::align(
        &bodies[0],
        &bodies[1],
        samples,
        out,
        database,
        binding,
        expected_settings,
    )?;
    reader_document(library, item, revision, ceiling)?;
    Ok(result)
}

pub fn batch(args: BatchArgs) -> Result<()> {
    let started = Instant::now();
    let ceiling: Sensitivity = args.max_sensitivity.into();
    let doc = reader_document(&args.library, &args.item, &args.revision, ceiling)?;
    let original = doc
        .editions
        .iter()
        .find(|e| e.side == "original")
        .context("missing_original_edition")?;
    let paired: Vec<_> = original
        .chapters
        .iter()
        .filter(|c| c.counterpart.is_some())
        .collect();
    anyhow::ensure!(!paired.is_empty(), "book_has_no_paired_chapters");
    let settings = || -> Result<Value> {
        let db = args.database.as_ref().context("missing_database")?;
        let conn =
            rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        Ok(
            json!({"models":memex_models::read(&conn)?,"profile":memex_models::low_dial::read(&conn)?}),
        )
    };
    let snapshot = if args.allow_model {
        Some(settings()?)
    } else {
        None
    };
    if args.allow_model {
        anyhow::ensure!(
            paired.len() <= args.max_requests,
            "book_alignment_request_budget_exceeded"
        );
        match fs::symlink_metadata(&args.out) {
            Ok(m) => anyhow::ensure!(
                m.is_dir() && !m.file_type().is_symlink(),
                "invalid_alignment_output"
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => fs::create_dir(&args.out)?,
            Err(e) => return Err(e.into()),
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&args.out, fs::Permissions::from_mode(0o700))?;
        }
        let plan = json!({"item_id":args.item,"revision_id":args.revision,"canonical_sha256":doc.canonical_sha256,
            "samples":args.samples,"settings_sha256":reader_sha256(&serde_json::to_vec(&snapshot)?)});
        let file = args.out.join("plan.json");
        if file.try_exists()? {
            let previous: Value = serde_json::from_slice(&read(&file, 128 * 1024)?)?;
            anyhow::ensure!(previous == plan, "book_alignment_plan_mismatch");
        } else {
            write(&file, &serde_json::to_vec(&plan)?)?;
            fs::File::open(&args.out)?.sync_all()?;
        }
    }
    let mut bundle = BookAlignment {
        schema_version: 1,
        item_id: args.item.clone(),
        revision_id: args.revision.clone(),
        chapters: vec![],
    };
    let mut timings = Vec::new();
    let mut requests = 0u64;
    let mut tokens = 0u64;
    let mut usage_unknown = 0usize;
    let mut failures = 0usize;
    for ch in paired {
        if let Some(ref initial) = snapshot {
            anyhow::ensure!(&settings()? == initial, "stale_low_dial_settings");
        }
        let started_chapter = Instant::now();
        // Hash the key: arbitrary chapter labels must never become filesystem paths.
        let task = args.out.join(reader_sha256(ch.key.as_bytes()));
        let result = chapter(
            &args.library,
            &args.item,
            &args.revision,
            &ch.key,
            ceiling,
            args.samples,
            &task,
            if args.allow_model {
                args.database.as_deref()
            } else {
                None
            },
            snapshot.as_ref(),
        );
        match result {
            Ok(report) => {
                let calls = report["requests_this_run"].as_u64().unwrap_or(0);
                requests += calls;
                if calls != 0 {
                    if let Some(total) = report["model_receipt"]["usage"]["total_tokens"].as_u64() {
                        tokens += total;
                    } else {
                        usage_unknown += 1;
                    }
                }
                if args.allow_model {
                    let index: ChapterAlignment =
                        serde_json::from_slice(&read(&task.join("alignment.json"), 128 * 1024)?)?;
                    bundle.chapters.push(index);
                }
                timings.push(json!({"chapter":ch.key,"elapsed_ms":started_chapter.elapsed().as_millis(),
                    "accepted":true,"requests_this_run":calls,"sample_text_bytes":report["sample_text_bytes"],
                    "request_input_bytes":report["request_input_bytes"],
                    "anchors":report["anchors"].as_array().map(Vec::len),
                    "usage":report["model_receipt"]["usage"]}));
            }
            Err(_) => {
                // Preserve per-task receipts; never retry, fabricate anchors, or echo a source-bearing error.
                failures += 1;
                timings.push(json!({"chapter":ch.key,"elapsed_ms":started_chapter.elapsed().as_millis(),"accepted":false}));
            }
        }
        eprintln!(
            "chapter_tasks_finished={} failures={failures}",
            timings.len()
        );
        // Permission or revision loss stops the batch, not just one chapter.
        reader_document(&args.library, &args.item, &args.revision, ceiling)?;
    }
    if args.allow_model && failures == 0 {
        anyhow::ensure!(
            snapshot.as_ref() == Some(&settings()?),
            "stale_low_dial_settings"
        );
        let file = args.out.join("book-alignment.json");
        let bytes = serde_json::to_vec_pretty(&bundle)?;
        if file.try_exists()? {
            anyhow::ensure!(
                read(&file, 4 * 1024 * 1024)? == bytes,
                "book_alignment_bundle_conflict"
            );
        } else {
            write(&file, &bytes)?;
        }
        fs::File::open(&args.out)?.sync_all()?;
    }
    println!(
        "{}",
        json!({"schema_version":1,"item_id":args.item,"revision_id":args.revision,
        "elapsed_ms":started.elapsed().as_millis(),"failed_chapters":failures,
        "successful_requests_this_run":requests,"successful_request_reported_tokens":tokens,
        "successful_requests_usage_unknown":usage_unknown,
        "failed_request_usage":"see_per_task_receipts","tasks":timings,
        "library_written":false,"model_enabled":args.allow_model})
    );
    anyhow::ensure!(failures == 0, "book_alignment_batch_incomplete");
    Ok(())
}
