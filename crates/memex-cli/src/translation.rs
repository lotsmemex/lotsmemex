//! Explicit, revision-bound table backfill, never a whole-document translation retry.
use anyhow::{ensure, Result};
use clap::Subcommand;
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum Commands {
    /// Plan/status by default. --execute --yes authorizes only the listed item's missing tables.
    RepairTables {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        item: String,
        #[arg(long)]
        expected_analysis: String,
        #[arg(long, requires = "yes")]
        execute: bool,
        #[arg(long, requires = "execute")]
        yes: bool,
        /// Explicitly continue this known-invalid repair once: at most two more requests,
        /// three total. Unknown/in-flight outcomes are never eligible.
        #[arg(long, requires = "execute")]
        resume_invalid: Option<String>,
        /// Revalidate and reuse this exact known response without another grant or request
        /// for that table. Only the existing remaining budget can dispatch subsequent tables.
        #[arg(long, requires = "resume_invalid")]
        revalidate_response_sha256: Option<String>,
    },
}

pub fn run(command: Commands) -> Result<()> {
    let Commands::RepairTables {
        config,
        item,
        expected_analysis,
        execute,
        yes: _,
        resume_invalid,
        revalidate_response_sha256,
    } = command;
    let config = memex_agent::AgentConfig::read(&config)?;
    config.validate()?;
    let result = if execute {
        memex_agent::repair_saved_translation_tables(
            &config,
            &item,
            &expected_analysis,
            resume_invalid.as_deref(),
            revalidate_response_sha256.as_deref(),
        )?
    } else {
        memex_analysis::translation_table_repair_plan(&config.library, &item, &expected_analysis)?
    };
    println!("{}", serde_json::to_string_pretty(&result)?);
    ensure!(!execute || result.plan.tables == 0 || result.state == "succeeded",
        "translation_table_repair_not_completed; inspect the same request identity before taking further action");
    Ok(())
}
