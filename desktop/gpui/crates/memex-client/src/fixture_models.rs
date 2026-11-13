//! Synthetic model settings for the fixture backend, following
//! `memex.model_settings.md`: revision CAS for configuration, generation CAS for
//! credentials, explicit discovery. No key is kept beyond its last three
//! characters, and discovery returns invented IDs without any network access.

use crate::dto::*;
use std::collections::{BTreeMap, HashSet};

#[derive(Default)]
pub(crate) struct ModelState {
    revision: u64,
    configuration: ModelConfiguration,
    /// provider id -> (generation, api_base the key was saved for, masked suffix if saved)
    credentials: BTreeMap<String, (u64, String, Option<String>)>,
}

fn ok(r: Response) -> Response {
    Response { ok: true, ..r }
}
fn err(code: &str) -> Response {
    Response { ok: false, reason_code: Some(code.into()), ..Default::default() }
}

fn valid_id(id: &str) -> bool {
    (1..=80).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
}
fn valid_label(l: &str) -> bool {
    (1..=256).contains(&l.len()) && !l.trim().is_empty() && !l.chars().any(char::is_control)
}
fn valid_base(u: &str) -> bool {
    u.ends_with('/')
        && !u.contains(['?', '#', '@'])
        && (u.starts_with("https://") || u.starts_with("http://127.0.0.1:") || u.starts_with("http://[::1]:"))
}

fn validate(c: &ModelConfiguration) -> bool {
    let mut providers = HashSet::new();
    for p in &c.providers {
        if !valid_id(&p.id) || !valid_label(&p.label) || !valid_base(&p.api_base) || !providers.insert(p.id.as_str()) {
            return false;
        }
    }
    let mut models = HashSet::new();
    for m in &c.models {
        let id_ok = (1..=256).contains(&m.model_id.len()) && !m.model_id.chars().any(|ch| ch.is_whitespace() || ch.is_control());
        if !valid_id(&m.id) || !valid_label(&m.label) || !id_ok || !providers.contains(m.provider_id.as_str()) || !models.insert(m.id.as_str()) {
            return false;
        }
    }
    let mut purposes = HashSet::new();
    c.selections.iter().all(|s| {
        models.contains(s.model.as_str())
            && purposes.insert(s.purpose)
            && s.reasoning_effort.as_deref().is_none_or(|e| REASONING_EFFORTS.contains(&e))
    })
        && c.providers.len() <= 32
        && c.models.len() <= 128
}

impl ModelState {
    fn snapshot(&self) -> ModelSettings {
        ModelSettings { schema_version: 1, revision: self.revision, configuration: self.configuration.clone() }
    }

    fn credential(&self, provider_id: &str) -> Option<ProviderCredential> {
        let p = self.configuration.providers.iter().find(|p| p.id == provider_id)?;
        let (generation, base, masked) = self.credentials.get(provider_id).cloned().unwrap_or((0, p.api_base.clone(), None));
        let state = match (&masked, base == p.api_base) {
            (None, _) => CredentialState::Missing,
            (Some(_), true) => CredentialState::Saved,
            (Some(_), false) => CredentialState::ScopeChanged,
        };
        Some(ProviderCredential {
            settings_revision: self.revision,
            provider_id: provider_id.into(),
            api_base: p.api_base.clone(),
            generation,
            masked_key: if state == CredentialState::Saved { masked } else { None },
            state,
        })
    }

    pub(crate) fn handle(&mut self, command: &Command) -> Response {
        match command {
            Command::ModelSettingsRead => ok(Response { model_settings: Some(self.snapshot()), ..Default::default() }),
            Command::ModelSettingsReplace { expected_revision, configuration } => {
                if *expected_revision != self.revision {
                    return err("stale_model_settings");
                }
                if !validate(configuration) {
                    return err("invalid_model_settings");
                }
                self.configuration = configuration.clone();
                self.revision += 1;
                ok(Response { model_settings: Some(self.snapshot()), ..Default::default() })
            }
            Command::ProviderCredentialRead { expected_revision, provider_id } => {
                if *expected_revision != self.revision {
                    return err("stale_model_settings");
                }
                match self.credential(provider_id) {
                    Some(c) => ok(Response { provider_credential: Some(c), ..Default::default() }),
                    None => err("provider_not_found"),
                }
            }
            Command::ProviderCredentialReplace { expected_revision, provider_id, expected_generation, api_key } => {
                if *expected_revision != self.revision {
                    return err("stale_model_settings");
                }
                let Some(current) = self.credential(provider_id) else { return err("provider_not_found") };
                if current.generation != *expected_generation {
                    return err("stale_provider_credential");
                }
                let raw = serde_json::to_value(api_key).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
                if raw.is_empty() || raw.len() > 4096 || !raw.bytes().all(|b| b.is_ascii_graphic()) {
                    return err("invalid_provider_credential");
                }
                let masked = if raw.len() <= 3 { "***".to_string() } else { format!("***{}", &raw[raw.len() - 3..]) };
                self.credentials.insert(provider_id.clone(), (current.generation + 1, current.api_base, Some(masked)));
                ok(Response { provider_credential: self.credential(provider_id), ..Default::default() })
            }
            Command::ProviderCredentialClear { expected_revision, provider_id, expected_generation } => {
                if *expected_revision != self.revision {
                    return err("stale_model_settings");
                }
                let Some(current) = self.credential(provider_id) else { return err("provider_not_found") };
                if current.generation != *expected_generation {
                    return err("stale_provider_credential");
                }
                self.credentials.insert(provider_id.clone(), (current.generation + 1, current.api_base, None));
                ok(Response { provider_credential: self.credential(provider_id), ..Default::default() })
            }
            Command::ProviderModelsDiscover { expected_revision, provider_id, authorization } => {
                if *expected_revision != self.revision {
                    return err("stale_model_settings");
                }
                let Some(current) = self.credential(provider_id) else { return err("provider_not_found") };
                let generation = match authorization {
                    DiscoverAuthorization::Credentialless => None,
                    DiscoverAuthorization::SavedKey { expected_generation } => {
                        if current.state != CredentialState::Saved {
                            return err("provider_unauthorized");
                        }
                        if current.generation != *expected_generation {
                            return err("stale_provider_credential");
                        }
                        Some(current.generation)
                    }
                };
                ok(Response {
                    provider_models: Some(ProviderModels {
                        schema_version: 1,
                        settings_revision: self.revision,
                        provider_id: provider_id.clone(),
                        api_base: current.api_base,
                        credential_generation: generation,
                        model_ids: vec!["fixture-model-alpha".into(), "fixture-model-beta".into(), "fixture-model-gamma".into()],
                    }),
                    ..Default::default()
                })
            }
            _ => err("invalid_request"),
        }
    }
}
