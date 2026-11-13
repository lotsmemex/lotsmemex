//! Owner-local native data import. No daemon, provider or network entry points.
use anyhow::{ensure, Context, Result};
use clap::Subcommand;
use memex_core::Sensitivity;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
mod items;

#[derive(Subcommand)]
pub enum ImportCommands {
    /// Freeze one group from an existing SQLite backup, converting local content offline.
    PrepareZotero {
        #[arg(long)]
        snapshot: PathBuf,
        #[arg(long)]
        storage: PathBuf,
        #[arg(long)]
        group: u64,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        python: PathBuf,
    },
    /// Complete DOCX/PDF text extraction into a separate final bundle manifest.
    SupplementZotero {
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long)]
        python: PathBuf,
    },
    /// Propose editable topic/tag folders without mutating the Library.
    PlanTopics {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        root_name: String,
        /// Stable owner-chosen identity for this organization, independent of display name.
        #[arg(long)]
        namespace: String,
        #[arg(long)]
        python: PathBuf,
    },
    /// Bind the original folder hierarchy to successfully imported item receipts.
    PlanZoteroGroups {
        #[arg(long)]
        hierarchy: PathBuf,
        #[arg(long)]
        receipts: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        python: PathBuf,
    },
    /// Materialize frozen Zotero records through normal Store ingest journals.
    Items {
        #[arg(long)]
        plan: PathBuf,
        #[arg(long)]
        library: PathBuf,
        #[arg(long)]
        receipts: PathBuf,
    },
    /// Atomically create a frozen native group tree/membership plan; never overwrite.
    Groups {
        #[arg(long)]
        plan: PathBuf,
        #[arg(long)]
        library: PathBuf,
    },
    /// Export only live Evernote receipt-linked items for local organization.
    /// The output contains private text; it is never printed or sent to a model.
    ExportEvernote {
        #[arg(long)]
        library: PathBuf,
        #[arg(long)]
        receipts: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
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

// Scripts ship inside the executable: no source checkout or user-specific path
// is required. Planning has no Store handles and cannot call model providers.
fn python_tool(name: &str, args: &[&std::ffi::OsStr], python: &Path) -> Result<()> {
    use std::process::{Command, Stdio};
    let scratch = tempfile::tempdir()?;
    let tools = scratch.path().join("native_import");
    fs::create_dir(&tools)?;
    for (name, source) in [
        ("zotero.py", include_str!("zotero.py")),
        ("zotero_supplement.py", include_str!("zotero_supplement.py")),
        ("zotero_groups.py", include_str!("zotero_groups.py")),
        ("topics.py", include_str!("topics.py")),
    ] {
        write_new(&tools.join(name), source.as_bytes())?;
    }
    let converters = scratch.path().join("file_conversion");
    fs::create_dir(&converters)?;
    write_new(
        &converters.join("convert.py"),
        include_bytes!("../file_conversion/convert.py"),
    )?;
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("/usr/bin/sandbox-exec");
        command.args(["-p", "(version 1) (allow default) (deny network*)"]);
        command.arg(python);
        command
    };
    #[cfg(not(target_os = "macos"))]
    anyhow::bail!("offline import planning currently requires macOS sandbox");
    #[cfg(target_os = "macos")]
    {
        let status = command
            .args(["-I", "-c", "import sys,runpy; sys.path.insert(0,sys.argv[1]); sys.argv=sys.argv[2:]; runpy.run_path(sys.argv[0],run_name='__main__')"])
            .arg(&tools).arg(tools.join(name)).args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("OPENBLAS_NUM_THREADS", "1").env("OMP_NUM_THREADS", "1")
            .stdin(Stdio::null()).status().context("offline planner unavailable")?;
        ensure!(status.success(), "offline_plan_failed");
        Ok(())
    }
}

