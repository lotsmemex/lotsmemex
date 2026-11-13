//! Trusted-host coordination of settings, OS credentials and explicit discovery.
//! No library content, inference, UI, proxy configuration or implicit retries.
use crate::queue::Queue;
use anyhow::{bail, Result};
use memex_models::{Provider, Snapshot};
use memex_provider::credentials::{ApiKey, CredentialState, CredentialStatus, Credentials};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum DiscoveryAuthorization {
    SavedKey { expected_generation: u64 },
    Credentialless,
}

fn provider<'a>(snapshot: &'a Snapshot, id: &str) -> Result<&'a Provider> {
    snapshot
        .configuration
        .providers
        .iter()
        .find(|p| p.id == id)
        .ok_or_else(|| anyhow::anyhow!("invalid_provider_settings"))
}

impl Queue {
    pub(crate) fn provider_credential(
        &mut self,
        expected_revision: u64,
        provider_id: &str,
    ) -> Result<CredentialStatus> {
        self.with_model_settings(expected_revision, |path, snapshot| {
            let provider = provider(&snapshot, provider_id)?;
            Ok(Credentials::for_database(path, provider_id).status(provider, snapshot.revision)?)
        })
    }

    pub(crate) fn replace_provider_credential(
        &mut self,
        expected_revision: u64,
        provider_id: &str,
        expected_generation: u64,
        key: Option<ApiKey>,
    ) -> Result<CredentialStatus> {
        self.with_model_settings(expected_revision, |path, snapshot| {
            let provider = provider(&snapshot, provider_id)?;
            Ok(Credentials::for_database(path, provider_id).replace(
                provider,
                snapshot.revision,
                expected_generation,
                key,
            )?)
        })
    }

    pub(crate) fn discover_provider_models(
        &mut self,
        expected_revision: u64,
        provider_id: &str,
        authorization: DiscoveryAuthorization,
    ) -> Result<memex_provider::DiscoveredModels> {
        let (snapshot, credential) =
            self.with_model_settings(expected_revision, |path, snapshot| {
                let provider = provider(&snapshot, provider_id)?;
                let credential = match authorization {
                    DiscoveryAuthorization::SavedKey {
                        expected_generation,
                    } => Some(
                        Credentials::for_database(path, provider_id)
                            .resolve(provider, expected_generation)?,
                    ),
                    DiscoveryAuthorization::Credentialless => None,
                };
                Ok((snapshot, credential))
            })?;
        // Do not hold the DB write lock over network I/O. Snapshot and credentials
        // are captured once; changed settings never reroute this request.
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(memex_provider::discover(
                &snapshot,
                provider_id,
                credential.as_ref(),
            ))?;
        self.with_model_settings(expected_revision, |path, current| {
            if let Some(generation) = result.credential_generation {
                let status = Credentials::for_database(path, provider_id)
                    .status(provider(&current, provider_id)?, current.revision)?;
                if status.generation != generation || status.state != CredentialState::Saved {
                    bail!("stale_provider_credential");
                }
            }
            Ok(())
        })?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };

    #[test]
    fn discovery_rechecks_settings_after_network_without_holding_sqlite_lock() {
        for change_during_request in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let database = root.path().canonicalize().unwrap().join("state/agent.db");
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let configuration = memex_models::Configuration {
                providers: vec![Provider {
                    id: "synthetic".into(),
                    label: "Synthetic".into(),
                    api_base: format!("http://{}/v1/", listener.local_addr().unwrap()),
                }],
                ..Default::default()
            };
            let mut queue = Queue::open(&database).unwrap();
            queue
                .replace_model_settings(0, configuration.clone())
                .unwrap();
            assert_eq!(
                queue
                    .discover_provider_models(
                        0,
                        "synthetic",
                        DiscoveryAuthorization::Credentialless
                    )
                    .unwrap_err()
                    .to_string(),
                "stale_model_settings"
            );
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                    assert!(request.len() < 8192);
                }
                assert!(!String::from_utf8(request)
                    .unwrap()
                    .to_lowercase()
                    .contains("authorization:"));
                if change_during_request {
                    // This would deadlock/time out if discovery held the write lock.
                    Queue::open(&database)
                        .unwrap()
                        .replace_model_settings(1, configuration)
                        .unwrap();
                }
                let body = r#"{"data":[{"id":"synthetic-model"}]}"#;
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            });
            let result = queue.discover_provider_models(
                1,
                "synthetic",
                DiscoveryAuthorization::Credentialless,
            );
            server.join().unwrap();
            if change_during_request {
                assert_eq!(result.unwrap_err().to_string(), "stale_model_settings");
            } else {
                let result = result.unwrap();
                assert_eq!(result.settings_revision, 1);
                assert_eq!(result.model_ids, ["synthetic-model"]);
                assert_eq!(result.credential_generation, None);
            }
        }
    }
}
