//! Thin owner-IPC client: no Store writes, model configuration or local job queue.
use anyhow::{anyhow, ensure, Result};
use clap::Subcommand;
use serde_json::{json, Value};
use std::{
    fs,
    io::{Read, Write},
    path::PathBuf,
};

#[derive(clap::Args)]
pub struct Args {
    #[arg(long)]
    socket: Option<PathBuf>,
    #[arg(long, value_enum)]
    max_sensitivity: crate::CliSensitivity,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Local-only preview. Selection file contains only the frozen selection shape.
    Preview {
        #[arg(
            long,
            required_unless_present = "preview_id",
            conflicts_with = "preview_id"
        )]
        selection: Option<PathBuf>,
        #[arg(long)]
        preview_id: Option<String>,
    },
    Targets {
        #[arg(long)]
        preview_id: String,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u16).range(1..=100))]
        limit: u16,
    },
    /// Confirm the displayed quote's scope, cost and selected-text use on saved Low dial.
    Request {
        #[arg(long)]
        quote_id: String,
        #[arg(long)]
        request_id: String,
        #[arg(long, required = true)]
        accept_cost: bool,
        /// Explicit consent to send selected text (including private targets) to saved Low dial.
        #[arg(long, required = true)]
        allow_model: bool,
    },
    Status {
        #[arg(long)]
        batch_id: String,
    },
    Results {
        #[arg(long)]
        batch_id: String,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u16).range(1..=100))]
        limit: u16,
    },
    /// Read change metadata only; never prints before/after text.
    Changes {
        #[arg(long)]
        batch_id: String,
        #[arg(long)]
        target_id: String,
        #[arg(long)]
        expected_version: String,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long, default_value_t = 64, value_parser = clap::value_parser!(u16).range(1..=64))]
        limit: u16,
    },
    /// Explicit local-only before/after chunk export to a NEW mode-0600 JSON file.
    ChangeText {
        #[arg(long)]
        batch_id: String,
        #[arg(long)]
        target_id: String,
        #[arg(long)]
        expected_version: String,
        #[arg(long)]
        change_id: String,
        #[arg(long, value_parser = ["before", "after"])]
        part: String,
        #[arg(long, default_value_t = 0)]
        start_byte: usize,
        #[arg(long, default_value_t = 8192, value_parser = clap::value_parser!(u16).range(1..=8192))]
        limit_bytes: u16,
        #[arg(long)]
        out: PathBuf,
    },
    Restore {
        #[arg(long)]
        batch_id: String,
        #[arg(long)]
        target_id: String,
        #[arg(long)]
        expected_revision_id: String,
        #[arg(long)]
        request_id: String,
    },
    /// Resume only the original confirmed batch and remaining budget; never retry unknown calls.
    Resume {
        #[arg(long)]
        batch_id: String,
        #[arg(long, required = true)]
        allow_model: bool,
    },
}

