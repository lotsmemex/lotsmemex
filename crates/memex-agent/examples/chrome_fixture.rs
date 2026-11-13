//! Disposable loopback intake/Queue/Store with fixed synthetic transport/model.
use anyhow::{ensure, Result};
use memex_agent::{
    coordinator::Coordinator,
    fetch::{HttpTransport, RawResponse, Resolver},
    queue::ChromeIntake,
    AgentConfig,
};
use memex_analysis::capture_quality::*;
use std::{
    fs,
    io::{BufRead, Write},
    net::{IpAddr, SocketAddr},
    os::unix::fs::PermissionsExt,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

const ORIGIN: &str = "chrome-extension://dmccdibgheklkgclcddbbhllfdihkppl";
const SECRET: [u8; 64] = [b'a'; 64];
struct Synthetic;
impl Resolver for Synthetic {
    fn resolve(&self, host: &str, _: Duration) -> Result<Vec<IpAddr>> {
        ensure!(host.ends_with(".example.invalid"), "dns_error");
        Ok(vec!["93.184.216.34".parse()?])
    }
}
impl HttpTransport for Synthetic {
    fn get(&self, url: &url::Url, _: &[SocketAddr], _: Duration) -> Result<RawResponse> {
        ensure!(
            url.host_str()
                .is_some_and(|host| host.ends_with(".example.invalid")),
            "transport_error"
        );
        let host = url.host_str().unwrap();
        if host.starts_with("rate.") {
            return Ok(RawResponse {
                status: 429,
                headers: vec![("retry-after".into(), "120".into())],
                body: vec![],
            });
        }
        if host.starts_with("challenge.") {
            return Ok(RawResponse {
                status: 200,
                headers: vec![("content-type".into(), "text/html".into())],
                body: b"<main>captcha: verify you are human</main>".to_vec(),
            });
        }
        let comments = if host.starts_with("frame.") {
            "<div id=disqus_thread></div>"
        } else if host.starts_with("none.") {
            ""
        } else {
            "<section id=comments><p>COMMENT-SYNTHETIC 独立评论，不是文章正文。</p></section>"
        };
        let text = "This public-shaped synthetic article discusses evidence and careful reading. It contains no private data. ".repeat(20);
        let quality = if host.starts_with("quality-fail.") {
            "QUALITY-FAILURE"
        } else if host.starts_with("quality-review.") {
            "QUALITY-INVENT"
        } else if host.starts_with("quality-correct.") {
            "QUALITY-FORMAT"
        } else if host.starts_with("quality-unavailable.") {
            "QUALITY-UNAVAILABLE"
        } else {
            "QUALITY-PASS"
        };
        Ok(RawResponse { status:200, headers:vec![("content-type".into(), "text/html; charset=utf-8".into())],
            body:format!("<!doctype html><html><head><title>Synthetic Chrome article</title></head><body><article><h1>Synthetic Chrome article</h1><p>{quality} {text}</p>{comments}</article></body></html>").into_bytes() })
    }
}

impl CaptureQualityModel for Synthetic {
    fn descriptor(&self) -> CaptureQualityDescriptor {
        CaptureQualityDescriptor {
            model_id: "synthetic-capture-quality".into(),
            adapter_id: "synthetic".into(),
            adapter_version: "1".into(),
            configuration_sha256: format!("sha256:{}", "a".repeat(64)),
        }
    }
    fn check(
        &self,
        input: CaptureQualityInput<'_>,
    ) -> std::result::Result<String, CaptureQualityFailure> {
        assert!(!input.source_markdown.contains("COMMENT-SYNTHETIC"));
        std::thread::sleep(Duration::from_millis(400));
        if input.source_markdown.contains("QUALITY-FAILURE") {
            return Err(CaptureQualityFailure::Failed);
        }
        if input.source_markdown.contains("QUALITY-UNAVAILABLE") {
            return Err(CaptureQualityFailure::Unavailable);
        }
        if input.source_markdown.contains("QUALITY-INVENT") {
            return Ok("Invented article facts".into());
        }
        if input.source_markdown.contains("QUALITY-FORMAT") {
            return Ok(format!("{}\n", input.source_markdown));
        }
        Ok(input.source_markdown.into())
    }
}

fn main() -> Result<()> {
    let temporary = tempfile::Builder::new()
        .prefix("memex-chrome-")
        .tempdir()?;
    let root = temporary.path().canonicalize()?;
    let config = AgentConfig {
        library: root.join("Library"),
        spool: root.join("spool"),
        database: root.join("agent.db"),
        socket: root.join("agent.sock"),
        legacy_translation: None,
        intake: None,
        metadata_lookup_contact: None,
        chrome_executable: None,
    };
    let model = Arc::new(Synthetic);
    let mut worker =
        Coordinator::new(config.clone(), Synthetic, Synthetic)?.with_quality_model(model.clone());
    let snapshots = root.join("snapshots");
    fs::create_dir(&snapshots)?;
    fs::set_permissions(&snapshots, fs::Permissions::from_mode(0o700))?;
    let server =
        memex_intake::IntakeServer::bind(&config.library, &snapshots, SECRET, ORIGIN.into(), 0)?
            .with_url_jobs(ChromeIntake::new(
                config.database.clone(),
                config.library.clone(),
            )?)
            .with_quality_model(model);
    let url = server.startup_receipt().url;
    let receipt = serde_json::json!({"synthetic_only":true,"root":root,"pid":std::process::id(),"url":url,
        "origin":ORIGIN,"secret":String::from_utf8(SECRET.to_vec())?,"quality_available":true});
    fs::write(
        root.join("synthetic-receipt.json"),
        serde_json::to_vec(&receipt)?,
    )?;
    // Keep only after startup succeeds; the importing harness owns exact-root cleanup.
    let _ = temporary.keep();
    std::thread::spawn(move || server.serve().expect("synthetic intake stopped"));
    let paused = Arc::new(AtomicBool::new(false));
    let worker_paused = paused.clone();
    let library = config.library.clone();
    let mut quality_reconciler = CaptureQualityReconciler::default();
    std::thread::spawn(move || loop {
        if !worker_paused.load(Ordering::SeqCst) {
            if worker.process_one().is_err() {
                eprintln!("synthetic_worker_failed");
            }
            if quality_reconciler.process_next(&library).is_err() {
                eprintln!("synthetic_quality_materialization_deferred");
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    });
    println!("{receipt}");
    std::io::stdout().flush()?;
    for line in std::io::stdin().lock().lines() {
        let request: serde_json::Value = serde_json::from_str(&line?)?;
        let result = match request["command"].as_str() {
            Some("pause") => {
                paused.store(true, Ordering::SeqCst);
                serde_json::json!({"paused":true})
            }
            Some("resume") => {
                paused.store(false, Ordering::SeqCst);
                serde_json::json!({"paused":false})
            }
            Some("inspect") => {
                serde_json::json!({"items":memex_store::list_items(&config.library)?})
            }
            Some("stop") => break,
            _ => serde_json::json!({"error":"invalid_fixture_command"}),
        };
        println!(
            "{}",
            serde_json::json!({"id":request["id"],"result":result})
        );
        std::io::stdout().flush()?;
    }
    Ok(())
}
