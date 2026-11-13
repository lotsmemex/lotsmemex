//! Reusable one-request mechanical task execution; no automatic retries or fallback.
use crate::file_conversion::{hash, read_bounded, write_new};
use anyhow::{ensure, Context, Result};
use clap::Subcommand;
use memex_models::{low_dial as settings, Purpose, ReasoningEffort, Selection};
use memex_provider::{
    credentials::Credentials,
    generation::{complete_with_optional_effort, TextRequest},
};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    key: String,
    state: Completed,
    cache_hit: bool,
    requests_this_run: u8,
    model: String,
    reported_model: String,
    effort: Option<ReasoningEffort>,
    usage: Option<Usage>,
    result: Value,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Completed {
    Completed,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Usage {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
    prompt_tokens_details: Option<PromptDetails>,
    completion_tokens_details: Option<CompletionDetails>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PromptDetails {
    cached_tokens: Option<u64>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CompletionDetails {
    reasoning_tokens: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InvalidCandidateReason {
    UnmatchedHasIds,
    Count,
    OutsideCandidates,
    NonContiguous,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InvalidTaskResponse {
    Schema,
    SampleCount,
    SampleId,
    CandidateIds {
        sample_id: usize,
        reason: InvalidCandidateReason,
        original_ids_prefix: Vec<usize>,
        translation_ids_prefix: Vec<usize>,
        original_allowed: Vec<usize>,
        translation_allowed: Vec<usize>,
    },
}
impl std::fmt::Display for InvalidTaskResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Schema => "task_response_invalid_schema",
            Self::SampleCount => "task_response_invalid_sample_count",
            Self::SampleId => "task_response_invalid_sample_id",
            Self::CandidateIds { .. } => "task_response_invalid_candidate_ids",
        })
    }
}
impl std::error::Error for InvalidTaskResponse {}

#[derive(Subcommand)]
pub enum Commands {
    /// Read the independent mechanical route, without probing credentials or models.
    Show {
        #[arg(long)]
        database: PathBuf,
    },
    /// Select an existing model; never changes chat/translation routes or calls a model.
    Configure {
        #[arg(long)]
        database: PathBuf,
        #[arg(long)]
        model: String,
        #[arg(long)]
        expected_revision: u64,
        /// omit sends no effort parameter; none sends the explicit provider value.
        #[arg(long, default_value="omit", value_parser=["omit","none","minimal","low"])]
        effort: String,
        #[arg(long, default_value_t = 65536)]
        max_input_bytes: usize,
        #[arg(long, default_value_t = 8192)]
        max_completion_tokens: u32,
    },
}

pub fn run(command: Commands) -> Result<()> {
    let value = match command {
        Commands::Show { database } => {
            let c = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            json!({"profile":settings::read(&c)?})
        }
        Commands::Configure {
            database,
            model,
            expected_revision,
            effort,
            max_input_bytes,
            max_completion_tokens,
        } => {
            let mut c = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
            let reasoning_effort = match effort.as_str() {
                "omit" => None,
                "none" => Some(ReasoningEffort::None),
                "minimal" => Some(ReasoningEffort::Minimal),
                _ => Some(ReasoningEffort::Low),
            };
            json!({"profile":settings::replace(&mut c, expected_revision, settings::Profile {
                revision:0, model, reasoning_effort, max_input_bytes, max_completion_tokens,
            })?, "inference_performed":false})
        }
    };
    println!("{value}");
    Ok(())
}

/// Validators return only task-owned IDs/enums/coordinates, never unchecked model text.
/// The output directory is the retry identity: an incomplete attempt is never resent.
pub fn execute(
    database: &Path,
    out: &Path,
    task: &str,
    prompt: &str,
    input: &Value,
    expected_settings: Option<&Value>,
    validate: impl Fn(&str) -> Result<Value>,
) -> Result<Value> {
    let database = database.canonicalize()?;
    let conn = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let original = memex_models::read(&conn)?;
    let profile = settings::read(&conn)?.context("low_dial_not_configured")?;
    if let Some(expected) = expected_settings {
        ensure!(
            *expected == json!({"models":original,"profile":profile}),
            "stale_low_dial_settings"
        );
    }
    let mut snapshot = original.clone();
    ensure!(
        snapshot
            .configuration
            .models
            .iter()
            .any(|m| m.id == profile.model),
        "unknown_low_dial_model"
    );
    snapshot
        .configuration
        .selections
        .retain(|s| s.purpose != Purpose::CaptureQuality);
    snapshot.configuration.selections.push(Selection {
        purpose: Purpose::CaptureQuality,
        model: profile.model.clone(),
        reasoning_effort: profile.reasoning_effort,
    });
    snapshot.configuration.validate()?;
    let input = serde_json::to_string(input)?;
    ensure!(
        input.len() + prompt.len() <= profile.max_input_bytes,
        "low_dial_input_budget_exceeded"
    );
    let (provider, model) = snapshot
        .configuration
        .target(Purpose::CaptureQuality)
        .context("missing_low_dial_route")?;
    let key = hash(&serde_json::to_vec(
        &json!({"schema":1,"task":task,"profile":profile,
        "database":hash(database.to_string_lossy().as_bytes()),"provider":provider,"model":model,
        "prompt":hash(prompt.as_bytes()),"input":hash(input.as_bytes())}),
    )?);
    if out.try_exists()? {
        ensure!(
            fs::symlink_metadata(out)?.is_dir()
                && !fs::symlink_metadata(out)?.file_type().is_symlink(),
            "invalid_low_dial_cache"
        );
        let receipt = read_bounded(&out.join("result.json"), 128 * 1024)
            .map_err(|_| anyhow::anyhow!("low_dial_previous_attempt_no_retry"))?;
        let mut cached: Receipt = serde_json::from_slice(&receipt)
            .map_err(|_| anyhow::anyhow!("invalid_low_dial_cache"))?;
        ensure!(
            cached.key == key
                && cached.model == model.model_id
                && cached.reported_model == model.model_id
                && cached.effort == profile.reasoning_effort
                && !cached.cache_hit
                && cached.requests_this_run == 1,
            "low_dial_cache_mismatch"
        );
        // Revalidate even locally cached output against the current task inputs.
        cached.result = validate(&serde_json::to_string(&cached.result)?)?;
        cached.cache_hit = true;
        cached.requests_this_run = 0;
        return Ok(serde_json::to_value(cached)?);
    }
    fs::create_dir(out)?;
    fs::File::open(
        out.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )?
    .sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(out, fs::Permissions::from_mode(0o700))?;
    }
    write_new(
        &out.join("intent.json"),
        &serde_json::to_vec(&json!({"key":key,"task":task,
        "input_bytes":input.len(),"system_bytes":prompt.len(),"request_limit":1,"profile":profile}))?,
    )?;
    fs::File::open(out)?.sync_all()?;
    let mut stage = "credential";
    let result = (|| -> Result<Value> {
        let vault = Credentials::for_database(&database, &provider.id);
        let status = vault.status(provider, snapshot.revision)?;
        let credential = vault.resolve(provider, status.generation)?;
        ensure!(
            memex_models::read(&conn)? == original
                && settings::read(&conn)?.as_ref() == Some(&profile),
            "stale_low_dial_settings"
        );
        write_new(
            &out.join("dispatch.json"),
            &serde_json::to_vec(&json!({"key":key,
            "state":"dispatched_outcome_unknown","model":model.model_id,
            "credential_generation":status.generation}))?,
        )?;
        fs::File::open(out)?.sync_all()?;
        stage = "provider";
        let response = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(complete_with_optional_effort(
                &snapshot,
                &credential,
                TextRequest {
                    purpose: Purpose::CaptureQuality,
                    system: prompt,
                    input: &input,
                    max_completion_tokens: profile.max_completion_tokens,
                },
            ))?;
        // A received response can be billed even when its task payload is invalid.
        // Preserve only typed provider metadata, never the untrusted response text.
        stage = "response_receipt";
        write_new(
            &out.join("response.json"),
            &serde_json::to_vec(&json!({
                "key":key,"state":"response_received_not_task_accepted",
                "model":response.requested_model,"reported_model":response.reported_model,
                "effort":response.requested_effort,"usage":response.usage,
                "json_syntax_valid":serde_json::from_str::<Value>(&response.text).is_ok(),
            }))?,
        )?;
        fs::File::open(out)?.sync_all()?;
        stage = "task_validation";
        let checked = validate(&response.text)?;
        stage = "settings_recheck";
        ensure!(
            memex_models::read(&conn)? == original
                && settings::read(&conn)?.as_ref() == Some(&profile),
            "stale_low_dial_settings"
        );
        ensure!(
            vault.status(provider, snapshot.revision)?.generation == status.generation,
            "stale_low_dial_credential"
        );
        Ok(
            json!({"key":key,"state":"completed","cache_hit":false,"requests_this_run":1,
            "model":response.requested_model,"reported_model":response.reported_model,
            "effort":response.requested_effort,"usage":response.usage,"result":checked}),
        )
    })();
    match result {
        Ok(value) => {
            write_new(
                &out.join("result.json"),
                &serde_json::to_vec_pretty(&value)?,
            )?;
            fs::File::open(out)?.sync_all()?;
            Ok(value)
        }
        Err(error) => {
            // Do not leak source-bearing parser or provider errors. Never retry here.
            // These two typed errors render fixed codes only, never raw responses.
            let code = if let Some(e) =
                error.downcast_ref::<memex_provider::credentials::CredentialError>()
            {
                e.to_string()
            } else if let Some(e) =
                error.downcast_ref::<memex_provider::generation::GenerationError>()
            {
                e.to_string()
            } else if let Some(e) = error.downcast_ref::<InvalidTaskResponse>() {
                e.to_string()
            } else {
                "low_dial_execution_failed_no_retry".into()
            };
            let dispatched = out.join("dispatch.json").try_exists()?;
            write_new(
                &out.join("failure.json"),
                &serde_json::to_vec(&json!({"key":key,"code":code,"stage":stage,
                "task_error":error.downcast_ref::<InvalidTaskResponse>(),
                "state":if dispatched {"failed_or_unknown_no_retry"} else {"not_dispatched"}}))?,
            )?;
            fs::File::open(out)?.sync_all()?;
            anyhow::bail!("low_dial_execution_failed_no_retry")
        }
    }
}
