//! Disposable canonical Store + real owner daemon for encrypted gateway tests.
//! No input library/config/credentials accepted; never opens an installed store.
use anyhow::Result;
use memex_core::{
    BlobDescriptor, CaptureEnvelope, ItemDraft, Sensitivity, SourceProfile, VerifiedBlob,
    VerifiedCapture,
};
use sha2::{Digest, Sha256};
use std::{fs, io::Write, os::unix::fs::PermissionsExt};

#[path = "fixtures/reader_book.rs"]
mod reader_book;

// No sockets, DNS or model client: only these fixed synthetic responses. They
// still pass the real fetch policy, admission, translator, Store and Analysis.
struct SyntheticTransport;
impl memex_agent::fetch::Resolver for SyntheticTransport {
    fn resolve(&self, host: &str, _: std::time::Duration) -> Result<Vec<std::net::IpAddr>> {
        anyhow::ensure!(host == "capture.example.invalid", "dns_error");
        Ok(vec!["93.184.216.34".parse()?])
    }
}
impl memex_agent::fetch::HttpTransport for SyntheticTransport {
    fn get(
        &self,
        url: &url::Url,
        _: &[std::net::SocketAddr],
        _: std::time::Duration,
    ) -> Result<memex_agent::fetch::RawResponse> {
        anyhow::ensure!(
            url.host_str() == Some("capture.example.invalid"),
            "transport_error"
        );
        let text = if url.path() == "/chinese" {
            "这是完全合成的中文文章，只用于验证手机收录和状态，不涉及真实资料。".repeat(40)
        } else {
            "This synthetic article explains how careful experts evaluate evidence and make decisions. The test contains no private source material and does not call a real model. ".repeat(40)
        };
        let title = if url.path() == "/fail-translation" {
            "Synthetic failure article"
        } else {
            "Synthetic capture article"
        };
        Ok(memex_agent::fetch::RawResponse { status: 200,
            headers: vec![("content-type".into(), "text/html; charset=utf-8".into())],
            body: format!("<!doctype html><html><head><title>{title}</title></head><body><article><h1>{title}</h1><p>{text}</p></article></body></html>").into_bytes(),
        })
    }
}
struct SyntheticModel;
impl memex_analysis::TranslationModel for SyntheticModel {
    fn descriptor(&self) -> memex_analysis::TranslationModelDescriptor {
        memex_analysis::TranslationModelDescriptor {
            model_id: "synthetic-mobile".into(),
            model_sha256: format!("sha256:{}", "a".repeat(64)),
            adapter_id: "synthetic-mobile".into(),
            adapter_version: "1".into(),
            request_timeout: std::time::Duration::from_secs(1),
        }
    }
    fn translate(
        &mut self,
        input: memex_analysis::TranslationModelInput<'_>,
    ) -> std::result::Result<String, memex_analysis::TranslationModelError> {
        if input.source_text().contains("Synthetic failure") {
            return Err(memex_analysis::TranslationModelError::ProviderFailure);
        }
        Ok(match input.purpose() {
            memex_analysis::GeneratedTextPurpose::TitleTranslation => "合成收录文章".into(),
            _ => format!(
                "{}末尾界🙂",
                "这是合成译文，专家依据证据审慎判断。".repeat(400)
            ),
        })
    }
}

/// `--seed-ai` only: a multi-paragraph public essay whose AI insights and chat history are
/// seeded, so gateway-mode readers can be checked against real-shaped (synthetic) data.
const ESSAY: &str = "Good research notes are written for a future reader who has forgotten the context.\n\nA note that only records a conclusion is hard to trust later, because the reasons behind it are gone.\n\nKeeping the exact source location next to every claim lets the reader check the claim in seconds.\n\nSome teams report that linked notes halve the time spent re-reading old papers, although the figure is anecdotal.\n\nThe best note is short, specific, and points back to the passage it came from.\n\n(Synthetic fixture essay, not from any library.)";

