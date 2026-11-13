//! Non-secret provider configuration and purpose routing. No UI, library, network or inference.
//! Saving a target does not verify it or authorize sending content to it.
pub mod low_dial;
use anyhow::{bail, Result};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use url::Url;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provider {
    pub id: String,
    pub label: String,
    /// OpenAI-compatible API base, including its path, e.g. http://127.0.0.1:8317/v1/.
    pub api_base: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Model {
    pub id: String,
    pub label: String,
    pub provider_id: String,
    /// Exact upstream ID, not an Amp display name or an asserted capability.
    pub model_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    Chat,
    Suggestions,
    Translation,
    Summary,
    Classification,
    CaptureQuality,
}

/// A saved request preference, not evidence that a provider supports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub purpose: Purpose,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Configuration {
    pub providers: Vec<Provider>,
    pub models: Vec<Model>,
    /// No implicit fallback. Removing a selected model must explicitly clear or replace its route.
    pub selections: Vec<Selection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub schema_version: u32,
    pub revision: u64,
    pub configuration: Configuration,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            schema_version: 1,
            revision: 0,
            configuration: Configuration::default(),
        }
    }
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

fn label(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

fn base_is_valid(base: &str) -> bool {
    let Ok(url) = Url::parse(base) else {
        return false;
    };
    // HTTP is permitted only for a literal loopback address. A hostname resolving
    // to loopback is not sufficient. Transport must separately reject redirects/proxies.
    let local = matches!(url.host_str(), Some("127.0.0.1" | "[::1]"));
    (url.scheme() == "https" || (url.scheme() == "http" && local))
        && url.host_str().is_some()
        && url.port() != Some(0)
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.path().ends_with('/')
        && base.len() <= 2048
        && url.as_str() == base
}

impl Configuration {
    pub fn validate(&self) -> Result<()> {
        if self.providers.len() > 32
            || self.models.len() > 128
            || self.selections.len() > 6
            || serde_json::to_vec(self)?.len() > 48 * 1024
        {
            bail!("invalid_model_settings");
        }
        let mut providers = BTreeSet::new();
        for provider in &self.providers {
            if !identifier(&provider.id)
                || !label(&provider.label)
                || !base_is_valid(&provider.api_base)
                || !providers.insert(&provider.id)
            {
                bail!("invalid_model_settings");
            }
        }
        let mut models = BTreeSet::new();
        for model in &self.models {
            if !identifier(&model.id)
                || !label(&model.label)
                || model.model_id.is_empty()
                || model.model_id.len() > 256
                || model.model_id.chars().any(char::is_whitespace)
                || model.model_id.chars().any(char::is_control)
                || !providers.contains(&model.provider_id)
                || !models.insert(&model.id)
            {
                bail!("invalid_model_settings");
            }
        }
        for (i, selection) in self.selections.iter().enumerate() {
            if !models.contains(&selection.model)
                || self.selections[..i]
                    .iter()
                    .any(|s| s.purpose == selection.purpose)
            {
                bail!("invalid_model_settings");
            }
        }
        Ok(())
    }

    pub fn target(&self, purpose: Purpose) -> Option<(&Provider, &Model)> {
        let selection = self.selections.iter().find(|s| s.purpose == purpose)?;
        let model = self.models.iter().find(|m| m.id == selection.model)?;
        let provider = self.providers.iter().find(|p| p.id == model.provider_id)?;
        Some((provider, model))
    }
}

/// The host supplies its already secured local database connection. This module
/// owns only its namespaced table; it neither opens paths nor touches canonical data.
pub fn read(connection: &Connection) -> Result<Snapshot> {
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='memex_model_settings_v1')",
        [], |row| row.get(0),
    )?;
    if !exists {
        return Ok(Snapshot::default());
    }
    let payload: Option<String> = connection
        .query_row(
            "SELECT payload FROM memex_model_settings_v1 WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let snapshot = match payload {
        None => Snapshot::default(),
        Some(payload) => serde_json::from_str::<Snapshot>(&payload)
            .map_err(|_| anyhow::anyhow!("invalid_model_settings"))?,
    };
    if snapshot.schema_version != 1 {
        bail!("unsupported_model_settings");
    }
    snapshot.configuration.validate()?;
    Ok(snapshot)
}

pub fn replace(
    connection: &mut Connection,
    expected_revision: u64,
    configuration: Configuration,
) -> Result<Snapshot> {
    configuration.validate()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current = read(&transaction)?;
    if current.revision != expected_revision {
        bail!("stale_model_settings");
    }
    let snapshot = Snapshot {
        schema_version: 1,
        revision: current
            .revision
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("invalid_model_settings"))?,
        configuration,
    };
    transaction.execute_batch("CREATE TABLE IF NOT EXISTS memex_model_settings_v1(singleton INTEGER PRIMARY KEY CHECK(singleton=1), payload TEXT NOT NULL)")?;
    transaction.execute("INSERT INTO memex_model_settings_v1(singleton,payload) VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET payload=excluded.payload", [serde_json::to_string(&snapshot)?])?;
    transaction.commit()?;
    Ok(snapshot)
}
