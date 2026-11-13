//! Scoped macOS Keychain storage. Caller serializes access with its settings DB
//! transaction: keychain replacement is atomic, and no secret is written to SQL.
use crate::ApiCredential;
use memex_models::Provider;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::path::Path;
use zeroize::Zeroizing;

const SERVICE: &str = "org.memex.provider-credentials.v1";

/// Serialized only for owner-only local IPC and Keychain; never a response field.
pub struct ApiKey(Zeroizing<String>);

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKey([REDACTED])")
    }
}
impl Serialize for ApiKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}
impl<'de> Deserialize<'de> for ApiKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Zeroizing::new(String::deserialize(deserializer)?);
        if value.is_empty()
            || value.len() > 4096
            || !value.bytes().all(|b| (0x21..=0x7e).contains(&b))
        {
            return Err(serde::de::Error::custom("invalid_provider_credential"));
        }
        Ok(Self(value))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialError {
    Unavailable,
    Invalid,
    Stale,
    Missing,
    ScopeChanged,
}
impl std::fmt::Display for CredentialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unavailable => "credential_store_unavailable",
            Self::Invalid => "invalid_provider_credential",
            Self::Stale => "stale_provider_credential",
            Self::Missing => "provider_credential_missing",
            Self::ScopeChanged => "provider_credential_scope_mismatch",
        })
    }
}
impl std::error::Error for CredentialError {}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CredentialState {
    Missing,
    Saved,
    ScopeChanged,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialStatus {
    pub settings_revision: u64,
    pub provider_id: String,
    pub api_base: String,
    pub generation: u64,
    pub state: CredentialState,
    pub masked_key: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    schema_version: u32,
    generation: u64,
    api_base: String,
    key: Option<ApiKey>,
}

/// The host supplies its validated canonical agent DB path, never a renderer path.
/// A missing entry differs from access denied/locked/corrupt, which fail closed.
pub struct Credentials {
    account: String,
}
impl Credentials {
    pub fn for_database(database: &Path, provider_id: &str) -> Self {
        use std::os::unix::ffi::OsStrExt;
        let mut hash = Sha256::new();
        let path = database.as_os_str().as_bytes();
        hash.update((path.len() as u64).to_be_bytes());
        hash.update(path);
        hash.update(provider_id.as_bytes());
        Self {
            account: format!("{:x}", hash.finalize()),
        }
    }

    fn read(&self) -> Result<Option<Stored>, CredentialError> {
        let Some(bytes) = native_read(&self.account)? else {
            return Ok(None);
        };
        let bytes = Zeroizing::new(bytes);
        if bytes.len() > 32768 {
            return Err(CredentialError::Invalid);
        }
        let stored: Stored =
            serde_json::from_slice(&bytes).map_err(|_| CredentialError::Invalid)?;
        if stored.schema_version != 1 || stored.generation == 0 {
            return Err(CredentialError::Invalid);
        }
        Ok(Some(stored))
    }

    pub fn status(
        &self,
        provider: &Provider,
        settings_revision: u64,
    ) -> Result<CredentialStatus, CredentialError> {
        let stored = self.read()?;
        Ok(status(stored.as_ref(), provider, settings_revision))
    }

    /// None clears the secret but retains a non-secret generation tombstone to
    /// prevent stale delete/save replay. Provider removal does not delete keys.
    pub fn replace(
        &self,
        provider: &Provider,
        settings_revision: u64,
        expected_generation: u64,
        key: Option<ApiKey>,
    ) -> Result<CredentialStatus, CredentialError> {
        let previous = self.read()?;
        if previous.as_ref().map_or(0, |s| s.generation) != expected_generation {
            return Err(CredentialError::Stale);
        }
        let stored = Stored {
            schema_version: 1,
            generation: expected_generation
                .checked_add(1)
                .ok_or(CredentialError::Invalid)?,
            api_base: provider.api_base.clone(),
            key,
        };
        let bytes =
            Zeroizing::new(serde_json::to_vec(&stored).map_err(|_| CredentialError::Invalid)?);
        native_write(&self.account, &bytes)?;
        Ok(status(Some(&stored), provider, settings_revision))
    }

