//! Explicit operator-only public-input trial; never runs from the daemon or GUI.
//! See interfaces/memex.prompt_trial.md before using real credentials.
use anyhow::{bail, ensure, Result};
use memex_analysis::{
    CLASSICAL_TRANSLATION_PROMPT, FAITHFUL_TRANSLATION_PROMPT, GROUNDED_ANSWER_PROMPT,
};
use memex_models::{Purpose, ReasoningEffort};
use memex_provider::{
    credentials::Credentials,
    generation::{complete, TextRequest},
};
use rusqlite::{Connection, OpenFlags};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
    time::Instant,
};

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn append(file: &mut File, value: serde_json::Value) -> Result<()> {
    serde_json::to_writer(&mut *file, &value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

fn private_file(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.is_file()
            && meta.nlink() == 1
            && meta.mode() & 0o077 == 0
            && meta.uid() == unsafe { libc::geteuid() }
            && path.canonicalize()? == path,
        "unsafe_trial_path"
    );
    Ok(())
}

fn run() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    ensure!(args.len() == 8, "usage: prompt_trial ABS_DB REVISION GENERATION PROFILE ABS_APPROVED_INPUT SHA256 ABS_NEW_RECEIPT");
    let database = Path::new(&args[1]);
    private_file(database)?;
    let expected_revision: u64 = args[2].parse()?;
    let generation: u64 = args[3].parse()?;
    let (system, max_tokens) = match args[4].as_str() {
        "translation-baseline-v1" => (FAITHFUL_TRANSLATION_PROMPT, 10000),
        "translation-classical-v1" => (CLASSICAL_TRANSLATION_PROMPT, 10000),
        // No old product Q&A prompt exists. This is an explicit trial control,
        // not a historical, verified production prompt.
        "answer-control-v1" => (
            "依据给定文章，用简体中文回答用户问题，准确、清楚，不编造。",
            2500,
        ),
        "answer-grounded-v1" => (GROUNDED_ANSWER_PROMPT, 2500),
        _ => bail!("unknown_trial_profile"),
    };
    let input_path = Path::new(&args[5]);
    private_file(input_path)?;
    let mut bytes = Vec::new();
    File::open(input_path)?
        .take(65537)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 65536 && digest(&bytes) == args[6],
        "unapproved_trial_input"
    );
    let input = String::from_utf8(bytes)?;
    ensure!(!input.trim().is_empty(), "empty_trial_input");

    // Only the namespaced model settings table is read. Do not open Queue,
    // initialize schemas, create a library, ingest, or consume any queued job.
    let connection = Connection::open_with_flags(
        database,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    connection.execute_batch("PRAGMA query_only=ON;")?;
    let snapshot = memex_models::read(&connection)?;
    ensure!(
        snapshot.revision == expected_revision,
        "stale_model_settings"
    );
    let (provider, model) = snapshot
        .configuration
        .target(Purpose::Translation)
        .ok_or_else(|| anyhow::anyhow!("missing_translation_route"))?;
    ensure!(
        provider.id == "local-cliproxy"
            && provider.api_base == "http://127.0.0.1:8317/v1/"
            && model.model_id == "gpt-6-astra"
            && snapshot
                .configuration
                .selections
                .iter()
                .any(|s| s.purpose == Purpose::Translation
                    && s.reasoning_effort == Some(ReasoningEffort::Low)),
        "unapproved_trial_target"
    );

    let receipt_path = Path::new(&args[7]);
    let parent = receipt_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("invalid_receipt"))?;
    let meta = fs::symlink_metadata(parent)?;
    ensure!(
        meta.is_dir()
            && meta.mode() & 0o077 == 0
            && meta.uid() == unsafe { libc::geteuid() }
            && parent.canonicalize()? == parent,
        "unsafe_receipt_directory"
    );
    // A reused receipt fails before Keychain/network. A crash leaves an honest
    // pending/unknown record; do not choose a new filename to blindly replay it.
    let mut receipt = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(receipt_path)?;
    File::open(parent)?.sync_all()?;
    append(
        &mut receipt,
        json!({"state":"prepared", "profile":args[4],
        "input_sha256":args[6], "prompt_sha256":digest(system.as_bytes()),
        "provider":provider.id, "api_base":provider.api_base,
        "requested_model":model.model_id, "requested_effort":"low",
        "settings_revision":expected_revision, "credential_generation":generation,
        "max_completion_tokens":max_tokens, "price_known":false, "automatic_retry":false}),
    )?;
    let vault = Credentials::for_database(database, &provider.id);
    let credential = match vault.resolve(provider, generation) {
        Ok(value) => value,
        Err(error) => {
            append(
                &mut receipt,
                json!({"state":"not_dispatched", "error":error.to_string()}),
            )?;
            return Err(error.into());
        }
    };
    ensure!(
        memex_models::read(&connection)? == snapshot,
        "stale_model_settings"
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    append(&mut receipt, json!({"state":"dispatching_outcome_unknown"}))?;
    let started = Instant::now();
    let result = runtime.block_on(complete(
        &snapshot,
        &credential,
        TextRequest {
            purpose: Purpose::Translation,
            system,
            input: &input,
            max_completion_tokens: max_tokens,
        },
    ));
    match result {
        Ok(result) => {
            // This is an isolated experiment, not permission to publish a stale
            // answer to a conversation or immutable Analysis record.
            let current = memex_models::read(&connection)?;
            let bound = current == snapshot && vault.resolve(provider, generation).is_ok();
            append(
                &mut receipt,
                json!({"state":if bound {"succeeded"} else {"stale_result_not_accepted"},
                "elapsed_ms":started.elapsed().as_millis(), "result":result}),
            )?;
            ensure!(bound, "stale_trial_result");
            println!("succeeded; receipt saved; usage is provider-reported, price unknown");
        }
        Err(error) => {
            append(
                &mut receipt,
                json!({"state":"stopped_no_retry", "error":error.to_string(),
                "elapsed_ms":started.elapsed().as_millis(), "usage":null,
                "execution_or_billing_may_have_occurred":true}),
            )?;
            return Err(error.into());
        }
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
