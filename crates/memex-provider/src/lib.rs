//! Explicit provider operations, isolated from library authority and scheduling.
//! A trusted host supplies a captured settings snapshot and scoped credential.
pub mod credentials;
pub mod generation;

use memex_models::{Provider, Snapshot};
use reqwest::{header, redirect::Policy, Client, StatusCode};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, time::Duration};
use zeroize::Zeroizing;

const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_MODELS: usize = 1024;
const DEADLINE: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryError {
    InvalidSettings,
    InvalidCredential,
    CredentialScopeMismatch,
    Unauthorized,
    RedirectRefused,
    Unavailable,
    Timeout,
    ResponseTooLarge,
    InvalidResponse,
}

impl std::fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidSettings => "invalid_provider_settings",
            Self::InvalidCredential => "invalid_provider_credential",
            Self::CredentialScopeMismatch => "provider_credential_scope_mismatch",
            Self::Unauthorized => "provider_unauthorized",
            Self::RedirectRefused => "provider_redirect_refused",
            Self::Unavailable => "provider_unavailable",
            Self::Timeout => "provider_timeout",
            Self::ResponseTooLarge => "provider_response_too_large",
            Self::InvalidResponse => "invalid_provider_response",
        })
    }
}
impl std::error::Error for DiscoveryError {}

/// Neither Debug nor Serialize: credentials must not enter settings or diagnostics.
/// Generation is assigned by the credential owner, not an untrusted remote client.
pub struct ApiCredential {
    provider_id: String,
    api_base: String,
    generation: u64,
    value: Zeroizing<String>,
}

impl ApiCredential {
    pub fn new(
        provider: &Provider,
        generation: u64,
        value: String,
    ) -> Result<Self, DiscoveryError> {
        let value = Zeroizing::new(value);
        if generation == 0
            || value.is_empty()
            || value.len() > 4096
            || !value.bytes().all(|b| (0x21..=0x7e).contains(&b))
        {
            return Err(DiscoveryError::InvalidCredential);
        }
        Ok(Self {
            provider_id: provider.id.clone(),
            api_base: provider.api_base.clone(),
            generation,
            value,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoveredModels {
    pub schema_version: u32,
    pub settings_revision: u64,
    pub provider_id: String,
    pub api_base: String,
    /// None is an explicitly credentialless request, not a missing-secret fallback.
    pub credential_generation: Option<u64>,
    pub model_ids: Vec<String>,
}

#[derive(Deserialize)]
struct ModelList {
    data: Vec<ModelEntry>,
}
#[derive(Deserialize)]
struct ModelEntry {
    id: String,
}

/// Performs only GET <configured api_base>/models, with no retries or inference.
/// This result is an advertised list, never an inference/permission/health guarantee.
/// Caller must discard it if settings or credential generation changed in flight.
pub async fn discover(
    snapshot: &Snapshot,
    provider_id: &str,
    credential: Option<&ApiCredential>,
) -> Result<DiscoveredModels, DiscoveryError> {
    if snapshot.schema_version != 1 {
        return Err(DiscoveryError::InvalidSettings);
    }
    snapshot
        .configuration
        .validate()
        .map_err(|_| DiscoveryError::InvalidSettings)?;
    let provider = snapshot
        .configuration
        .providers
        .iter()
        .find(|p| p.id == provider_id)
        .ok_or(DiscoveryError::InvalidSettings)?;
    if credential.is_some_and(|c| c.provider_id != provider.id || c.api_base != provider.api_base) {
        return Err(DiscoveryError::CredentialScopeMismatch);
    }
    let endpoint = url::Url::parse(&provider.api_base)
        .and_then(|base| base.join("models"))
        .map_err(|_| DiscoveryError::InvalidSettings)?;
    let client = Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .referer(false)
        .timeout(DEADLINE)
        .build()
        .map_err(|_| DiscoveryError::Unavailable)?;
    let mut request = client
        .get(endpoint)
        .header(header::ACCEPT, "application/json");
    if let Some(credential) = credential {
        let bearer = Zeroizing::new(format!("Bearer {}", credential.value.as_str()));
        let mut value = header::HeaderValue::from_str(&bearer)
            .map_err(|_| DiscoveryError::InvalidCredential)?;
        value.set_sensitive(true);
        request = request.header(header::AUTHORIZATION, value);
    }
    let models = tokio::time::timeout(DEADLINE, async {
        let mut response = request.send().await.map_err(transport_error)?;
        if matches!(
            response.status(),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
        ) {
            return Err(DiscoveryError::Unauthorized);
        }
        if response.status().is_redirection() {
            return Err(DiscoveryError::RedirectRefused);
        }
        if !response.status().is_success() {
            return Err(DiscoveryError::Unavailable);
        }
        if response
            .content_length()
            .is_some_and(|n| n > MAX_RESPONSE_BYTES as u64)
        {
            return Err(DiscoveryError::ResponseTooLarge);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
            if chunk.len() > MAX_RESPONSE_BYTES - body.len() {
                return Err(DiscoveryError::ResponseTooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        let list: ModelList =
            serde_json::from_slice(&body).map_err(|_| DiscoveryError::InvalidResponse)?;
        if list.data.len() > MAX_MODELS {
            return Err(DiscoveryError::ResponseTooLarge);
        }
        let mut ids = BTreeSet::new();
        for model in list.data {
            if model.id.is_empty()
                || model.id.len() > 256
                || model
                    .id
                    .chars()
                    .any(|c| c.is_whitespace() || c.is_control())
            {
                return Err(DiscoveryError::InvalidResponse);
            }
            ids.insert(model.id);
        }
        Ok(ids.into_iter().collect::<Vec<_>>())
    })
    .await
    .map_err(|_| DiscoveryError::Timeout)??;
    Ok(DiscoveredModels {
        schema_version: 1,
        settings_revision: snapshot.revision,
        provider_id: provider.id.clone(),
        api_base: provider.api_base.clone(),
        credential_generation: credential.map(|c| c.generation),
        model_ids: models,
    })
}

fn transport_error(error: reqwest::Error) -> DiscoveryError {
    if error.is_timeout() {
        DiscoveryError::Timeout
    } else {
        DiscoveryError::Unavailable
    }
}