    pub fn resolve(
        &self,
        provider: &Provider,
        expected_generation: u64,
    ) -> Result<ApiCredential, CredentialError> {
        let stored = self.read()?.ok_or(CredentialError::Missing)?;
        if stored.generation != expected_generation {
            return Err(CredentialError::Stale);
        }
        if stored.api_base != provider.api_base {
            return Err(CredentialError::ScopeChanged);
        }
        let key = stored.key.ok_or(CredentialError::Missing)?;
        ApiCredential::new(provider, stored.generation, key.0.to_string())
            .map_err(|_| CredentialError::Invalid)
    }
}

fn status(
    stored: Option<&Stored>,
    provider: &Provider,
    settings_revision: u64,
) -> CredentialStatus {
    let (state, masked_key) = match stored {
        Some(s) if s.key.is_some() && s.api_base != provider.api_base => {
            (CredentialState::ScopeChanged, None)
        }
        Some(s) if s.key.is_some() => {
            let key = &s.key.as_ref().unwrap().0;
            let mask = if key.len() <= 3 {
                "***".into()
            } else {
                format!("***{}", &key[key.len() - 3..])
            };
            (CredentialState::Saved, Some(mask))
        }
        _ => (CredentialState::Missing, None),
    };
    CredentialStatus {
        settings_revision,
        provider_id: provider.id.clone(),
        api_base: provider.api_base.clone(),
        generation: stored.map_or(0, |s| s.generation),
        state,
        masked_key,
    }
}

#[cfg(target_os = "macos")]
fn options(account: &str) -> security_framework::passwords::PasswordOptions {
    let mut options =
        security_framework::passwords::PasswordOptions::new_generic_password(SERVICE, account);
    options.set_access_synchronized(Some(false));
    options
}
#[cfg(target_os = "macos")]
fn noninteractive<T>(
    operation: impl FnOnce() -> Result<T, CredentialError>,
) -> Result<T, CredentialError> {
    use security_framework::os::macos::keychain::SecKeychain;
    // The OS interaction flag is process-local, not per request. Serialize these
    // calls and restore its prior state; never prompt from the headless daemon.
    static KEYCHAIN_CALL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _call = KEYCHAIN_CALL
        .lock()
        .map_err(|_| CredentialError::Unavailable)?;
    let _no_ui = if SecKeychain::user_interaction_allowed()
        .map_err(|_| CredentialError::Unavailable)?
    {
        Some(SecKeychain::disable_user_interaction().map_err(|_| CredentialError::Unavailable)?)
    } else {
        None
    };
    operation()
}
#[cfg(target_os = "macos")]
fn native_read(account: &str) -> Result<Option<Vec<u8>>, CredentialError> {
    noninteractive(
        || match security_framework::passwords::generic_password(options(account)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.code() == -25300 => Ok(None), // errSecItemNotFound only
            Err(_) => Err(CredentialError::Unavailable),
        },
    )
}
#[cfg(target_os = "macos")]
fn native_write(account: &str, bytes: &[u8]) -> Result<(), CredentialError> {
    noninteractive(|| {
        security_framework::passwords::set_generic_password_options(bytes, options(account))
            .map_err(|_| CredentialError::Unavailable)
    })
}
#[cfg(not(target_os = "macos"))]
fn native_read(_: &str) -> Result<Option<Vec<u8>>, CredentialError> {
    Err(CredentialError::Unavailable)
}
#[cfg(not(target_os = "macos"))]
fn native_write(_: &str, _: &[u8]) -> Result<(), CredentialError> {
    Err(CredentialError::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_debug_and_status_redact_and_scope_without_serializing_the_secret() {
        let key: ApiKey = serde_json::from_str("\"synthetic-secret\"").unwrap();
        assert!(!format!("{key:?}").contains("synthetic-secret"));
        let provider = Provider {
            id: "synthetic".into(),
            label: "Synthetic".into(),
            api_base: "http://127.0.0.1:9/v1/".into(),
        };
        let mut stored = Stored {
            schema_version: 1,
            generation: 3,
            api_base: provider.api_base.clone(),
            key: Some(key),
        };
        let value = status(Some(&stored), &provider, 9);
        assert_eq!(value.masked_key.as_deref(), Some("***ret"));
        assert!(!serde_json::to_string(&value)
            .unwrap()
            .contains("synthetic-secret"));
        stored.key = Some(serde_json::from_str("\"abc\"").unwrap());
        assert_eq!(
            status(Some(&stored), &provider, 9).masked_key.as_deref(),
            Some("***")
        );
        stored.api_base = "https://different.invalid/".into();
        let value = status(Some(&stored), &provider, 9);
        assert_eq!(value.state, CredentialState::ScopeChanged);
        assert!(value.masked_key.is_none());
    }

    #[test]
    #[cfg(target_os = "macos")]
    #[ignore = "explicit native test creates and removes one synthetic Keychain entry"]
    fn native_keychain_roundtrip_cas_scope_clear_and_reopen() {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("synthetic-agent.db");
        let provider = Provider {
            id: "synthetic".into(),
            label: "Synthetic".into(),
            api_base: "http://127.0.0.1:9/v1/".into(),
        };
        let vault = Credentials::for_database(&db, &provider.id);
        struct Cleanup(String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = security_framework::passwords::delete_generic_password_options(options(
                    &self.0,
                ));
            }
        }
        let cleanup = Cleanup(vault.account.clone());
        assert_eq!(
            vault.status(&provider, 2).unwrap().state,
            CredentialState::Missing
        );
        let saved = vault
            .replace(
                &provider,
                2,
                0,
                Some(serde_json::from_str("\"synthetic-key-one\"").unwrap()),
            )
            .unwrap();
        assert_eq!(saved.generation, 1);
        assert_eq!(saved.masked_key.as_deref(), Some("***one"));
        assert_eq!(
            vault.replace(&provider, 2, 0, None).unwrap_err(),
            CredentialError::Stale
        );
        let reopened = Credentials::for_database(&db, &provider.id);
        assert_eq!(reopened.status(&provider, 2).unwrap().generation, 1);
        assert!(reopened.resolve(&provider, 1).is_ok());
        let mut moved = provider.clone();
        moved.api_base = "http://127.0.0.1:10/v1/".into();
        assert_eq!(
            reopened.status(&moved, 3).unwrap().state,
            CredentialState::ScopeChanged
        );
        assert!(matches!(
            reopened.resolve(&moved, 1),
            Err(CredentialError::ScopeChanged)
        ));
        let cleared = reopened.replace(&provider, 2, 1, None).unwrap();
        assert_eq!(cleared.generation, 2);
        assert_eq!(cleared.state, CredentialState::Missing);
        assert!(matches!(
            reopened.resolve(&provider, 2),
            Err(CredentialError::Missing)
        ));
        assert_eq!(
            reopened.replace(&provider, 2, 1, None).unwrap_err(),
            CredentialError::Stale
        );
        security_framework::passwords::delete_generic_password_options(options(&cleanup.0))
            .unwrap();
        assert!(native_read(&cleanup.0).unwrap().is_none());
    }
}