fn request(command: Commands) -> Result<(memex_agent::ipc::ReaderRequest, Option<PathBuf>)> {
    let mut out = None;
    let value = match command {
        Commands::Preview {
            selection,
            preview_id,
        } => match selection {
            Some(path) => {
                let meta = fs::symlink_metadata(&path)
                    .map_err(|_| anyhow!("invalid_content_format_request"))?;
                ensure!(
                    meta.is_file() && !meta.file_type().is_symlink() && meta.len() <= 16384,
                    "invalid_content_format_request"
                );
                let mut bytes = Vec::new();
                fs::File::open(path)
                    .and_then(|file| file.take(16385).read_to_end(&mut bytes))
                    .map_err(|_| anyhow!("invalid_content_format_request"))?;
                ensure!(bytes.len() <= 16384, "invalid_content_format_request");
                let selection: Value = serde_json::from_slice(&bytes)
                    .map_err(|_| anyhow!("invalid_content_format_request"))?;
                json!({"op":"format_preview", "selection": selection})
            }
            None => json!({"op":"format_preview", "preview_id": preview_id}),
        },
        Commands::Targets {
            preview_id,
            offset,
            limit,
        } => json!({"op":"format_targets", "preview_id":preview_id,"offset":offset,"limit":limit}),
        Commands::Request {
            quote_id,
            request_id,
            accept_cost,
            allow_model,
        } => {
            ensure!(
                accept_cost && allow_model,
                "content_format_confirmation_required"
            );
            json!({"op":"format_request","quote_id":quote_id,"request_id":request_id,"accept_cost":true,"accept_model_processing":true})
        }
        Commands::Status { batch_id } => json!({"op":"format_status","batch_id":batch_id}),
        Commands::Results {
            batch_id,
            offset,
            limit,
        } => json!({"op":"format_results","batch_id":batch_id,"offset":offset,"limit":limit}),
        Commands::Changes {
            batch_id,
            target_id,
            expected_version,
            offset,
            limit,
        } => {
            json!({"op":"format_changes","view":"list","batch_id":batch_id,"target_id":target_id,"expected_version":expected_version,"offset":offset,"limit":limit})
        }
        Commands::ChangeText {
            batch_id,
            target_id,
            expected_version,
            change_id,
            part,
            start_byte,
            limit_bytes,
            out: path,
        } => {
            out = Some(path);
            json!({"op":"format_changes","view":"text","batch_id":batch_id,"target_id":target_id,"expected_version":expected_version,"change_id":change_id,"part":part,"start_byte":start_byte,"limit_bytes":limit_bytes})
        }
        Commands::Restore {
            batch_id,
            target_id,
            expected_revision_id,
            request_id,
        } => {
            json!({"op":"format_restore","batch_id":batch_id,"target_id":target_id,"expected_revision_id":expected_revision_id,"request_id":request_id})
        }
        Commands::Resume {
            batch_id,
            allow_model,
        } => {
            ensure!(allow_model, "content_format_confirmation_required");
            json!({"op":"format_resume","batch_id":batch_id})
        }
    };
    // Deserialize the owner's authoritative strict types instead of maintaining a second contract.
    let request =
        serde_json::from_value(value).map_err(|_| anyhow!("invalid_content_format_request"))?;
    Ok((request, out))
}

pub fn run(args: Args) -> Result<()> {
    let (request, out) = request(args.command)?;
    let socket = args.socket.map(Ok).unwrap_or_else(crate::socket)?;
    let response = memex_agent::send_command(
        &socket,
        &memex_agent::Command::Reader {
            max_sensitivity: args.max_sensitivity.into(),
            principal: None,
            request,
        },
    )
    .map_err(|_| anyhow!("daemon_unavailable"))?;
    if !response.ok {
        // Never echo an arbitrary daemon/provider error string.
        let code = response.reason_code.as_deref().unwrap_or("");
        let safe = match code {
            "invalid_content_format_request"
            | "content_format_unavailable"
            | "content_format_preview_not_found"
            | "content_format_quote_stale"
            | "content_format_quote_consumed"
            | "content_format_batch_not_found"
            | "content_format_changes_stale"
            | "content_format_restore_conflict"
            | "content_format_invalid_result"
            | "permission_denied"
            | "library_resource_not_found"
            | "reader_job_in_progress" => code,
            _ => "content_format_request_failed",
        };
        return Err(anyhow!(safe.to_owned()));
    }
    let value = response
        .reader
        .ok_or_else(|| anyhow!("invalid_content_format_response"))?;
    let bytes =
        serde_json::to_vec(&value).map_err(|_| anyhow!("invalid_content_format_response"))?;
    if let Some(path) = out {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(path)
            .map_err(|_| anyhow!("content_format_export_failed"))?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| anyhow!("content_format_export_failed"))?;
        println!("{{\"exported\":true}}");
    } else {
        println!(
            "{}",
            String::from_utf8(bytes).map_err(|_| anyhow!("invalid_content_format_response"))?
        );
    }
    Ok(())
}