/// Validated-shape items anchored verbatim in `ESSAY` blocks (no model call).
struct SeededInsights;
impl memex_analysis::InsightModel for SeededInsights {
    fn descriptor(&self) -> memex_analysis::TranslationModelDescriptor {
        memex_analysis::TranslationModelDescriptor {
            model_id: "synthetic-mobile".into(),
            model_sha256: format!("sha256:{}", "a".repeat(64)),
            adapter_id: "synthetic-mobile".into(),
            adapter_version: "1".into(),
            request_timeout: std::time::Duration::from_secs(1),
        }
    }
    fn generate(
        &mut self,
        input: memex_analysis::InsightModelInput<'_>,
    ) -> std::result::Result<memex_analysis::InsightOutput, memex_analysis::TranslationModelError>
    {
        let items = match input.kind() {
            memex_analysis::InsightKind::Highlights => serde_json::json!([
                {"block_index": 0, "quote": "written for a future reader", "style": "bold", "label": null},
                {"block_index": 2, "quote": "Keeping the exact source location next to every claim", "style": "highlight", "label": null},
                {"block_index": 3, "quote": "the figure is anecdotal", "style": "negative", "label": null},
                {"block_index": 4, "quote": "points back to the passage it came from", "style": "positive", "label": null}
            ]),
            memex_analysis::InsightKind::Quotes => serde_json::json!([
                {"block_index": 4, "quote": "The best note is short, specific, and points back to the passage it came from.",
                 "translation_zh": "最好的笔记简短、具体，并且能指回它出自的那段原文。", "why": "合成金句：全文的落点。"},
                {"block_index": 0, "quote": "Good research notes are written for a future reader who has forgotten the context.",
                 "translation_zh": "好的研究笔记，是写给已经忘了来龙去脉的未来读者的。", "why": "合成金句：开篇立意。"}
            ]),
            memex_analysis::InsightKind::FactCheck => serde_json::json!([
                {"block_index": 3, "quote": "linked notes halve the time spent re-reading old papers",
                 "verdict": "unverifiable", "explanation": "合成核查：文中只给出传闻性数字，没有来源。", "confidence": "medium"},
                {"block_index": 1, "quote": "A note that only records a conclusion is hard to trust later",
                 "verdict": "opinion", "explanation": "合成核查：这是作者的观点，不是可核实的事实。", "confidence": "high"}
            ]),
        };
        Ok(memex_analysis::InsightOutput {
            items,
            dropped: 0,
            translation_used: false,
        })
    }
}

fn seed_ai(library: &std::path::Path, database: &std::path::Path, item_id: &str) -> Result<()> {
    let (_, revision) = memex_store::read_item(library, item_id)?;
    let mut queue = memex_analysis::InsightQueue::open(library)?;
    for kind in memex_analysis::InsightKind::ALL {
        queue.request(&revision, kind)?;
        anyhow::ensure!(queue.process_next(kind, &mut SeededInsights)?, "invalid_fixture");
    }
    drop(memex_agent::queue::Queue::open(database)?);
    // An older exchange restored from a (synthetic) screenshot, before the stored chat turns.
    let restored: Vec<memex_agent::item_chat::ImportTurn> = serde_json::from_value(serde_json::json!([
        {"role": "user", "content": "（恢复的合成提问）这篇适合谁读？", "created_at_ms": 1_767_225_600_000_i64},
        {"role": "assistant", "content": "（恢复的合成回答）适合需要长期整理研究笔记的人。", "created_at_ms": 1_767_225_660_000_i64}
    ]))?;
    memex_agent::item_chat::conversation_import(
        database,
        library,
        item_id,
        memex_agent::item_chat::RESTORED_FROM_USER_SCREENSHOT,
        &restored,
    )?;
    for (id, question, reply) in [
        ("fixture-history-0001-synthetic", "这篇文章的核心观点是什么？",
         "（合成回答）作者认为：好的研究笔记要写给忘了上下文的未来读者，并且每条结论都要指回原文的具体位置。"),
        ("fixture-history-0002-synthetic", "文中那个“节省一半时间”的数字可靠吗？",
         "（合成回答）不太可靠。作者自己说这个数字是传闻（anecdotal），没有给出来源。"),
    ] {
        memex_agent::item_chat::record_synthetic_exchange(
            library, database, item_id, id, question, reply, "Synthetic Sol",
        )?;
    }
    Ok(())
}

