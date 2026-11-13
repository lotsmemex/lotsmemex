use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use memex_ingest::{legacy::preflight_package, read_verified_capture};
use memex_store::{ingest, item_index_json, item_markdown, read_item};
use lots_scout_extract::{Extractor, GenericWebpageExtractor};
use std::{io::Write, net::Ipv4Addr, path::PathBuf};
mod added_at;
mod book;
mod content_format;
mod file_conversion;
mod low_dial;
mod native_import;
mod translation;
mod web;
#[derive(Parser)]
#[command(name = "memex")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}
#[derive(Subcommand)]
enum Commands {
    /// Plan or explicitly execute a persisted table-only translation repair.
    Translation {
        #[command(subcommand)]
        command: translation::Commands,
    },
    /// Reusable, network-free webpage structure, discovery and snapshot collection.
    Web {
        #[command(subcommand)]
        command: web::Commands,
    },
    /// Configure reusable low-cost mechanical tasks independently of interactive routes.
    LowDial {
        #[command(subcommand)]
        command: low_dial::Commands,
    },
    Book {
        #[command(subcommand)]
        command: book::BookCommands,
    },
    Reader(book::ReaderArgs),
    /// Persisted owner-daemon formatting, with explicit quote/model consent.
    ContentFormat(content_format::Args),
    Import {
        #[command(subcommand)]
        command: native_import::ImportCommands,
    },
    File {
        #[command(subcommand)]
        command: file_conversion::FileCommands,
    },
    Daemon {
        #[command(subcommand)]
        command: DaemonCommands,
    },
    Direct {
        #[command(subcommand)]
        command: DirectCommands,
    },
    Capture {
        #[arg(required = true)]
        text: Vec<String>,
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        request_id: Option<String>,
    },
    Jobs {
        #[command(subcommand)]
        command: JobCommands,
    },
    Intake {
        #[command(subcommand)]
        command: IntakeCommands,
    },
    Serve {
        #[arg(long)]
        library: PathBuf,
        #[arg(long, value_enum)]
        max_sensitivity: CliSensitivity,
        #[arg(long, default_value_t = 0)]
        port: u16,
        #[arg(long, default_value_t = false)]
        orb_portal: bool,
        #[arg(long, requires = "lan_hostname", conflicts_with = "orb_portal")]
        lan_address: Option<Ipv4Addr>,
        #[arg(long, requires = "lan_address", conflicts_with = "orb_portal")]
        lan_hostname: Option<String>,
    },
    Recall {
        #[arg(long)]
        library: PathBuf,
        #[arg(long)]
        query: String,
        #[arg(long, value_enum)]
        max_sensitivity: CliSensitivity,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long,value_enum,default_value_t=Format::Json)]
        format: Format,
    },
    /// Read a revision-bound UTF-8 byte range, with source references, as JSON.
    ContextRead {
        #[arg(long)]
        library: PathBuf,
        #[arg(long)]
        item: String,
        #[arg(long)]
        expected_revision_id: String,
        #[arg(long)]
        start_byte: usize,
        #[arg(long)]
        end_byte: usize,
        #[arg(long, value_enum)]
        max_sensitivity: CliSensitivity,
    },
    /// Query the local daemon's verified library snapshot (local trial).
    LibraryQuery {
        #[arg(long)]
        socket: Option<PathBuf>,
        #[arg(long, default_value = "")]
        query: String,
        #[arg(long)]
        source: Option<String>,
        #[arg(long)]
        item_type: Option<String>,
        #[arg(long, value_parser = ["capture", "import"])]
        origin: Option<String>,
        #[arg(long, default_value = "title", value_parser = ["title", "source", "captured_at", "added_at", "item_type"])]
        sort: String,
        #[arg(long)]
        descending: bool,
        #[arg(long, value_enum)]
        max_sensitivity: CliSensitivity,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long)]
        view: Option<String>,
    },
    Analysis {
        #[command(subcommand)]
        command: AnalysisCommands,
    },
    Publication {
        #[command(subcommand)]
        command: PublicationCommands,
    },
    Source {
        #[command(subcommand)]
        command: SourceCommands,
    },
    Ingest {
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        capture: PathBuf,
        #[arg(long)]
        library: PathBuf,
    },
    ShadowImportLegacy {
        #[arg(long)]
        package_root: PathBuf,
        #[arg(long)]
        meta: PathBuf,
        #[arg(long, required = true)]
        items: Vec<PathBuf>,
        #[arg(long)]
        library: PathBuf,
    },
    Show {
        #[arg(long)]
        library: PathBuf,
        #[arg(long)]
        item: String,
        #[arg(long, value_enum, default_value_t=ItemFormat::Json)]
        format: ItemFormat,
    },
    /// Read-only added_at maintenance (guarded backfill).
    AddedAt {
        #[command(subcommand)]
        command: added_at::AddedAtCommands,
    },
}
#[derive(Subcommand)]
enum DaemonCommands {
    Run {
        #[arg(long)]
        config: Option<PathBuf>,
    },
    Install,
    Uninstall,
    Status,
}
#[derive(Subcommand)]
enum DirectCommands {
    Install {
        #[arg(long)]
        lan_address: Ipv4Addr,
        #[arg(long)]
        lan_hostname: String,
        #[arg(long, default_value_t = 43191)]
        port: u16,
    },
    Uninstall,
    Status,
}
#[derive(Subcommand)]
enum JobCommands {
    List {
        #[arg(long)]
        cursor: Option<String>,
    },
    Status {
        id: String,
    },
    Retry {
        id: String,
    },
    Cancel {
        id: String,
    },
    Approve {
        id: String,
    },
    /// Read-only capture progress (`capture_jobs_recent_v1`) as JSON: the raw reply plus
    /// `memex_agent::capture_progress` summary and labels. No content, options or paths.
    Recent {
        #[arg(long, default_value_t = 50)]
        limit: u32,
        #[arg(long, value_enum, default_value = "restricted")]
        max_sensitivity: CliSensitivity,
        #[arg(long, value_enum, default_value = "zh")]
        lang: CliLang,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum CliLang {
    Zh,
    En,
}

fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("home_unavailable"))
}
fn socket() -> Result<PathBuf> {
    Ok(memex_agent::AgentConfig::standard(&home()?).socket)
}
fn capture_request_id(request_id: Option<String>) -> Result<String> {
    let value = request_id.unwrap_or_else(|| format!("cli_{}", uuid::Uuid::now_v7().simple()));
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        anyhow::bail!("invalid_request_id")
    }
    Ok(value)
}
fn print_agent_response(r: memex_agent::Response) -> Result<()> {
    println!("{}", serde_json::to_string(&r)?);
    if !r.ok {
        anyhow::bail!(r.reason_code.unwrap_or_else(|| "internal_error".into()))
    }
    Ok(())
}
#[derive(Subcommand)]
enum IntakeCommands {
    Serve {
        #[arg(long)]
        library: PathBuf,
        #[arg(long)]
        snapshots: PathBuf,
        #[arg(long)]
        secret_file: PathBuf,
        #[arg(long)]
        origin: String,
        #[arg(long, default_value_t = 0)]
        port: u16,
    },
}
#[derive(Subcommand)]
enum AnalysisCommands {
    Add {
        #[arg(long)]
        library: PathBuf,
        #[arg(long)]
        request: PathBuf,
    },
    Link {
        #[arg(long)]
        library: PathBuf,
        #[arg(long)]
        request: PathBuf,
    },
    List {
        #[arg(long)]
        library: PathBuf,
        #[arg(long,value_enum,default_value_t=Format::Json)]
        format: Format,
    },
    Show {
        #[arg(long)]
        library: PathBuf,
        #[arg(long)]
        analysis: String,
        #[arg(long,value_enum,default_value_t=Format::Json)]
        format: Format,
    },
}
#[derive(Subcommand)]
enum PublicationCommands {
    Create {
        #[arg(long)]
        library: PathBuf,
        #[arg(long)]
        request: PathBuf,
    },
    List {
        #[arg(long)]
        library: PathBuf,
        #[arg(long, value_enum, default_value_t=Format::Json)]
        format: Format,
    },
    Show {
        #[arg(long)]
        library: PathBuf,
        #[arg(long)]
        publication: String,
        #[arg(long, value_enum, default_value_t=Format::Json)]
        format: Format,
    },
    ReleasePublic {
        #[arg(long)]
        library: PathBuf,
        #[arg(long)]
        request: PathBuf,
    },
    ShowPublic {
        #[arg(long)]
        library: PathBuf,
        #[arg(long)]
        release: String,
        #[arg(long, value_enum, default_value_t=Format::Json)]
        format: Format,
    },
}
#[derive(Subcommand)]
enum SourceCommands {
    ImportLegacy {
        #[arg(long)]
        package_root: PathBuf,
        #[arg(long, required = true)]
        profile: Vec<PathBuf>,
        #[arg(long)]
        library: PathBuf,
    },
    List {
        #[arg(long)]
        library: PathBuf,
        #[arg(long, value_enum, default_value_t=Format::Json)]
        format: Format,
    },
    Show {
        #[arg(long)]
        library: PathBuf,
        #[arg(long)]
        source: String,
        #[arg(long, value_enum, default_value_t=Format::Json)]
        format: Format,
    },
}
#[derive(Clone, ValueEnum)]
enum Format {
    Json,
    Markdown,
}
#[derive(Clone, ValueEnum)]
enum ItemFormat {
    Json,
    Markdown,
    PortableJson,
}
#[derive(Clone, ValueEnum)]
enum CliSensitivity {
    Public,
    Internal,
    Private,
    Restricted,
}
impl From<CliSensitivity> for memex_core::Sensitivity {
    fn from(v: CliSensitivity) -> Self {
        match v {
            CliSensitivity::Public => Self::Public,
            CliSensitivity::Internal => Self::Internal,
            CliSensitivity::Private => Self::Private,
            CliSensitivity::Restricted => Self::Restricted,
        }
    }
}

