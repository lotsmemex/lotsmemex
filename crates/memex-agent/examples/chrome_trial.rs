//! Explicit finite public-site trial; never uses installed config or credentials.
//! Args: NEW_ROOT SECRET_FILE ALLOWED_URLS_JSON EXTENSION_ORIGIN
use anyhow::{ensure, Context, Result};
use memex_agent::{
    coordinator::Coordinator,
    fetch::{HttpTransport, RawResponse, ReqwestTransport, Resolver, SystemResolver},
    queue::ChromeIntake,
    AgentConfig,
};
use memex_intake::url_jobs::{QualityMode, UrlJobBackend, UrlJobCommand, UrlJobError};
use std::{
    collections::HashSet,
    fs,
    io::Write,
    net::SocketAddr,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::PathBuf,
    time::Duration,
};

struct TrialJobs {
    backend: ChromeIntake,
    urls: HashSet<String>,
}
impl UrlJobBackend for TrialJobs {
    fn execute(
        &self,
        registration: &str,
        command: UrlJobCommand,
    ) -> std::result::Result<serde_json::Value, UrlJobError> {
        if let UrlJobCommand::Enqueue(request) = &command {
            if !self.urls.contains(&request.url) || request.quality_check != QualityMode::Off {
                return Err(UrlJobError::InvalidRequest);
            }
        }
        self.backend.execute(registration, command)
    }
}
struct TrialTransport {
    inner: ReqwestTransport,
    origins: HashSet<String>,
    responses: PathBuf,
}
struct TrialResolver {
    inner: SystemResolver,
    host: String,
}
impl Resolver for TrialResolver {
    fn resolve(&self, host: &str, timeout: Duration) -> Result<Vec<std::net::IpAddr>> {
        ensure!(host == self.host, "dns_error");
        self.inner.resolve(host, timeout)
    }
}
impl HttpTransport for TrialTransport {
    fn get(
        &self,
        url: &url::Url,
        addresses: &[SocketAddr],
        timeout: Duration,
    ) -> Result<RawResponse> {
        ensure!(
            self.origins.contains(&url.origin().ascii_serialization()),
            "transport_error"
        );
        let response = self.inner.get(url, addresses, timeout)?;
        // Only this explicitly authorized public trial retains rejected inputs.
        // Do not log headers such as Set-Cookie or add this to production capture.
        let id = uuid::Uuid::now_v7().to_string();
        let mut body = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(self.responses.join(format!("{id}.body")))?;
        body.write_all(&response.body)?;
        let selected_headers: Vec<_> = response
            .headers
            .iter()
            .filter(|(name, _)| {
                name.eq_ignore_ascii_case("content-type")
                    || name.eq_ignore_ascii_case("content-encoding")
            })
            .collect();
        use sha2::{Digest, Sha256};
        let receipt = serde_json::json!({"url":url.as_str(),"status":response.status,"headers":selected_headers,"bytes":response.body.len(),"sha256":format!("{:x}",Sha256::digest(&response.body))});
        let mut meta = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(self.responses.join(format!("{id}.json")))?;
        meta.write_all(&serde_json::to_vec(&receipt)?)?;
        Ok(response)
    }
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure!(
        args.len() == 4,
        "expected NEW_ROOT SECRET_FILE ALLOWED_URLS_JSON EXTENSION_ORIGIN"
    );
    let secret = memex_intake::read_pairing_secret(&args[1])?;
    let urls: HashSet<String> = serde_json::from_slice(&fs::read(&args[2])?)?;
    ensure!(
        !urls.is_empty() && urls.len() <= 6,
        "trial requires 1..6 explicit URLs"
    );
    let mut origins = HashSet::new();
    for text in &urls {
        let url = url::Url::parse(text)?;
        ensure!(
            url.scheme() == "https"
                && url.username().is_empty()
                && url.password().is_none()
                && url.fragment().is_none(),
            "invalid trial URL"
        );
        origins.insert(url.origin().ascii_serialization());
    }
    ensure!(origins.len() == 1, "trial is limited to one origin");
    let requested = PathBuf::from(&args[0]);
    ensure!(requested.is_absolute(), "absolute new root required");
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&requested)
        .context("trial root must not already exist")?;
    let root = requested.canonicalize()?;
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
    let responses = root.join("public-responses");
    fs::DirBuilder::new().mode(0o700).create(&responses)?;
    let mut worker = Coordinator::new(
        config.clone(),
        TrialResolver {
            inner: SystemResolver::default(),
            host: url::Url::parse(urls.iter().next().unwrap())?
                .host_str()
                .context("missing host")?
                .into(),
        },
        TrialTransport {
            inner: ReqwestTransport::default(),
            origins,
            responses,
        },
    )?;
    let snapshots = root.join("snapshots");
    fs::DirBuilder::new().mode(0o700).create(&snapshots)?;
    let server =
        memex_intake::IntakeServer::bind(&config.library, &snapshots, secret, args[3].clone(), 0)?
            .without_snapshots()
            .with_url_jobs(TrialJobs {
                backend: ChromeIntake::new(config.database.clone(), config.library.clone())?,
                urls,
            });
    let receipt = serde_json::json!({"pid":std::process::id(),"root":root,"url":server.startup_receipt().url,"origin":args[3],"quality_available":false,"isolated_public_trial":true});
    fs::write(
        root.join("trial-receipt.json"),
        serde_json::to_vec(&receipt)?,
    )?;
    println!("{receipt}");
    std::io::stdout().flush()?;
    std::thread::spawn(move || {
        if server.serve().is_err() {
            eprintln!("trial_intake_stopped");
            std::process::exit(1);
        }
    });
    loop {
        worker.process_one().context("trial worker stopped")?;
        std::thread::sleep(Duration::from_millis(100));
    }
}