fn export_evernote(library: &Path, receipts: &Path, out: &Path) -> Result<()> {
    let mut rows = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut excluded = 0;
    memex_store::with_consistent_snapshot(library, |library| {
        for line in fs::read_to_string(receipts)?.lines() {
            let row: serde_json::Value = serde_json::from_str(line)?;
            let key = row["record_id"]
                .as_str()
                .context("invalid import receipt")?;
            let receipt: memex_core::IngestReceipt =
                serde_json::from_value(row["receipt"].clone())?;
            ensure!(
                receipt.item_ids.len() == 1
                    && receipt.capture_id.starts_with("capture_")
                    && receipt
                        .capture_id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
                "invalid import identity"
            );
            let record: memex_core::CaptureRecord = serde_json::from_slice(&fs::read(
                library
                    .join("captures")
                    .join(&receipt.capture_id)
                    .join("capture.json"),
            )?)?;
            ensure!(
                record.capture_id == receipt.capture_id
                    && record.adapter_id == "memex.local-evernote-import"
                    && record.source_id == "source_evernote_import_v1"
                    && record.external_capture_id == key,
                "source mapping mismatch"
            );
            let canonical: memex_core::IngestReceipt = serde_json::from_slice(&fs::read(
                library
                    .join("receipts")
                    .join(format!("{}.json", receipt.capture_id)),
            )?)?;
            ensure!(
                canonical.item_ids == receipt.item_ids
                    && canonical.evidence_ids == receipt.evidence_ids,
                "source receipt mismatch"
            );
            let id = &receipt.item_ids[0];
            if !seen.insert(id.clone()) {
                continue;
            }
            let Some((item, revision)) =
                memex_store::read_item_at_sensitivity(library, id, Sensitivity::Private)?
            else {
                excluded += 1;
                continue;
            };
            rows.push(serde_json::json!({
                "item_id": item.item_id, "revision_id": revision.revision_id,
                "source_key": key, "title": revision.title, "content": revision.content_markdown,
                "tags": revision.metadata.manual_tags.map(|s| s.value).unwrap_or_default(),
            }));
        }
        Ok(())
    })?;
    write_new(out, &serde_json::to_vec(&rows)?)?;
    println!(
        "{}",
        serde_json::json!({"exported": rows.len(), "excluded": excluded})
    );
    Ok(())
}

pub fn run(command: ImportCommands) -> Result<()> {
    match command {
        ImportCommands::PrepareZotero {
            snapshot,
            storage,
            group,
            out,
            python,
        } => {
            python_tool(
                "zotero.py",
                &[
                    "--snapshot".as_ref(),
                    snapshot.as_os_str(),
                    "--storage".as_ref(),
                    storage.as_os_str(),
                    "--group".as_ref(),
                    group.to_string().as_ref(),
                    "--out".as_ref(),
                    out.as_os_str(),
                ],
                &python,
            )?;
        }
        ImportCommands::SupplementZotero { bundle, python } => {
            python_tool("zotero_supplement.py", &[bundle.as_os_str()], &python)?;
        }
        ImportCommands::PlanTopics {
            input,
            out,
            root_name,
            namespace,
            python,
        } => {
            python_tool(
                "topics.py",
                &[
                    input.as_os_str(),
                    out.as_os_str(),
                    root_name.as_ref(),
                    namespace.as_ref(),
                ],
                &python,
            )?;
        }
        ImportCommands::PlanZoteroGroups {
            hierarchy,
            receipts,
            out,
            python,
        } => {
            python_tool(
                "zotero_groups.py",
                &[hierarchy.as_os_str(), receipts.as_os_str(), out.as_os_str()],
                &python,
            )?;
        }
        ImportCommands::Items {
            plan,
            library,
            receipts,
        } => items::apply(&plan, &library, &receipts)?,
        ImportCommands::Groups { plan, library } => {
            let plan: memex_store::NativeGroupImport = serde_json::from_slice(&fs::read(plan)?)?;
            let receipt = memex_store::import_native_groups(library, &plan, Sensitivity::Private)?;
            println!("{}", serde_json::to_string(&receipt)?);
        }
        ImportCommands::ExportEvernote {
            library,
            receipts,
            out,
        } => export_evernote(&library, &receipts, &out)?,
    }
    Ok(())
}