fn main() -> Result<()> {
    // Resume only a disposable root previously created by this example, for
    // crash/replay proof. Never accepts a production config or library path.
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|arg| arg == "--resume") {
        let path = std::path::PathBuf::from(
            args.get(2)
                .ok_or_else(|| anyhow::anyhow!("invalid_fixture"))?,
        );
        anyhow::ensure!(
            path.parent() == Some(std::path::Path::new("/private/tmp"))
                && path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("memex-mobile-")
                && path.canonicalize()? == path,
            "invalid_fixture"
        );
        let mut receipt: serde_json::Value =
            serde_json::from_slice(&fs::read(path.join("synthetic-receipt.json"))?)?;
        anyhow::ensure!(
            receipt["synthetic_only"] == true && receipt["root"] == path.to_str().unwrap(),
            "invalid_fixture"
        );
        let config_path = path.join("config.json");
        let config = memex_agent::AgentConfig::read(&config_path)?;
        anyhow::ensure!(
            config.library == path.join("Library")
                && config.spool == path.join("spool")
                && config.database == path.join("agent.db")
                && config.socket == path.join("agent.sock")
                && config.legacy_translation.is_none()
                && config.intake.is_none(),
            "invalid_fixture"
        );
        receipt["pid"] = serde_json::json!(std::process::id());
        println!("{receipt}");
        std::io::stdout().flush()?;
        return serve_fixture(&config_path, &config);
    }
    let root = tempfile::Builder::new()
        .prefix("memex-mobile-")
        .tempdir()?;
    let root_path = root.path().canonicalize()?;
    let library = root_path.join("Library");
    let mut items = Vec::new();
    for (index, (title, sensitivity, mut text)) in [
        (
            "Amber public",
            Sensitivity::Public,
            format!("A{}界🙂Z", "x".repeat(16382)),
        ),
        (
            "Blue internal",
            Sensitivity::Internal,
            "Synthetic internal body 中文".to_owned(),
        ),
        (
            "Crimson private",
            Sensitivity::Private,
            "PRIVATE_CANARY_NOT_FOR_PUBLIC".to_owned(),
        ),
        (
            "Zinc public",
            Sensitivity::Public,
            "Public last body".to_owned(),
        ),
    ]
    .into_iter()
    .chain(
        std::env::args()
            .any(|arg| arg == "--seed-ai")
            .then(|| ("Ivory public essay", Sensitivity::Public, ESSAY.to_owned())),
    )
    .enumerate()
    {
        if index == 3 && args.iter().any(|a| a == "--seed-format") {
            text = "Next\n\n合成摘要  中文🌹正文。\n\nA synthetic  paragraph remains intact.\n".into();
        }
        let image = if index == 3 && args.iter().any(|a| a == "--seed-images") {
            let bytes = include_bytes!("fixtures/reading-image.png").to_vec();
            let hash = format!("{:x}", Sha256::digest(&bytes));
            let relative = format!("Imports/synthetic/raw/objects/{hash}");
            let path = root_path.join(&relative);
            fs::create_dir_all(path.parent().unwrap())?;
            fs::write(path, &bytes)?;
            let target = format!("attachments/{hash}.png");
            text = format!("# 正文配图验证\n\n这是图片前的正文。图片保存在本地导入资源中。\n\n![合成流程图]({target})\n\n这是图片后的正文。段内图片：![同一合成流程图]({target})之后继续阅读。\n\n![未保存的图片](attachments/{}.png)\n\n![远程图片](https://example.invalid/not-fetched.png)", "0".repeat(64));
            Some(VerifiedBlob { descriptor: BlobDescriptor { schema_version: 1, locator: relative.clone(),
                media_type: "application/octet-stream".into(), byte_size: bytes.len() as u64,
                sha256: hash.clone(), source_url: None }, blob_id: format!("sha256:{hash}"),
                source_backend_ref: Some(format!("memex-local-import:{relative}")), bytes })
        } else { None };
        let url = format!(
            "https://{}.example.invalid/item",
            if index == 2 { "secret" } else { "synthetic" }
        );
        let mut capture = VerifiedCapture {
            source: SourceProfile {
                schema_version: 1,
                source_id: format!("synthetic-{index}"),
                name: "Synthetic only".into(),
                source_kind: "web".into(),
                canonical_uris: vec![],
                evidence_roles: vec![],
                default_sensitivity: sensitivity,
                preferred_extractor: None,
            },
            capture: CaptureEnvelope {
                schema_version: 1,
                adapter_id: "mobile-fixture".into(),
                adapter_version: "1".into(),
                external_capture_id: format!("fixture-{index}"),
                source_id: format!("synthetic-{index}"),
                captured_at: "2026-01-01T00:00:00Z".into(),
                capture_method: if index == 1 { "owner_authorized_offline_import" } else { "browser_snapshot_v1" }.into(),
                sensitivity,
                route_summary: None,
                title: Some(title.into()),
                source_url: url.clone(),
                canonical_url: None,
                artifacts: vec![],
            },
            blobs: vec![VerifiedBlob {
                descriptor: BlobDescriptor {
                    schema_version: 1,
                    locator: "body".into(),
                    media_type: "text/plain".into(),
                    byte_size: text.len() as u64,
                    sha256: format!("{:x}", Sha256::digest(&text)),
                    source_url: None,
                },
                blob_id: format!("sha256:{:x}", Sha256::digest(&text)),
                source_backend_ref: Some("synthetic:mobile-fixture".into()),
                bytes: text.as_bytes().to_vec(),
            }],
        };
        if let Some(image) = image { capture.blobs.push(image); }
        let receipt = memex_store::ingest(
            &library,
            &capture,
            ItemDraft {
                item_type: if index == 2 { "secret_kind" } else { "webpage" }.into(),
                title: title.into(),
                source_url: url,
                canonical_url: None,
                content_markdown: text,
                sensitivity,
                translator_id: "mobile-fixture".into(),
                translator_version: "1".into(),
                metadata: Default::default(),
                attachments: vec![],
            },
        )?;
        if index == 3 && args.iter().any(|a| a == "--seed-images") {
            let rss = b"<rss/>";
            memex_store::covers::put_cover(&library, memex_store::covers::CoverEdit {
                item_id: receipt.item_ids[0].clone(),
                expected_revision_id: receipt.revision_ids[0].clone(),
                expected_cover_version: None, request_id: "synthetic-cover".into(),
                source: memex_store::covers::CoverSource::Podcast {
                    episode_url: capture.capture.source_url.clone(),
                    youtube_video_id: "abcdefghijk".into(),
                    image_url: "https://i.ytimg.com/vi/abcdefghijk/hqdefault.jpg".into(),
                    rss_sha256: format!("{:x}", Sha256::digest(rss)),
                },
            }, include_bytes!("fixtures/reading-image.png"), rss, sensitivity)?;
        }
        items.push(
            serde_json::json!({"item_id": receipt.item_ids[0], "revision_id": receipt.revision_ids[0]}),
        );
    }
    let config = memex_agent::AgentConfig {
        library,
        spool: root_path.join("spool"),
        database: root_path.join("agent.db"),
        socket: root_path.join("agent.sock"),
        legacy_translation: None,
        intake: None,
        metadata_lookup_contact: None,
        chrome_executable: None,
    };
    if let Some(essay) = items.get(4) {
        seed_ai(
            &config.library,
            &config.database,
            essay["item_id"].as_str().unwrap_or_default(),
        )?;
    }
    let config_path = root_path.join("config.json");
    fs::write(&config_path, serde_json::to_vec(&config)?)?;
    fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600))?;
    let fragmented = args.iter().any(|a| a == "--seed-fragmented");
    let seed_format = args.iter().any(|a| a == "--seed-format");
    let legacy_translation = args.iter().any(|a| a == "--seed-translation-legacy");
    let mapped_translation = args.iter().any(|a| a == "--seed-translation-mapped");
    anyhow::ensure!(!(legacy_translation && mapped_translation), "invalid_fixture");
    let seed_repair = fragmented || args.iter().any(|a| a == "--seed-repair" || a == "--seed-translation");
    let paragraph_book = if fragmented {
        Some(reader_book::fragmented(&config.library)?)
    } else if seed_repair || seed_format || legacy_translation || mapped_translation || args.iter().any(|a| a == "--seed-paragraphs") {
        Some(reader_book::seed(&config.library)?)
    } else { None };
    if legacy_translation || mapped_translation {
        use memex_store::reader::chapter_translation as projection;
        use memex_agent::queue::reader_translation::planner;
        let book = paragraph_book.as_ref().unwrap();
        let source = projection::source(&config.library, &book.item_id, &book.revision_id, "appendix", Sensitivity::Public)?;
        let plan = planner::plan(&source.markdown, planner::AdapterLimits { max_input_bytes: 8192, max_completion_tokens: 1024 })?;
        let outputs = plan.batches().iter().map(|batch| {
            batch.validate_output(&serde_json::json!({"source_sha256": plan.identity().source_sha256, "batch_id": batch.batch_id(),
                "blocks": batch.blocks().iter().map(|block| serde_json::json!({"id": block.id, "translation": "合成中文译文🙂"})).collect::<Vec<_>>()}).to_string())
        }).collect::<std::result::Result<Vec<_>, _>>()?;
        let (markdown, blocks) = plan.assemble_mapped(&outputs)?;
        let provenance = projection::Provenance { route: projection::Route {
            provider_id: "synthetic".into(), model_id: "synthetic".into(), reasoning_effort: None },
            prompt_version: planner::PROMPT_VERSION.into(), prompt_sha256: plan.identity().prompt_sha256.clone(),
            plan_sha256: plan.identity().sha256() };
        if legacy_translation {
            projection::publish(&config.library, source.identity, provenance, markdown, Sensitivity::Public)?;
        } else {
            projection::publish_mapped(&config.library, source.identity, provenance, markdown, blocks, Sensitivity::Public)?;
        }
    }
    if seed_repair {
        if config.library.join("catalog/chapter-alignments").exists() {
            fs::remove_dir_all(config.library.join("catalog/chapter-alignments"))?;
        }
    }
    if seed_repair || seed_format {
        // Create the fixture database with the daemon's owner-only permissions.
        drop(memex_agent::queue::Queue::open(&config.database)?);
        let mut conn = rusqlite::Connection::open(&config.database)?;
        memex_models::replace(&mut conn, 0, memex_models::Configuration {
            providers: vec![memex_models::Provider { id: "synthetic".into(), label: "Synthetic".into(), api_base: "https://model.example.invalid/v1/".into() }],
            models: vec![memex_models::Model { id: "synthetic".into(), label: "Synthetic".into(), provider_id: "synthetic".into(), model_id: "synthetic".into() }],
            selections: vec![memex_models::Selection { purpose: memex_models::Purpose::Translation,
                model: "synthetic".into(), reasoning_effort: None }],
        })?;
        memex_models::low_dial::replace(&mut conn, 0, memex_models::low_dial::Profile {
            revision: 1, model: "synthetic".into(), reasoning_effort: None,
            max_input_bytes: 8192, max_completion_tokens: 1024,
        })?;
    }
    let receipt = serde_json::json!({"synthetic_only": true, "root": root_path,
        "socket": config.socket, "items": items, "pid": std::process::id(), "paragraphBook": paragraph_book});
    fs::write(
        root_path.join("synthetic-receipt.json"),
        serde_json::to_vec(&receipt)?,
    )?;
    println!("{receipt}");
    std::io::stdout().flush()?;
    serve_fixture(&config_path, &config)
}

