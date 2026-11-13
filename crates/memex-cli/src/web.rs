use anyhow::{Context, Result};
use clap::Subcommand;
use std::{fs::File, io::Read, path::PathBuf};

#[derive(Subcommand)]
pub enum Commands {
    /// Inspect supplied HTML; no browser, fetching, script execution or expansion.
    Inspect { #[arg(long)] html: PathBuf, #[arg(long)] url: String },
    /// Convert supplied HTML to inert Markdown, including video poster links.
    Extract { #[arg(long)] html: PathBuf, #[arg(long)] url: String },
    /// One-hop candidates with every citation/Learn more/related occurrence.
    Discover {
        #[arg(long)] html: PathBuf,
        #[arg(long)] url: String,
        /// Optional verified library lookup; collection rechecks under the writer lock.
        #[arg(long)] library: Option<PathBuf>,
    },
    /// Collect a Chrome V1 binary snapshot through the same admission and Store path.
    Collect {
        #[arg(long)] snapshot: PathBuf,
        #[arg(long)] library: PathBuf,
        /// Existing owner-only intake snapshot directory (same one configured in the daemon).
        #[arg(long, required_unless_present = "dry_run")] snapshots: Option<PathBuf>,
        /// Validate only, without writing a library or evidence spool.
        #[arg(long, conflicts_with = "yes")] dry_run: bool,
        /// Explicitly allow writing to the specified library.
        #[arg(long, required_unless_present = "dry_run")] yes: bool,
    },
}

fn bounded(path: &PathBuf, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path).context("open webpage input")?.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= limit, "web_input_size_limit");
    Ok(bytes)
}

pub fn run(command: Commands) -> Result<()> {
    match command {
        Commands::Collect { snapshot, library, snapshots, dry_run, .. } => {
            let bytes = bounded(&snapshot, 36 * 1024 * 1024)?;
            println!("{}", serde_json::to_string_pretty(&memex_intake::web::collect_binary(&library, snapshots.as_deref(), &bytes, dry_run)?)?);
        }
        command => {
            let (html, url, library, mode) = match command {
                Commands::Inspect { html, url } => (html, url, None, "inspect"),
                Commands::Extract { html, url } => (html, url, None, "extract"),
                Commands::Discover { html, url, library } => (html, url, library, "discover"),
                Commands::Collect { .. } => unreachable!(),
            };
            let bytes = bounded(&html, 8 * 1024 * 1024)?;
            let result = lots_scout_extract::web::extract_webpage(&url, std::str::from_utf8(&bytes)?)?;
            if mode == "extract" { print!("{}", result.markdown); }
            else if mode == "inspect" { println!("{}", serde_json::to_string_pretty(&result)?); }
            else {
                let mut candidates = Vec::new();
                for reference in result.references {
                    let existing = library.as_ref().map(|root| memex_store::find_web_capture(root, &reference.url)).transpose()?.flatten();
                    candidates.push(serde_json::json!({"url":reference.url,"occurrences":reference.occurrences,
                        "library_checked":library.is_some(),"existing_item_id":existing.map(|item| item.item_id)}));
                }
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({"schema_version":1,"depth":1,"source_url":url,"candidates":candidates}))?);
            }
        }
    }
    Ok(())
}
