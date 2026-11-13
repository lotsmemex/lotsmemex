//! Separate mechanical-task route: existing clients round-trip Configuration unchanged.
use crate::{read as read_settings, ReasoningEffort};
use anyhow::{ensure, Result};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub revision: u64,
    pub model: String,
    /// None omits the parameter. It is not the provider value "none".
    pub reasoning_effort: Option<ReasoningEffort>,
    pub max_input_bytes: usize,
    pub max_completion_tokens: u32,
}

impl Profile {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.revision > 0
                && !self.model.is_empty()
                && self.model.len() <= 80
                && (1024..=262_144).contains(&self.max_input_bytes)
                && (64..=16_000).contains(&self.max_completion_tokens)
                && matches!(
                    self.reasoning_effort,
                    None | Some(
                        ReasoningEffort::None | ReasoningEffort::Minimal | ReasoningEffort::Low
                    )
                ),
            "invalid_low_dial_profile"
        );
        Ok(())
    }
}

pub fn read(connection: &Connection) -> Result<Option<Profile>> {
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='memex_low_dial_v1')", [], |r| r.get(0))?;
    if !exists {
        return Ok(None);
    }
    let payload: Option<String> = connection
        .query_row(
            "SELECT payload FROM memex_low_dial_v1 WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    payload
        .map(|p| {
            let profile: Profile = serde_json::from_str(&p)
                .map_err(|_| anyhow::anyhow!("invalid_low_dial_profile"))?;
            profile.validate()?;
            Ok(profile)
        })
        .transpose()
}

/// CAS update only this route; never creates a provider, credential or model setting.
pub fn replace(
    connection: &mut Connection,
    expected_revision: u64,
    mut profile: Profile,
) -> Result<Profile> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current = read(&tx)?.map_or(0, |p| p.revision);
    ensure!(current == expected_revision, "stale_low_dial_profile");
    profile.revision = current
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("invalid_low_dial_profile"))?;
    profile.validate()?;
    ensure!(
        read_settings(&tx)?
            .configuration
            .models
            .iter()
            .any(|m| m.id == profile.model),
        "unknown_low_dial_model"
    );
    tx.execute_batch("CREATE TABLE IF NOT EXISTS memex_low_dial_v1(singleton INTEGER PRIMARY KEY CHECK(singleton=1), payload TEXT NOT NULL)")?;
    tx.execute("INSERT INTO memex_low_dial_v1(singleton,payload) VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET payload=excluded.payload", [serde_json::to_string(&profile)?])?;
    tx.commit()?;
    Ok(profile)
}
