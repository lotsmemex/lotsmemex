//! Offline file conversion is isolated from daemon/provider and Store writes.
pub(crate) mod alignment;
mod quality;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const ADAPTER: &str = concat!(include_str!("pdf_text.py"), "\n", include_str!("convert.py"));
const MAX_PDF: u64 = 100 * 1024 * 1024;
const MAX_TEXT: u64 = 16 * 1024 * 1024;

#[derive(Subcommand)]
pub enum FileCommands {
    /// Convert one local PDF offline. Never imports or overwrites existing output.
    Convert(ConvertArgs),
    /// Audit Markdown from PDF, Word or web extraction; optional bounded model sampling.
    AuditFormat(quality::AuditArgs),
    /// Sparse bilingual anchors for two Markdown sections; offline preview by default.
    AlignSamples(alignment::Args),
    /// Replace one existing item's reading projection, preserving identity/history.
    /// Requires compatible Store readers; does not deploy or restart services.
    ApplyConversion(ApplyArgs),
}

#[derive(Args)]
pub struct ConvertArgs {
    input: PathBuf,
    #[arg(long)]
    out: PathBuf,
    /// Python environment containing markitdown[pdf]==0.1.9. No automatic installation.
    #[arg(long)]
    python: Option<PathBuf>,
}

#[derive(Args)]
pub struct ApplyArgs {
    #[arg(long)]
    bundle: PathBuf,
    #[arg(long)]
    library: PathBuf,
    #[arg(long)]
    item: String,
    #[arg(long)]
    expected_revision_id: String,
    #[arg(long, value_enum)]
    max_sensitivity: crate::CliSensitivity,
}

pub(crate) fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn valid_engine(engine: &serde_json::Value) -> bool {
    (engine["engine"] == "microsoft/markitdown"
        && engine["engine_version"] == "0.1.9"
        && matches!(engine["adapter_version"].as_u64(), Some(1 | 2)))
        || (engine["engine"] == "memex/pdf-layout"
            && engine["engine_version"] == "1"
            && engine["adapter_version"] == 3)
}

pub(crate) fn read_bounded(path: &Path, max: u64) -> Result<Vec<u8>> {
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        anyhow::bail!("symlink input rejected")
    }
    let file = fs::File::open(path)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > max {
        anyhow::bail!("invalid file size/type")
    }
    let mut bytes = Vec::new();
    file.take(max + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        anyhow::bail!("file size limit")
    }
    Ok(bytes)
}

pub(crate) fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
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

fn convert(args: ConvertArgs) -> Result<()> {
    let bytes = read_bounded(&args.input, MAX_PDF)?;
    if !bytes.starts_with(b"%PDF-") {
        anyhow::bail!("not a PDF")
    }
    if args.out.try_exists()? {
        anyhow::bail!("output already exists")
    }
    let python = args
        .python
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(".local/share/memex/file-converter/bin/python"))
        })
        .context("provide --python with markitdown[pdf]==0.1.9")?;
    let scratch = tempfile::tempdir()?;
    let input = scratch.path().join("input.pdf");
    write_new(&input, &bytes)?;
    let stderr = scratch.path().join("stderr");
    // This bounded Mac capability is fail-closed if the OS network sandbox is
    // unavailable. Neither inherited proxy/provider variables nor PDF actions
    // can turn conversion into a network request.
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut c = Command::new("/usr/bin/sandbox-exec");
        c.args(["-p", "(version 1) (allow default) (deny network*)"]);
        c.arg(&python);
        c
    };
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (python, input, stderr);
        anyhow::bail!("offline file converter currently requires macOS sandbox")
    }
    #[cfg(target_os = "macos")]
    {
        command
            .args(["-I", "-c", ADAPTER])
            .arg(&input)
            .arg(scratch.path())
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(&stderr)?);
        let mut child = command
            .spawn()
            .context("converter unavailable; install pinned Python dependencies")?;
        let deadline = Instant::now() + Duration::from_secs(120);
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill()?;
                child.wait()?;
                anyhow::bail!("conversion_timeout")
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        if !status.success() {
            let message = fs::read_to_string(&stderr).unwrap_or_default();
            let reason = [
                "ocr_required",
                "no_extractable_text",
                "unmapped_pdf_glyphs",
                "converter_version_mismatch",
                "unsupported_pdf_actual_text_scope",
                "unsupported_pdf_actual_text_nesting",
                "ambiguous_pdf_actual_text",
                "unresolved_pdf_actual_text",
            ]
            .into_iter()
            .find(|r| message.lines().any(|l| l == *r))
            .unwrap_or("pdf_conversion_failed");
            anyhow::bail!("{reason}")
        }
        let markdown = read_bounded(&scratch.path().join("document.md"), MAX_TEXT)?;
        let raw = read_bounded(&scratch.path().join("raw.md"), MAX_TEXT)?;
        let engine: serde_json::Value = serde_json::from_slice(&read_bounded(
            &scratch.path().join("engine.json"),
            64 * 1024,
        )?)?;
        let receipt = serde_json::json!({"schema_version":1, "input_sha256":hash(&bytes),
            "markdown_sha256":hash(&markdown), "raw_sha256":hash(&raw), "markdown_bytes":markdown.len(),
            "engine":engine, "item_type":"document"});
        fs::create_dir(&args.out)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&args.out, fs::Permissions::from_mode(0o700))?;
        }
        for (name, content) in [
            ("original.pdf", bytes),
            ("raw.md", raw),
            ("document.md", markdown),
            ("conversion.json", serde_json::to_vec_pretty(&receipt)?),
        ] {
            write_new(&args.out.join(name), &content)?;
        }
        fs::File::open(&args.out)?.sync_all()?;
        println!("{}", receipt);
        Ok(())
    }
}

pub fn run(command: FileCommands) -> Result<()> {
    match command {
        FileCommands::Convert(args) => convert(args),
        FileCommands::AuditFormat(args) => quality::run(args),
        FileCommands::AlignSamples(args) => alignment::run(args),
        FileCommands::ApplyConversion(args) => {
            let receipt: serde_json::Value = serde_json::from_slice(&read_bounded(
                &args.bundle.join("conversion.json"),
                64 * 1024,
            )?)?;
            let pdf = read_bounded(&args.bundle.join("original.pdf"), MAX_PDF)?;
            let markdown = read_bounded(&args.bundle.join("document.md"), MAX_TEXT)?;
            if receipt["schema_version"] != 1
                || !valid_engine(&receipt["engine"])
                || receipt["input_sha256"] != hash(&pdf)
                || receipt["markdown_sha256"] != hash(&markdown)
            {
                anyhow::bail!("conversion bundle integrity mismatch")
            }
            let update = memex_store::FileReconversion {
                expected_revision_id: args.expected_revision_id,
                original_pdf_sha256: hash(&pdf),
                markdown: String::from_utf8(markdown)?,
            };
            let applied = memex_store::reconvert_file_item(
                args.library,
                &args.item,
                update,
                args.max_sensitivity.into(),
            )?;
            println!("{}", serde_json::to_string(&applied)?);
            Ok(())
        }
    }
}
