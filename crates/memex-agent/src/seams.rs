use anyhow::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
pub trait Keychain {
    fn secret(&self, reference: &str) -> Result<Vec<u8>>;
    fn put_secret(&self, reference: &str, secret: &[u8]) -> Result<()>;
    fn delete_secret(&self, reference: &str) -> Result<()>;
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeClickCapture {
    pub top_level_url: String,
    pub dom_utf8: Vec<u8>,
    pub explicit_click: bool,
}
impl NativeClickCapture {
    pub fn validate(&self) -> Result<()> {
        let url = url::Url::parse(&self.top_level_url)
            .map_err(|_| anyhow::anyhow!("native_message_rejected"))?;
        if !self.explicit_click
            || self.dom_utf8.len() > 8 * 1024 * 1024
            || !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || url
                .port()
                .is_some_and(|port| port != if url.scheme() == "http" { 80 } else { 443 })
        {
            anyhow::bail!("native_message_rejected")
        }
        let s = std::str::from_utf8(&self.dom_utf8)
            .map_err(|_| anyhow::anyhow!("native_message_rejected"))?;
        memex_capture::validate_inert_dom_snapshot(s)
            .map_err(|_| anyhow::anyhow!("native_message_sensitive_field"))?;
        Ok(())
    }
}
pub trait Relay {
    fn poll_encrypted(&self) -> Result<Vec<RelayCommand>>;
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayCommand {
    pub envelope_version: u32,
    pub opaque_id: String,
    pub key_reference: String,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
    pub authentication_tag: Vec<u8>,
}
impl RelayCommand {
    pub fn validate(&self) -> Result<()> {
        if self.envelope_version != 1
            || !stable_id(&self.opaque_id)
            || !stable_id(&self.key_reference)
            || self.nonce.len() != 24
            || self.ciphertext.is_empty()
            || self.ciphertext.len() > 64 * 1024
            || self.authentication_tag.len() != 16
        {
            anyhow::bail!("relay_envelope_rejected")
        }
        Ok(())
    }
}
pub const BATCH_MAX_CANDIDATES: usize = 200;
pub const BATCH_GLOBAL_CONCURRENCY: usize = 2;
pub const BATCH_ORIGIN_CONCURRENCY: usize = 1;
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatchDiscovery {
    pub page_url: String,
    pub candidate_set_digest: String,
    pub candidate_ids: Vec<String>,
    pub selected_ids: Vec<String>,
    pub child_ids: Vec<String>,
}
impl BatchDiscovery {
    pub fn validate(&self) -> Result<()> {
        let parsed = url::Url::parse(&self.page_url)
            .map_err(|_| anyhow::anyhow!("batch_contract_rejected"))?;
        let candidate_digest = hex::encode(Sha256::digest(
            serde_json::to_vec(&self.candidate_ids)
                .map_err(|_| anyhow::anyhow!("batch_contract_rejected"))?,
        ));
        let expected_children = self
            .selected_ids
            .iter()
            .map(|candidate_id| {
                derive_batch_child_id(parsed.as_str(), &self.candidate_set_digest, candidate_id)
            })
            .collect::<Result<Vec<_>>>()?;
        if !matches!(parsed.scheme(), "http" | "https")
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.fragment().is_some()
            || parsed
                .port()
                .is_some_and(|port| port != if parsed.scheme() == "http" { 80 } else { 443 })
            || self.candidate_ids.is_empty()
            || self.candidate_ids.len() > BATCH_MAX_CANDIDATES
            || !strictly_sorted(&self.candidate_ids)
            || self.candidate_ids.iter().any(|id| !stable_id(id))
            || self.candidate_set_digest != candidate_digest
            || !strictly_sorted(&self.selected_ids)
            || !all_unique(&self.child_ids)
            || self.selected_ids.len() != self.child_ids.len()
            || self.child_ids != expected_children
            || self
                .selected_ids
                .iter()
                .any(|id| self.candidate_ids.binary_search(id).is_err())
            || self.child_ids.iter().any(|id| !stable_id(id))
        {
            anyhow::bail!("batch_contract_rejected")
        }
        Ok(())
    }
}

pub fn batch_child_id(
    page_url: &str,
    candidate_set_digest: &str,
    candidate_id: &str,
) -> Result<String> {
    let parsed =
        url::Url::parse(page_url).map_err(|_| anyhow::anyhow!("batch_contract_rejected"))?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
        || parsed
            .port()
            .is_some_and(|port| port != if parsed.scheme() == "http" { 80 } else { 443 })
        || candidate_set_digest.len() != 64
        || !candidate_set_digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || !stable_id(candidate_id)
    {
        anyhow::bail!("batch_contract_rejected")
    }
    derive_batch_child_id(parsed.as_str(), candidate_set_digest, candidate_id)
}

fn derive_batch_child_id(
    canonical_page_url: &str,
    candidate_set_digest: &str,
    candidate_id: &str,
) -> Result<String> {
    Ok(format!(
        "batch_{}",
        hex::encode(Sha256::digest(serde_json::to_vec(&(
            canonical_page_url,
            candidate_set_digest,
            candidate_id
        ))?))
    ))
}

fn strictly_sorted(values: &[String]) -> bool {
    values.windows(2).all(|pair| pair[0] < pair[1])
}
fn all_unique(values: &[String]) -> bool {
    let mut sorted = values.to_vec();
    sorted.sort();
    sorted.windows(2).all(|pair| pair[0] != pair[1])
}

fn stable_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn future_seams_reject_sensitive_or_mutable_shapes() {
        let click = NativeClickCapture {
            top_level_url: "https://article.test/one".into(),
            dom_utf8: b"<article>inert synthetic body</article>".to_vec(),
            explicit_click: true,
        };
        click.validate().unwrap();
        for html in [
            "<form></form>",
            "<input>",
            "<textarea>synthetic value</textarea>",
            "<select><option>synthetic value</option></select>",
            "<div contenteditable>synthetic value</div>",
            "<input type=password value=synthetic-secret>",
            "<input data-marker='>' type=pass&#119;ord value=synthetic-secret>",
        ] {
            assert!(NativeClickCapture {
                top_level_url: "https://article.test/one".into(),
                dom_utf8: html.as_bytes().to_vec(),
                explicit_click: true,
            }
            .validate()
            .is_err());
        }

        RelayCommand {
            envelope_version: 1,
            opaque_id: "relay_1".into(),
            key_reference: "key_1".into(),
            nonce: vec![0; 24],
            ciphertext: vec![1],
            authentication_tag: vec![0; 16],
        }
        .validate()
        .unwrap();

        let candidates = vec!["candidate_1".to_string()];
        let digest = hex::encode(Sha256::digest(serde_json::to_vec(&candidates).unwrap()));
        let child = batch_child_id("https://list.test/one-page", &digest, "candidate_1").unwrap();
        let batch = BatchDiscovery {
            page_url: "https://list.test/one-page".into(),
            candidate_set_digest: digest,
            candidate_ids: candidates,
            selected_ids: vec!["candidate_1".into()],
            child_ids: vec![child],
        };
        batch.validate().unwrap();
        assert_eq!(
            batch.child_ids,
            vec![batch_child_id(
                &batch.page_url,
                &batch.candidate_set_digest,
                &batch.selected_ids[0]
            )
            .unwrap()]
        );
        let mut changed = batch;
        changed.candidate_ids.push("candidate_2".into());
        assert!(changed.validate().is_err());
        changed.candidate_set_digest = hex::encode(Sha256::digest(
            serde_json::to_vec(&changed.candidate_ids).unwrap(),
        ));
        changed.candidate_ids.reverse();
        assert!(changed.validate().is_err());

        let digest = hex::encode(Sha256::digest(
            serde_json::to_vec(&["candidate_1"]).unwrap(),
        ));
        assert_ne!(
            batch_child_id("https://list.test/one-page", &digest, "candidate_1").unwrap(),
            batch_child_id("https://list.test/other-page", &digest, "candidate_1").unwrap()
        );
    }
}
