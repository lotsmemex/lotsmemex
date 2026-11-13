//! `model_generation_test`: one bounded request through the saved provider credential,
//! with a fixed built-in synthetic prompt. Never sends library content; for the settings
//! "test model" action and operator checks that the configured route really generates.

use crate::ipc::{Command, Response};
use crate::queue::Queue;
use anyhow::Result;
use memex_provider::credentials::Credentials;
use memex_provider::generation::{complete, TextRequest};
use std::time::Instant;

const SYSTEM: &str = "You are a connectivity check. Translate the user's sentence into Simplified Chinese. Reply with the translation only.";
const INPUT: &str = "A good library is a quiet place where ideas from many centuries meet.";

pub(crate) fn dispatch(
    q: &mut Queue,
    c: Command,
) -> Result<std::result::Result<Response, Command>> {
    let Command::ModelGenerationTest {
        expected_revision,
        purpose,
        expected_generation,
    } = c
    else {
        return Ok(Err(c));
    };
    let (snapshot, credential) = q.with_model_settings(expected_revision, |path, snapshot| {
        let (provider, _) = snapshot
            .configuration
            .target(purpose)
            .ok_or_else(|| anyhow::anyhow!("invalid_model_settings"))?;
        let credential =
            Credentials::for_database(path, &provider.id).resolve(provider, expected_generation)?;
        Ok((snapshot, credential))
    })?;
    // No database lock is held across the network request.
    let started = Instant::now();
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(complete(
            &snapshot,
            &credential,
            TextRequest {
                purpose,
                system: SYSTEM,
                input: INPUT,
                max_completion_tokens: 400,
            },
        ))?;
    let mut response = Response::empty_ok();
    response.generation_test = Some(serde_json::json!({
        "purpose": purpose,
        "input": INPUT,
        "latency_ms": started.elapsed().as_millis() as u64,
        "result": result,
    }));
    Ok(Ok(response))
}