fn shadow_report(
    run_ref_sha256: &str,
    input_files: &[memex_ingest::legacy::InputFile],
    total: usize,
    created: usize,
    replayed: usize,
    failed: usize,
    items: Vec<serde_json::Value>,
) -> serde_json::Value {
    let lossy = items
        .iter()
        .filter(|item| {
            item["unmapped_fields"]
                .as_array()
                .is_some_and(|fields| !fields.is_empty())
        })
        .count();
    serde_json::json!({
        "schema_version": 1, "adapter_id": memex_ingest::legacy::ADAPTER_ID,
        "run_ref_sha256": run_ref_sha256,
        "input_files": input_files.iter().map(|input| serde_json::json!({
            "path": input.path, "sha256": input.sha256,
        })).collect::<Vec<_>>(),
        "preflight": "passed",
        "summary": {"total": total, "created": created, "replayed": replayed, "failed": failed, "lossy": lossy},
        "items": items,
    })
}

fn shadow_item(
    prepared: &memex_ingest::legacy::PreparedItem,
    status: &str,
    item_id: Option<String>,
    capture_id: Option<String>,
    error_code: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "legacy_ref_sha256": prepared.legacy_ref_sha256,
        "item_id": item_id,
        "capture_id": capture_id,
        "status": status,
        "mapped_fields": ["source", "item_id", "title", "content", "url", "captured_at"],
        "unmapped_fields": prepared.unmapped_fields,
        "defaulted_fields": ["sensitivity:internal"],
        "provenance_gaps": ["route_provenance_missing", "translator_provenance_missing"],
        "error_code": error_code,
    })
}
fn registry_row(
    p: &memex_registry::Prepared,
    status: &str,
    record: Option<&memex_registry::Record>,
    error: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({"profile_ref_sha256":p.digest,"source_id":p.record.source_id,"revision_id":record.map(|r|r.revision_id.clone()),"revision_number":record.map(|r|r.revision_number),"status":status,"mapped_fields":p.record.provenance.mapped_fields,"unmapped_fields":p.record.provenance.unmapped_fields,"defaulted_fields":p.record.provenance.defaulted_fields,"warning_codes":p.record.provenance.warning_codes,"error_code":error})
}
#[derive(Default)]
struct RegistryCounts {
    total: usize,
    created: usize,
    revised: usize,
    replayed: usize,
    failed: usize,
    unattempted: usize,
}
fn registry_report(
    input_files: Vec<serde_json::Value>,
    counts: &RegistryCounts,
    sources: Vec<serde_json::Value>,
) -> serde_json::Value {
    let lossy = sources
        .iter()
        .filter(|s| {
            !s["unmapped_fields"].as_array().unwrap().is_empty()
                || !s["warning_codes"].as_array().unwrap().is_empty()
        })
        .count();
    serde_json::json!({"schema_version":1,"adapter_id":"intelligence-source-registry.legacy-profile.v0","adapter_version":"0","preflight":"passed","input_files":input_files,"summary":{"total":counts.total,"created":counts.created,"revised":counts.revised,"replayed":counts.replayed,"failed":counts.failed,"unattempted":counts.unattempted,"lossy":lossy},"sources":sources})
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Commands::Translation { command } => translation::run(command)?,
        Commands::Web { command } => web::run(command)?,
        Commands::LowDial { command } => low_dial::run(command)?,
        Commands::Book { command } => book::run(command)?,
        Commands::Reader(args) => book::reader(args)?,
        Commands::ContentFormat(args) => content_format::run(args)?,
        Commands::Import { command } => native_import::run(command)?,
        Commands::File { command } => file_conversion::run(command)?,
        Commands::Daemon { command } => match command {
            DaemonCommands::Run { config } => {
                let home = home()?;
                let config = config
                    .unwrap_or_else(|| memex_agent::AgentConfig::standard_config_path(&home));
                memex_agent::run_daemon_with_sources(&config, source_handlers())?
            }
            DaemonCommands::Install => {
                let home = home()?;
                let config = memex_agent::AgentConfig::standard_config_path(&home);
                memex_agent::daemon_install(&home, &std::env::current_exe()?, &config)?
            }
            DaemonCommands::Uninstall => memex_agent::daemon_uninstall(&home()?)?,
            DaemonCommands::Status => println!(
                "{}",
                serde_json::to_string(&memex_agent::daemon_status(&home()?)?)?
            ),
        },
        Commands::Direct { command } => match command {
            DirectCommands::Install {
                lan_address,
                lan_hostname,
                port,
            } => {
                memex_agent::validate_private_lan(lan_address, &lan_hostname, port)?;
                let home = home()?;
                let executable = std::env::current_exe()?;
                let config = memex_agent::AgentConfig::standard_config_path(&home);
                memex_agent::daemon_install(&home, &executable, &config)?;
                memex_agent::surface_install(
                    &home,
                    &executable,
                    lan_address,
                    &lan_hostname,
                    port,
                )?;
            }
            DirectCommands::Uninstall => {
                let home = home()?;
                memex_agent::surface_uninstall(&home)?;
                memex_agent::daemon_uninstall(&home)?;
            }
            DirectCommands::Status => {
                let home = home()?;
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({
                        "daemon": memex_agent::daemon_status(&home)?,
                        "surface": memex_agent::surface_status(&home)?,
                    }))?
                );
            }
        },
        Commands::Capture {
            text,
            yes,
            request_id,
        } => {
            let (url, summary) = memex_agent::extract_url(&text.join(" "))?;
            println!("{}", serde_json::to_string(&summary)?);
            if !yes {
                print!("enqueue? [y/N] ");
                std::io::stdout().flush()?;
                let mut answer = String::new();
                std::io::stdin().read_line(&mut answer)?;
                if !matches!(answer.trim(), "y" | "Y") {
                    anyhow::bail!("cancelled_by_user")
                }
            }
            let producer_job_id = capture_request_id(request_id)?;
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({"request_id": producer_job_id}))?
            );
            let command = memex_agent::Command::Enqueue {
                producer: memex_agent::Producer::LocalCli,
                producer_job_id: producer_job_id.clone(),
                url,
            };
            let socket = socket()?;
            let response = match memex_agent::send_command(&socket, &command) {
                Ok(response) => response,
                Err(_) => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    memex_agent::send_command(&socket, &command).map_err(|_| {
                        anyhow::anyhow!(
                            "daemon_unavailable; retry with --request-id {producer_job_id}"
                        )
                    })?
                }
            };
            print_agent_response(response)?;
        }
        Commands::Jobs { command } => {
            if let JobCommands::Recent { limit, max_sensitivity, lang } = command {
                let command = memex_agent::Command::CaptureJobsRecent { max_sensitivity: max_sensitivity.into(), limit };
                let response = memex_agent::send_command(&socket()?, &command)
                    .map_err(|_| anyhow::anyhow!("daemon_unavailable"))?;
                if !response.ok {
                    return print_agent_response(response);
                }
                let recent = response.capture_jobs.ok_or_else(|| anyhow::anyhow!("invalid_response"))?;
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs() as i64;
                let lang = match lang {
                    CliLang::Zh => memex_agent::capture_progress::Lang::Zh,
                    CliLang::En => memex_agent::capture_progress::Lang::En,
                };
                let progress = memex_agent::capture_progress::project(&recent, now, lang);
                println!("{}", serde_json::json!({"ok": true, "capture_jobs": recent, "progress": progress}));
                return Ok(());
            }
            let command = match command {
                JobCommands::Recent { .. } => unreachable!(),
                JobCommands::List { cursor } => memex_agent::Command::List { cursor },
                JobCommands::Status { id } => memex_agent::Command::Status { job_id: id },
                JobCommands::Retry { id } => memex_agent::Command::Retry { job_id: id },
                JobCommands::Cancel { id } => memex_agent::Command::Cancel { job_id: id },
                JobCommands::Approve { id } => memex_agent::Command::Approve { job_id: id },
            };
            let response = memex_agent::send_command(&socket()?, &command)
                .map_err(|_| anyhow::anyhow!("daemon_unavailable"))?;
            print_agent_response(response)?;
        }
        Commands::Intake { command } => match command {
            IntakeCommands::Serve {
                library,
                snapshots,
                secret_file,
                origin,
                port,
            } => {
                let secret = memex_intake::read_pairing_secret(secret_file)?;
                let server =
                    memex_intake::IntakeServer::bind(library, snapshots, secret, origin, port)?;
                println!("{}", serde_json::to_string(&server.startup_receipt())?);
                std::io::stdout().flush()?;
                server.serve()?;
            }
        },
        Commands::Serve {
            library,
            max_sensitivity,
            port,
            orb_portal,
            lan_address,
            lan_hostname,
        } => {
            let config = memex_surface::SurfaceConfig {
                library,
                max_sensitivity: max_sensitivity.into(),
                port,
            };
            let server = if orb_portal {
                if std::env::var("AMP_ORB").as_deref() != Ok("1") {
                    anyhow::bail!("--orb-portal is only available inside an Amp orb");
                }
                let public_url = std::env::var("PUBLIC_URL")
                    .map_err(|_| anyhow::anyhow!("PUBLIC_URL is required for --orb-portal"))?;
                memex_surface::SurfaceServer::new_for_orb_portal(config, &public_url)?
            } else if let (Some(address), Some(hostname)) = (lan_address, lan_hostname) {
                let socket = memex_agent::AgentConfig::standard(&home()?).socket;
                memex_surface::SurfaceServer::new_for_private_lan(
                    config, address, &hostname, socket,
                )?
            } else {
                memex_surface::SurfaceServer::new(config)?
            };
            println!("{}", serde_json::to_string(&server.startup_receipt())?);
            std::io::stdout().flush()?;
            server.serve()?;
        }
        Commands::Recall {
            library,
            query,
            max_sensitivity,
            limit,
            format,
        } => {
            let report = memex_analysis::recall(
                &library,
                &memex_analysis::RecallQuery {
                    query,
                    max_sensitivity: max_sensitivity.into(),
                    limit,
                },
            )?;
            match format {
                Format::Json => println!("{}", serde_json::to_string(&report)?),
                Format::Markdown => print!("{}", memex_analysis::render_recall_markdown(&report)),
            }
        }
        Commands::ContextRead {
            library,
            item,
            expected_revision_id,
            start_byte,
            end_byte,
            max_sensitivity,
        } => {
            let result = memex_analysis::context_read(
                &library,
                &memex_analysis::ContextReadRequest {
                    item_id: item,
                    expected_revision_id,
                    start_byte,
                    end_byte,
                    max_sensitivity: max_sensitivity.into(),
                },
            )?;
            println!("{}", serde_json::to_string(&result)?);
        }
        Commands::LibraryQuery {
            socket: requested_socket,
            query,
            source,
            item_type,
            origin,
            sort,
            descending,
            max_sensitivity,
            offset,
            limit,
            view,
        } => {
            let command =
                memex_agent::Command::LibraryQuery(memex_library::LibraryQueryRequest {
                    query: memex_library::Query {
                        text: query,
                        source,
                        item_type,
                        origin: origin.map(|value| serde_json::from_value(value.into())).transpose()?,
                        sort: serde_json::from_value(sort.into())?,
                        descending,
                        group_id: None,
                        include_descendants: false,
                        unfiled: false,
                    },
                    max_sensitivity: max_sensitivity.into(),
                    offset,
                    limit,
                    expected_view: view,
                });
            let path = match requested_socket {
                Some(path) => path,
                None => socket()?,
            };
            let response = memex_agent::send_command(&path, &command)
                .map_err(|_| anyhow::anyhow!("daemon_unavailable"))?;
            print_agent_response(response)?;
        }
        Commands::Analysis { command } => match command {
            AnalysisCommands::Add { library, request } => {
                let request = memex_analysis::read_request(request)?;
                let record = memex_analysis::prepare_record(&library, &request)?;
                let receipt = memex_analysis::apply_record(&library, &record)?;
                println!("{}", serde_json::to_string(&receipt)?);
            }
            AnalysisCommands::Link { library, request } => {
                let request = memex_analysis::read_link_request(request)?;
                let link = memex_analysis::prepare_link(&library, &request)?;
                let receipt = memex_analysis::apply_link(&library, &link)?;
                println!("{}", serde_json::to_string(&receipt)?);
            }
            AnalysisCommands::List { library, format } => {
                let records = memex_analysis::list_records(&library)?;
                match format {
                    Format::Json => println!(
                        "{}",
                        serde_json::to_string(
                            &serde_json::json!({"schema_version":1,"analyses":records})
                        )?
                    ),
                    Format::Markdown => print!(
                        "{}",
                        memex_analysis::render_record_index_markdown(&records)
                    ),
                }
            }
            AnalysisCommands::Show {
                library,
                analysis,
                format,
            } => {
                let record = memex_analysis::read_record(&library, &analysis)?;
                match format {
                    Format::Json => println!(
                        "{}",
                        serde_json::to_string(
                            &serde_json::json!({"schema_version":1,"record":record})
                        )?
                    ),
                    Format::Markdown => {
                        print!("{}", memex_analysis::render_record_markdown(&record))
                    }
                }
            }
        },
        Commands::Publication { command } => match command {
            PublicationCommands::Create { library, request } => {
                let request = memex_publication::read_publication_request(request)?;
                let record = memex_publication::prepare_publication(&library, &request)?;
                println!(
                    "{}",
                    serde_json::to_string(&memex_publication::apply_publication(
                        &library, &record
                    )?)?
                );
            }
            PublicationCommands::List { library, format } => {
                let report = memex_publication::list_publications(&library)?;
                match format {
                    Format::Json => println!("{}", serde_json::to_string(&report)?),
                    Format::Markdown => print!(
                        "{}",
                        memex_publication::render_publication_list_markdown(&report)
                    ),
                }
            }
            PublicationCommands::Show {
                library,
                publication,
                format,
            } => {
                let report = memex_publication::show_publication(&library, &publication)?;
                match format {
                    Format::Json => println!("{}", serde_json::to_string(&report)?),
                    Format::Markdown => print!(
                        "{}",
                        memex_publication::render_publication_markdown(&report.record)
                    ),
                }
            }
            PublicationCommands::ReleasePublic { library, request } => {
                let request = memex_publication::read_public_release_request(request)?;
                println!(
                    "{}",
                    serde_json::to_string(&memex_publication::release_public(
                        &library, &request
                    )?)?
                );
            }
            PublicationCommands::ShowPublic {
                library,
                release,
                format,
            } => {
                let projection = memex_publication::show_public_projection(&library, &release)?;
                match format {
                    Format::Json => println!("{}", serde_json::to_string(&projection)?),
                    Format::Markdown => print!(
                        "{}",
                        memex_publication::render_public_projection_markdown(&projection)
                    ),
                }
            }
        },
        Commands::Source { command } => match command {
            SourceCommands::ImportLegacy {
                package_root,
                profile,
                library,
            } => {
                let prepared = memex_registry::preflight(&package_root, &profile)?;
                let input_files = prepared
                    .iter()
                    .map(|p| serde_json::json!({"relative_path":p.relative_path,"sha256":p.digest}))
                    .collect();
                let mut counts = RegistryCounts {
                    total: prepared.len(),
                    ..Default::default()
                };
                let mut rows = Vec::new();
                let mut pending = prepared.into_iter();
                while let Some(p) = pending.next() {
                    let mut row = registry_row(&p, "pending", None, None);
                    match memex_registry::apply(&library, p) {
                        Ok(receipt) => {
                            match receipt.status {
                                memex_registry::ApplyStatus::Created => counts.created += 1,
                                memex_registry::ApplyStatus::Revised => counts.revised += 1,
                                memex_registry::ApplyStatus::Replayed => counts.replayed += 1,
                            };
                            let mut warnings =
                                row["warning_codes"].as_array().cloned().unwrap_or_default();
                            for warning in receipt.warning_codes {
                                if !warnings.iter().any(|v| v == &serde_json::json!(warning)) {
                                    warnings.push(serde_json::json!(warning));
                                }
                            }
                            row["warning_codes"] = serde_json::Value::Array(warnings);
                            row["revision_id"] = serde_json::json!(receipt.record.revision_id);
                            row["revision_number"] =
                                serde_json::json!(receipt.record.revision_number);
                            row["status"] = serde_json::json!(receipt.status.as_str());
                            rows.push(row);
                        }
                        Err(e) => {
                            row["status"] = serde_json::json!("failed");
                            row["error_code"] = serde_json::json!("registry_apply_failed");
                            rows.push(row);
                            counts.failed += 1;
                            for remaining in pending {
                                rows.push(registry_row(
                                    &remaining,
                                    "unattempted",
                                    None,
                                    Some("not_attempted_after_registry_failure"),
                                ));
                                counts.unattempted += 1;
                            }
                            println!(
                                "{}",
                                serde_json::to_string(&registry_report(
                                    input_files,
                                    &counts,
                                    rows
                                ))?
                            );
                            return Err(e);
                        }
                    }
                }
                println!(
                    "{}",
                    serde_json::to_string(&registry_report(input_files, &counts, rows))?
                );
            }
            SourceCommands::List { library, format } => {
                let records = memex_registry::list_current(&library)?;
                match format {
                    Format::Json => println!(
                        "{}",
                        serde_json::to_string(
                            &serde_json::json!({"schema_version":1,"sources":records})
                        )?
                    ),
                    Format::Markdown => print!("{}", memex_registry::registry_markdown(&records)),
                }
            }
            SourceCommands::Show {
                library,
                source,
                format,
            } => {
                let r = memex_registry::read_current(&library, &source)?;
                match format {
                    Format::Json => println!(
                        "{}",
                        serde_json::to_string(&serde_json::json!({"schema_version":1,"record":r}))?
                    ),
                    Format::Markdown => print!("{}", memex_registry::record_markdown(&r)),
                }
            }
        },
        Commands::Ingest {
            source,
            capture,
            library,
        } => {
            let verified = read_verified_capture(source, capture)?;
            let draft = GenericWebpageExtractor.extract(&verified)?;
            println!(
                "{}",
                serde_json::to_string(&ingest(library, &verified, draft)?)?
            );
        }
        Commands::ShadowImportLegacy {
            package_root,
            meta,
            items,
            library,
        } => {
            // No store call occurs until this has parsed and validated every selected input.
            let preflight = preflight_package(&package_root, &meta, &items)?;
            let total = preflight.items.len();
            let mut created = 0;
            let mut replayed = 0;
            let mut report_items = Vec::with_capacity(total);
            let translated: Vec<_> = preflight
                .items
                .into_iter()
                .map(|prepared| {
                    let draft = GenericWebpageExtractor.extract(&prepared.capture)?;
                    Ok((prepared, draft))
                })
                .collect::<Result<Vec<_>>>()?;
            for (index, (prepared, draft)) in translated.iter().enumerate() {
                let receipt = match ingest(&library, &prepared.capture, draft.clone()) {
                    Ok(receipt) => receipt,
                    Err(error) => {
                        report_items.push(shadow_item(
                            prepared,
                            "failed",
                            None,
                            None,
                            Some("store_ingest_failed"),
                        ));
                        for (remaining, _) in translated.iter().skip(index + 1) {
                            report_items.push(shadow_item(
                                remaining,
                                "unattempted",
                                None,
                                None,
                                Some("not_attempted_after_store_failure"),
                            ));
                        }
                        println!(
                            "{}",
                            serde_json::to_string(&shadow_report(
                                &preflight.run_ref_sha256,
                                &preflight.input_files,
                                total,
                                created,
                                replayed,
                                1,
                                report_items,
                            ))?
                        );
                        return Err(error);
                    }
                };
                let status = if receipt.idempotent_replay {
                    replayed += 1;
                    "replayed"
                } else {
                    created += 1;
                    "created"
                };
                report_items.push(shadow_item(
                    prepared,
                    status,
                    Some(receipt.item_ids[0].clone()),
                    Some(receipt.capture_id),
                    None,
                ));
            }
            println!(
                "{}",
                serde_json::to_string(&shadow_report(
                    &preflight.run_ref_sha256,
                    &preflight.input_files,
                    total,
                    created,
                    replayed,
                    0,
                    report_items,
                ))?
            );
        }
        Commands::Show {
            library,
            item,
            format,
        } => match format {
            ItemFormat::Json => {
                let (item, revision) = read_item(library, &item)?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &serde_json::json!({"schema_version": 1, "item": item, "current_revision": revision})
                    )?
                );
            }
            ItemFormat::Markdown => println!("{}", item_markdown(library, &item)?),
            ItemFormat::PortableJson => print!("{}", item_index_json(library, &item)?),
        },
        Commands::AddedAt { command } => added_at::run(command)?,
    };
    Ok(())
}

/// Source handlers this build links in. The public build registers none; a downstream
/// build adds its own through memex_agent::source::SourceHandler.
fn source_handlers() -> Vec<std::sync::Arc<dyn memex_agent::source::SourceHandler>> {
    #[allow(unused_mut)]
    let mut handlers: Vec<std::sync::Arc<dyn memex_agent::source::SourceHandler>> = Vec::new();
    handlers
}
