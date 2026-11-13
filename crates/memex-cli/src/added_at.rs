//! `memex added-at backfill`: guarded preview/apply of legacy added_at.
//!
//! Dry run writes nothing to the library: it prints the four status counts and
//! writes the full preview to `--out`. Apply consumes that preview file; each
//! row is re-validated under the Store writer lock and the durable receipt is
//! written inside the library (`catalog/added-at-backfill/<preview_id>.json`).

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use std::{fs, io::Write, path::PathBuf};

#[derive(Subcommand)]
pub enum AddedAtCommands {
    /// Fill missing added_at from each item's creating capture time.
    Backfill(BackfillArgs),
}

#[derive(Args)]
#[command(group(clap::ArgGroup::new("mode").required(true).args(["dry_run", "apply"])))]
pub struct BackfillArgs {
    /// Classify items and write a preview file; the library is not modified.
    #[arg(long)]
    dry_run: bool,
    /// Apply a preview written by --dry-run.
    #[arg(long, requires = "preview")]
    apply: bool,
    /// Preview file produced by --dry-run (apply only).
    #[arg(long, conflicts_with = "dry_run")]
    preview: Option<PathBuf>,
    /// Where to write the preview (dry run, required) or a receipt copy (apply).
    #[arg(long, required_if_eq("dry_run", "true"))]
    out: Option<PathBuf>,
    /// Library root.
    library: PathBuf,
}

fn write_new(path: &PathBuf, bytes: &[u8]) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("refusing to overwrite or create {}", path.display()))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

pub fn run(command: AddedAtCommands) -> Result<()> {
    let AddedAtCommands::Backfill(args) = command;
    if args.dry_run {
        let preview = memex_store::preview_added_at_backfill(&args.library)?;
        let out = args.out.context("--out is required with --dry-run")?;
        write_new(&out, &serde_json::to_vec_pretty(&preview)?)?;
        println!(
            "{}",
            serde_json::json!({
                "mode": "dry_run",
                "preview_id": preview.preview_id,
                "total_items": preview.total_items,
                "status_counts": preview.status_counts,
                "preview_file": out,
            })
        );
        return Ok(());
    }
    let path = args.preview.context("--preview is required with --apply")?;
    let preview: memex_store::AddedAtBackfillPreview =
        serde_json::from_slice(&fs::read(&path)?).context("invalid preview file")?;
    let receipt = memex_store::apply_added_at_backfill(&args.library, &preview)?;
    if let Some(out) = &args.out {
        write_new(out, &serde_json::to_vec_pretty(&receipt)?)?;
    }
    println!(
        "{}",
        serde_json::json!({
            "mode": "apply",
            "preview_id": receipt.preview_id,
            "replayed": receipt.replayed,
            "status_counts": receipt.status_counts,
            "apply_counts": receipt.apply_counts,
            "library_receipt": receipt.library_receipt,
        })
    );
    Ok(())
}