fn serve_fixture(config_path: &std::path::Path, config: &memex_agent::AgentConfig) -> Result<()> {
    if std::env::args().any(|arg| arg == "--capture") {
        // Finish the shared database's first creation/WAL setup before the
        // daemon opens its summary queue. The synthetic consumer must not race
        // that startup with another schema initializer every 200 ms.
        let mut queue = memex_analysis::TranslationQueue::open(&config.library)?;
        std::thread::spawn(move || loop {
            // Enqueue remains the daemon's existing post-Store hook. This
            // fixture only consumes it with deterministic synthetic output.
            let _ = queue.process_next(&mut SyntheticModel);
            std::thread::sleep(std::time::Duration::from_millis(200));
        });
    }
    memex_agent::ipc::run_daemon_with_workers(
        config_path,
        SyntheticTransport,
        SyntheticTransport,
        // One independent executor per reader function, as in the installed daemon.
        [|queue, library| memex_agent::queue::reader_repair::step_with(queue, library, None, |_,_,_,_,dispatch| {
            // This example never resolves credentials or creates a network client.
            dispatch()?;
            std::thread::sleep(std::time::Duration::from_millis(500));
            if std::env::args().any(|a| a == "--repair-unknown") {
                return Err(memex_provider::generation::GenerationError::OutcomeUnknown.into());
            }
            if std::env::args().any(|a| a == "--repair-fail") { return Ok(("invalid synthetic answer".into(), Some(17))) }
            if std::env::args().any(|a| a == "--seed-fragmented") {
                return Ok((r#"{"groups":[{"o":[0,1],"t":[0],"relation":"matched"},{"o":[2,3],"t":[1],"relation":"matched"},{"o":[4],"t":[2],"relation":"matched"},{"o":[4],"t":[3],"relation":"matched"}]}"#.into(), Some(23)));
            }
            Ok((r#"{"groups":[{"o":[0],"t":[0],"relation":"matched"},{"o":[1],"t":[1,2],"relation":"matched"},{"o":[2,3],"t":[3],"relation":"matched"},{"o":[],"t":[4],"relation":"translation_only"}]}"#.into(), Some(37)))
        }),
        |queue, library| memex_agent::queue::reader_translation::jobs::step_with(queue, library, |_, _, batch, dispatch| {
            dispatch()?;
            std::thread::sleep(std::time::Duration::from_millis(500));
            if std::env::args().any(|a| a == "--translation-unknown") {
                return Err(memex_provider::generation::GenerationError::OutcomeUnknown.into());
            }
            if std::env::args().any(|a| a == "--translation-fail") {
                return Err(memex_provider::generation::GenerationError::Rejected.into());
            }
            let input: serde_json::Value = serde_json::from_str(batch.input())?;
            Ok((serde_json::json!({"source_sha256": input["source_sha256"], "batch_id": batch.batch_id(),
                "blocks": batch.blocks().iter().map(|b| serde_json::json!({"id": b.id, "translation": "合成中文译文。"})).collect::<Vec<_>>() }).to_string(),
                memex_agent::queue::reader_repair::DispatchUsage { input_tokens: Some(11), output_tokens: Some(3), total_tokens: Some(17) }))
        }),
        |queue, library| memex_agent::queue::content_format::jobs::step_with(queue, library, |_,_|Ok(7), |_,_,_,window,dispatch| {
            dispatch()?;
            if std::env::args().any(|a|a=="--format-unknown") { return Err(memex_provider::generation::GenerationError::OutcomeUnknown.into()); }
            if std::env::args().any(|a|a=="--format-fail") { return Err(memex_provider::generation::GenerationError::Rejected.into()); }
            let input:serde_json::Value=serde_json::from_str(window.input())?;
            let parts=input["parts"].as_array().ok_or_else(||anyhow::anyhow!("invalid_fixture"))?;
            let mut edits=Vec::new();
            let mut classifications=Vec::new();
            for part in parts {
                if part["protected_unexamined"]==true {continue;}
                let text=part["text"].as_str().unwrap_or_default();
                let start=part["source_range"][0].as_u64().unwrap() as usize;
                let navigation=text.trim()=="Next" && part["complete_block"]==true;
                classifications.push(serde_json::json!({"range_id":part["id"],"role":if navigation{"furniture"}else{"body"}}));
                if navigation {
                    edits.push(serde_json::json!({"range_id":part["id"],"old_range":part["source_range"],"replacement":"","kind":"garbage_delete","reason_code":"navigation"}));
                } else if let Some(offset)=text.find("  ") {
                    edits.push(serde_json::json!({"range_id":part["id"],"old_range":[start+offset,start+offset+2],"replacement":" ","kind":"whitespace","reason_code":"paragraph_layout"}));
                }
            }
            Ok((serde_json::json!({"source_sha256":input["source_sha256"],"window_id":window.id(),"edits":edits,"classifications":classifications,"unresolved_ids":[]}).to_string(),
                memex_agent::queue::reader_repair::DispatchUsage{input_tokens:Some(11),output_tokens:Some(3),total_tokens:Some(17)}))
        },memex_store::reader::content_format::receipt,memex_store::reader::content_format::publish)],
        |_, _| Ok(7), // Synthetic generation; no credential status/resolve access.
        |_, _| Ok(7), // Independent formatting generation; no credentials.
    )
}
