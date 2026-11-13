//! Dormant MemeX capture mailbox v1 wire and cryptographic primitives.
//!
//! This module is deliberately transport-free. It parses bounded canonical wire
//! values, opens one authenticated HPKE command, and builds the exact HMAC
//! headers a future owner-controlled transport may use. Nothing here polls,
//! queues, acknowledges, persists, or activates a relay.

use anyhow::Result;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use hmac::{Hmac, Mac};
use hpke::{
    aead::AesGcm256, kdf::HkdfSha256, kem::X25519HkdfSha256, single_shot_open, Deserializable,
    Kem as KemTrait, OpModeR,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use subtle::ConstantTimeEq;
use url::Url;

const INFO: &[u8] = b"memex-capture-hpke-v1";
const SUITE: &str = "DHKEM_X25519_HKDF_SHA256/HKDF_SHA256/AES_256_GCM";
const MAX_NAME_BYTES: usize = 64;
const MAX_BODY_BYTES: usize = 16_384;
const MAX_RESPONSE_BYTES: usize = 131_072;
const MAX_STATUS_RESPONSE_BYTES: usize = 4_096;
const MAX_ENVELOPE_BYTES: usize = 6_144;
const MAX_TARGET_BYTES: usize = 1_024;
const MAX_PAGE_COMMANDS: usize = 20;
const MIN_LEASE_MILLIS: u64 = 30_000;
const MAX_LEASE_MILLIS: u64 = 300_000;
const MAX_FENCE_BYTES: usize = 256;

type HmacSha256 = Hmac<Sha256>;

pub const MAILBOX_PROTOCOL: &str = "memex.mailbox.command.v1";
pub const CAPTURE_PROTOCOL: &str = "memex.capture-command.v1";

/// Maximum request body accepted by the dormant wire boundary.
pub const MAX_REQUEST_BODY_BYTES: usize = MAX_BODY_BYTES;
/// Maximum response body accepted by the dormant wire boundary.
pub const MAX_WIRE_RESPONSE_BYTES: usize = MAX_RESPONSE_BYTES;
/// Maximum status/settlement response body accepted by the dormant wire boundary.
pub const MAX_STATUS_BYTES: usize = MAX_STATUS_RESPONSE_BYTES;
/// Maximum serialized HPKE envelope accepted by the current Dashboard boundary.
pub const MAX_ENVELOPE_SERIALIZED_BYTES: usize = MAX_ENVELOPE_BYTES;
/// Maximum number of commands in one current Dashboard lease page.
pub const MAX_LEASE_PAGE_COMMANDS: usize = MAX_PAGE_COMMANDS;
/// Maximum lease lifetime relative to the injected validation clock.
pub const MAX_LEASE_LIFETIME_MILLIS: u64 = MAX_LEASE_MILLIS;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub version: String,
    pub suite: String,
    pub mode: String,
    pub recipient_key_id: String,
    pub binding_key_id: String,
    pub info: String,
    pub aad: String,
    pub enc: String,
    pub ciphertext: String,
}

/// Canonical enqueue command body. It is a wire value only; no caller sends it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MailboxRequest {
    pub protocol: String,
    pub command_id: String,
    pub idempotency_key: String,
    pub command_commitment: String,
    pub envelope: Envelope,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MailboxResult {
    pub result_id: String,
    pub item_id: String,
    pub revision_id: String,
    pub translation: MailboxTranslation,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MailboxTranslation {
    pub state: String,
}

/// Source ACK and post-ACK translation requests carry only this ID tuple.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MailboxAckResult {
    pub result_id: String,
    pub item_id: String,
    pub revision_id: String,
}

/// Canonical status/settlement response body.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MailboxResponse {
    pub receipt: String,
    pub state: String,
    pub result: Option<MailboxResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EnqueueResponse {
    pub receipt: String,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LeasePage {
    pub commands: Vec<LeasedCommand>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LeasedCommand {
    pub receipt: String,
    pub command_id: String,
    pub command_commitment: String,
    pub envelope: Envelope,
    pub lease: Lease,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Lease {
    pub owner: String,
    pub token: String,
    pub expires_at: u64,
    pub attempt: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HeartbeatRequest {
    pub lease_token: String,
    pub extend_seconds: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HeartbeatResponse {
    pub receipt: String,
    pub lease_expires_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProgressRequest {
    pub lease_token: String,
    pub state: String,
    pub result: Option<MailboxResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AckRequest {
    pub lease_token: String,
    pub result: MailboxAckResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TranslationRequest {
    pub lease_token: String,
    pub result: MailboxAckResult,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NackRequest {
    pub lease_token: String,
    pub disposition: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PairingManifest {
    pub protocol: String,
    pub mailbox_origin: String,
    pub producer_id: String,
    pub consumer_id: String,
    pub producer_hmac_key_id: String,
    pub consumer_hmac_key_id: String,
    pub recipient_key_id: String,
    pub recipient_public_key_sha256: String,
    pub binding_key_id: String,
    pub activated_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MailboxFence {
    pub receipt: String,
    pub lease_token: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Aad {
    protocol: String,
    command_id: String,
    producer_id: String,
    recipient_key_id: String,
    binding_key_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Plaintext {
    protocol: String,
    url: String,
}

fn reject(code: &'static str) -> anyhow::Error {
    anyhow::anyhow!("{code}")
}

fn decode_b64(value: &str, min: usize, max: usize, code: &'static str) -> Result<Vec<u8>> {
    if value.is_empty() || value.contains('=') {
        return Err(reject(code));
    }
    let bytes = URL_SAFE_NO_PAD.decode(value).map_err(|_| reject(code))?;
    if bytes.len() < min || bytes.len() > max || URL_SAFE_NO_PAD.encode(&bytes) != value {
        return Err(reject(code));
    }
    Ok(bytes)
}

fn parse_canonical_with_code<T>(body: &[u8], max_bytes: usize, code: &'static str) -> Result<T>
where
    T: DeserializeOwned + Serialize,
{
    if body.is_empty() || body.len() > max_bytes {
        return Err(reject(code));
    }
    let value: T = serde_json::from_slice(body).map_err(|_| reject(code))?;
    let canonical = serde_json::to_vec(&value).map_err(|_| reject(code))?;
    if canonical != body {
        return Err(reject(code));
    }
    Ok(value)
}

/// Parse a bounded canonical JSON value without granting it any runtime effect.
pub fn parse_canonical_json<T>(body: &[u8], max_bytes: usize) -> Result<T>
where
    T: DeserializeOwned + Serialize,
{
    parse_canonical_with_code(body, max_bytes, "mailbox_wire_rejected")
}

fn valid_name(value: &str) -> bool {
    (3..=MAX_NAME_BYTES).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._~-".contains(&byte))
}

/// Validate a canonical 16-byte wire identifier with its protocol prefix.
pub fn validate_wire_id(value: &str, prefix: &str) -> bool {
    let Some(suffix) = value.strip_prefix(prefix) else {
        return false;
    };
    suffix.len() == 22
        && URL_SAFE_NO_PAD
            .decode(suffix)
            .ok()
            .is_some_and(|bytes| bytes.len() == 16 && URL_SAFE_NO_PAD.encode(bytes) == suffix)
}

/// Validate the injected synthetic nonce without reading a clock or generating entropy.
pub fn validate_nonce(nonce: &str) -> Result<()> {
    decode_b64(nonce, 16, 32, "mailbox_request_rejected").map(|_| ())
}

pub fn validate_cursor(value: &str) -> bool {
    if value.contains('=') {
        return false;
    }
    URL_SAFE_NO_PAD
        .decode(value)
        .ok()
        .filter(|bytes| (1..=256).contains(&bytes.len()))
        .and_then(|bytes| {
            serde_json::from_slice::<(u64, String)>(&bytes)
                .ok()
                .map(|tuple| (bytes, tuple))
        })
        .is_some_and(|(bytes, tuple)| {
            serde_json::to_vec(&tuple).is_ok_and(|canonical| canonical == bytes)
                && validate_wire_id(&tuple.1, "rct_")
        })
}

fn validate_envelope_metadata(envelope: &Envelope, code: &'static str) -> Result<()> {
    if envelope.version != "memex.hpke-envelope.v1"
        || envelope.suite != SUITE
        || envelope.mode != "base"
        || !valid_name(&envelope.recipient_key_id)
        || !valid_name(&envelope.binding_key_id)
    {
        return Err(reject(code));
    }
    if decode_b64(&envelope.info, INFO.len(), INFO.len(), code)?.as_slice() != INFO {
        return Err(reject(code));
    }
    decode_b64(&envelope.aad, 64, 512, code)?;
    decode_b64(&envelope.enc, 32, 32, code)?;
    decode_b64(&envelope.ciphertext, 17, 4_096, code)?;
    if serde_json::to_vec(envelope)
        .map_err(|_| reject(code))?
        .len()
        > MAX_ENVELOPE_BYTES
    {
        return Err(reject(code));
    }
    Ok(())
}

pub fn validate_mailbox_request(request: &MailboxRequest) -> Result<()> {
    if request.protocol != MAILBOX_PROTOCOL
        || !validate_wire_id(&request.command_id, "cmd_")
        || !(16..=128).contains(&request.idempotency_key.len())
        || !request
            .idempotency_key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._~-".contains(&byte))
    {
        return Err(reject("mailbox_request_rejected"));
    }
    decode_b64(
        &request.command_commitment,
        32,
        32,
        "mailbox_request_rejected",
    )?;
    validate_envelope_metadata(&request.envelope, "mailbox_request_rejected")?;
    if serde_json::to_vec(&request.envelope)
        .map_err(|_| reject("mailbox_request_rejected"))?
        .len()
        > MAX_ENVELOPE_BYTES
    {
        return Err(reject("mailbox_request_rejected"));
    }
    Ok(())
}

pub fn parse_mailbox_request(body: &[u8]) -> Result<MailboxRequest> {
    let request: MailboxRequest =
        parse_canonical_with_code(body, MAX_BODY_BYTES, "mailbox_request_rejected")?;
    validate_mailbox_request(&request)?;
    Ok(request)
}

fn valid_source_state(value: &str) -> bool {
    matches!(
        value,
        "queued"
            | "fetching"
            | "processing"
            | "stored"
            | "publishing"
            | "source_ready"
            | "published"
            | "failed"
    )
}

fn valid_translation_state(value: &str) -> bool {
    matches!(value, "queued" | "running" | "failed" | "succeeded")
}

fn valid_result_translation_state(value: &str) -> bool {
    valid_source_translation_state(value) || valid_translation_state(value)
}

fn valid_source_translation_state(value: &str) -> bool {
    matches!(value, "queued" | "not_required")
}

fn validate_result(result: &MailboxResult, code: &'static str) -> Result<()> {
    if !validate_wire_id(&result.result_id, "result_")
        || !valid_store_id(&result.item_id, "item_")
        || !valid_store_id(&result.revision_id, "revision_")
        || !valid_result_translation_state(&result.translation.state)
    {
        return Err(reject(code));
    }
    Ok(())
}

fn validate_source_result(result: &MailboxResult, code: &'static str) -> Result<()> {
    validate_result(result, code)?;
    if !valid_source_translation_state(&result.translation.state) {
        return Err(reject(code));
    }
    Ok(())
}

fn validate_ack_result(result: &MailboxAckResult, code: &'static str) -> Result<()> {
    if !validate_wire_id(&result.result_id, "result_")
        || !valid_store_id(&result.item_id, "item_")
        || !valid_store_id(&result.revision_id, "revision_")
    {
        return Err(reject(code));
    }
    Ok(())
}

fn valid_store_id(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|suffix| {
        (1..=128).contains(&suffix.len())
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    })
}

fn validate_response_shape(response: &MailboxResponse, code: &'static str) -> Result<()> {
    if !validate_wire_id(&response.receipt, "rct_") || !valid_source_state(&response.state) {
        return Err(reject(code));
    }
    if let Some(result) = &response.result {
        validate_result(result, code)?;
    }
    Ok(())
}

pub fn parse_mailbox_response(body: &[u8]) -> Result<MailboxResponse> {
    let response: MailboxResponse =
        parse_canonical_with_code(body, MAX_STATUS_RESPONSE_BYTES, "mailbox_response_rejected")?;
    validate_response_shape(&response, "mailbox_response_rejected")?;
    Ok(response)
}

pub fn validate_enqueue_response(response: &EnqueueResponse) -> Result<()> {
    if !validate_wire_id(&response.receipt, "rct_") || response.state != "queued" {
        return Err(reject("mailbox_response_rejected"));
    }
    Ok(())
}

pub fn parse_enqueue_response(body: &[u8]) -> Result<EnqueueResponse> {
    let response: EnqueueResponse =
        parse_canonical_with_code(body, MAX_STATUS_RESPONSE_BYTES, "mailbox_response_rejected")?;
    validate_enqueue_response(&response)?;
    Ok(response)
}

pub fn validate_mailbox_response(
    response: &MailboxResponse,
    expected_receipt: &str,
    expected_state: &str,
    expected_result: Option<&MailboxResult>,
) -> Result<()> {
    validate_response_shape(response, "mailbox_response_rejected")?;
    if response.receipt != expected_receipt || response.state != expected_state {
        return Err(reject("mailbox_response_rejected"));
    }
    match (&response.result, expected_result) {
        (None, None) => Ok(()),
        (Some(actual), Some(expected)) if actual == expected => Ok(()),
        _ => Err(reject("mailbox_response_rejected")),
    }
}

fn validate_lease_shape(lease: &Lease, code: &'static str) -> Result<()> {
    if !valid_name(&lease.owner)
        || !validate_wire_id(&lease.token, "lease_")
        || !(1..=5).contains(&lease.attempt)
        || lease.expires_at == 0
    {
        return Err(reject(code));
    }
    Ok(())
}

pub fn validate_lease(lease: &Lease, expected_owner: &str, now_millis: u64) -> Result<()> {
    validate_lease_shape(lease, "mailbox_lease_rejected")?;
    let earliest = now_millis
        .checked_add(MIN_LEASE_MILLIS)
        .ok_or_else(|| reject("mailbox_lease_rejected"))?;
    let latest = now_millis
        .checked_add(MAX_LEASE_MILLIS)
        .ok_or_else(|| reject("mailbox_lease_rejected"))?;
    if lease.owner != expected_owner || lease.expires_at < earliest || lease.expires_at > latest {
        return Err(reject("mailbox_lease_rejected"));
    }
    Ok(())
}

fn validate_lease_page_shape(
    page: &LeasePage,
    max_commands: usize,
    code: &'static str,
) -> Result<()> {
    if max_commands == 0
        || max_commands > MAX_PAGE_COMMANDS
        || page.commands.len() > max_commands
        || page
            .next_cursor
            .as_deref()
            .is_some_and(|cursor| !validate_cursor(cursor))
        || (page.commands.is_empty() && page.next_cursor.is_some())
    {
        return Err(reject(code));
    }
    let mut receipts = HashSet::with_capacity(page.commands.len());
    let mut commands = HashSet::with_capacity(page.commands.len());
    for command in &page.commands {
        if !validate_wire_id(&command.receipt, "rct_")
            || !validate_wire_id(&command.command_id, "cmd_")
            || !receipts.insert(&command.receipt)
            || !commands.insert(&command.command_id)
        {
            return Err(reject(code));
        }
        validate_lease_shape(&command.lease, code)?;
        validate_envelope_metadata(&command.envelope, code)?;
        decode_b64(&command.command_commitment, 32, 32, code)?;
    }
    Ok(())
}

pub fn validate_lease_page(
    page: &LeasePage,
    max_commands: usize,
    expected_owner: &str,
    now_millis: u64,
) -> Result<()> {
    validate_lease_page_shape(page, max_commands, "mailbox_lease_rejected")?;
    for command in &page.commands {
        validate_lease(&command.lease, expected_owner, now_millis)?;
    }
    Ok(())
}

pub fn parse_lease_page(body: &[u8]) -> Result<LeasePage> {
    let page: LeasePage =
        parse_canonical_with_code(body, MAX_RESPONSE_BYTES, "mailbox_response_rejected")?;
    validate_lease_page_shape(&page, MAX_PAGE_COMMANDS, "mailbox_response_rejected")?;
    Ok(page)
}

pub fn validate_heartbeat_request(request: &HeartbeatRequest) -> Result<()> {
    if !validate_wire_id(&request.lease_token, "lease_")
        || !(30..=300).contains(&request.extend_seconds)
    {
        return Err(reject("mailbox_request_rejected"));
    }
    Ok(())
}

pub fn parse_heartbeat_request(body: &[u8]) -> Result<HeartbeatRequest> {
    let request: HeartbeatRequest =
        parse_canonical_with_code(body, MAX_STATUS_RESPONSE_BYTES, "mailbox_request_rejected")?;
    validate_heartbeat_request(&request)?;
    Ok(request)
}

fn validate_heartbeat_response_shape(
    response: &HeartbeatResponse,
    code: &'static str,
) -> Result<()> {
    if !validate_wire_id(&response.receipt, "rct_") || response.lease_expires_at == 0 {
        return Err(reject(code));
    }
    Ok(())
}

pub fn validate_heartbeat_response(
    response: &HeartbeatResponse,
    expected_receipt: &str,
    previous_expires_at: u64,
    now_millis: u64,
) -> Result<()> {
    validate_heartbeat_response_shape(response, "mailbox_response_rejected")?;
    let latest = now_millis
        .checked_add(MAX_LEASE_MILLIS)
        .ok_or_else(|| reject("mailbox_response_rejected"))?;
    if response.receipt != expected_receipt
        || response.lease_expires_at < previous_expires_at
        || response.lease_expires_at <= now_millis
        || response.lease_expires_at > latest
    {
        return Err(reject("mailbox_response_rejected"));
    }
    Ok(())
}

pub fn parse_heartbeat_response(body: &[u8]) -> Result<HeartbeatResponse> {
    let response: HeartbeatResponse =
        parse_canonical_with_code(body, MAX_STATUS_RESPONSE_BYTES, "mailbox_response_rejected")?;
    validate_heartbeat_response_shape(&response, "mailbox_response_rejected")?;
    Ok(response)
}

pub fn validate_progress_request(request: &ProgressRequest) -> Result<()> {
    if !validate_wire_id(&request.lease_token, "lease_") {
        return Err(reject("mailbox_request_rejected"));
    }
    match (request.state.as_str(), request.result.as_ref()) {
        ("fetching" | "processing", None) => Ok(()),
        ("stored" | "publishing" | "source_ready", Some(result)) => {
            validate_source_result(result, "mailbox_request_rejected")
        }
        _ => Err(reject("mailbox_request_rejected")),
    }
}

pub fn parse_progress_request(body: &[u8]) -> Result<ProgressRequest> {
    let request: ProgressRequest =
        parse_canonical_with_code(body, MAX_STATUS_RESPONSE_BYTES, "mailbox_request_rejected")?;
    validate_progress_request(&request)?;
    Ok(request)
}

pub fn validate_ack_request(request: &AckRequest) -> Result<()> {
    if !validate_wire_id(&request.lease_token, "lease_") {
        return Err(reject("mailbox_request_rejected"));
    }
    validate_ack_result(&request.result, "mailbox_request_rejected")
}

pub fn parse_ack_request(body: &[u8]) -> Result<AckRequest> {
    let request: AckRequest =
        parse_canonical_with_code(body, MAX_STATUS_RESPONSE_BYTES, "mailbox_request_rejected")?;
    validate_ack_request(&request)?;
    Ok(request)
}

pub fn validate_translation_request(request: &TranslationRequest) -> Result<()> {
    if !validate_wire_id(&request.lease_token, "lease_") || !valid_translation_state(&request.state)
    {
        return Err(reject("mailbox_request_rejected"));
    }
    validate_ack_result(&request.result, "mailbox_request_rejected")
}

pub fn parse_translation_request(body: &[u8]) -> Result<TranslationRequest> {
    let request: TranslationRequest =
        parse_canonical_with_code(body, MAX_STATUS_RESPONSE_BYTES, "mailbox_request_rejected")?;
    validate_translation_request(&request)?;
    Ok(request)
}

pub fn validate_nack_request(request: &NackRequest) -> Result<()> {
    if !validate_wire_id(&request.lease_token, "lease_")
        || !matches!(request.disposition.as_str(), "retry" | "dead_letter")
    {
        return Err(reject("mailbox_request_rejected"));
    }
    Ok(())
}

pub fn parse_nack_request(body: &[u8]) -> Result<NackRequest> {
    let request: NackRequest =
        parse_canonical_with_code(body, MAX_STATUS_RESPONSE_BYTES, "mailbox_request_rejected")?;
    validate_nack_request(&request)?;
    Ok(request)
}

pub fn validate_pairing_manifest(manifest: &PairingManifest) -> Result<()> {
    let origin =
        Url::parse(&manifest.mailbox_origin).map_err(|_| reject("mailbox_pairing_rejected"))?;
    if manifest.protocol != "memex.mailbox-pairing.v1"
        || origin.scheme() != "https"
        || origin.host_str().is_none()
        || !origin.username().is_empty()
        || origin.password().is_some()
        || origin.port().is_some()
        || !matches!(origin.path(), "" | "/")
        || origin.query().is_some()
        || origin.fragment().is_some()
        || !valid_name(&manifest.producer_id)
        || !valid_name(&manifest.consumer_id)
        || manifest.producer_id == manifest.consumer_id
        || !valid_name(&manifest.producer_hmac_key_id)
        || !valid_name(&manifest.consumer_hmac_key_id)
        || !valid_name(&manifest.recipient_key_id)
        || !valid_name(&manifest.binding_key_id)
        || manifest.recipient_public_key_sha256.len() != 64
        || !manifest
            .recipient_public_key_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        || manifest.activated_at == 0
    {
        return Err(reject("mailbox_pairing_rejected"));
    }
    Ok(())
}

pub fn parse_pairing_manifest(body: &[u8]) -> Result<PairingManifest> {
    let manifest: PairingManifest =
        parse_canonical_with_code(body, MAX_STATUS_RESPONSE_BYTES, "mailbox_pairing_rejected")?;
    validate_pairing_manifest(&manifest)?;
    Ok(manifest)
}

pub fn parse_fence(body: &[u8]) -> Result<MailboxFence> {
    let fence: MailboxFence =
        parse_canonical_with_code(body, MAX_FENCE_BYTES, "mailbox_fence_rejected")?;
    validate_fence(&fence)?;
    Ok(fence)
}

pub fn validate_fence(fence: &MailboxFence) -> Result<()> {
    if !validate_wire_id(&fence.receipt, "rct_") || !validate_wire_id(&fence.lease_token, "lease_")
    {
        return Err(reject("mailbox_fence_rejected"));
    }
    Ok(())
}

/// Return the exact canonical URL accepted by the existing credentialless URL boundary.
pub fn validate_command_url(value: &str) -> Result<String> {
    let (canonical, _) =
        crate::url_input::extract_url(value).map_err(|_| reject("mailbox_ciphertext_rejected"))?;
    if canonical != value {
        return Err(reject("mailbox_ciphertext_rejected"));
    }
    Ok(canonical)
}

/// Compute the command commitment over the exact decrypted plaintext bytes.
pub fn command_commitment(binding_key: &[u8], plaintext: &[u8]) -> Result<String> {
    if !(32..=64).contains(&binding_key.len())
        || plaintext.is_empty()
        || plaintext.len() > MAX_BODY_BYTES
    {
        return Err(reject("mailbox_ciphertext_rejected"));
    }
    let mut mac = HmacSha256::new_from_slice(binding_key)
        .map_err(|_| reject("mailbox_ciphertext_rejected"))?;
    mac.update(b"memex-command-commitment-v1\n");
    mac.update(plaintext);
    Ok(URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
}

/// Open one HPKE command and return only its canonical, URL-shaped plaintext.
#[allow(clippy::too_many_arguments)]
pub fn open_command(
    envelope: &Envelope,
    command_id: &str,
    producer_id: &str,
    expected_recipient_key_id: &str,
    expected_binding_key_id: &str,
    private_key: &[u8],
    binding_key: &[u8],
    commitment: &str,
) -> Result<String> {
    if !validate_wire_id(command_id, "cmd_")
        || !valid_name(producer_id)
        || !valid_name(expected_recipient_key_id)
        || !valid_name(expected_binding_key_id)
        || envelope.recipient_key_id != expected_recipient_key_id
        || envelope.binding_key_id != expected_binding_key_id
        || private_key.len() != 32
        || !(32..=64).contains(&binding_key.len())
    {
        return Err(reject("mailbox_ciphertext_rejected"));
    }
    validate_envelope_metadata(envelope, "mailbox_ciphertext_rejected")?;
    let aad_bytes = decode_b64(&envelope.aad, 64, 512, "mailbox_ciphertext_rejected")?;
    let aad: Aad =
        serde_json::from_slice(&aad_bytes).map_err(|_| reject("mailbox_ciphertext_rejected"))?;
    let canonical_aad =
        serde_json::to_vec(&aad).map_err(|_| reject("mailbox_ciphertext_rejected"))?;
    if canonical_aad != aad_bytes
        || aad.protocol != CAPTURE_PROTOCOL
        || aad.command_id != command_id
        || aad.producer_id != producer_id
        || aad.recipient_key_id != envelope.recipient_key_id
        || aad.binding_key_id != envelope.binding_key_id
    {
        return Err(reject("mailbox_ciphertext_rejected"));
    }

    let sk = <X25519HkdfSha256 as KemTrait>::PrivateKey::from_bytes(private_key)
        .map_err(|_| reject("mailbox_ciphertext_rejected"))?;
    let enc = <X25519HkdfSha256 as KemTrait>::EncappedKey::from_bytes(&decode_b64(
        &envelope.enc,
        32,
        32,
        "mailbox_ciphertext_rejected",
    )?)
    .map_err(|_| reject("mailbox_ciphertext_rejected"))?;
    let ciphertext = decode_b64(
        &envelope.ciphertext,
        17,
        4_096,
        "mailbox_ciphertext_rejected",
    )?;
    let plaintext = single_shot_open::<AesGcm256, HkdfSha256, X25519HkdfSha256>(
        &OpModeR::Base,
        &sk,
        &enc,
        INFO,
        &ciphertext,
        &aad_bytes,
    )
    .map_err(|_| reject("mailbox_ciphertext_rejected"))?;
    let supplied = decode_b64(commitment, 32, 32, "mailbox_ciphertext_rejected")?;
    let expected = command_commitment(binding_key, &plaintext)?;
    let expected_bytes = decode_b64(&expected, 32, 32, "mailbox_ciphertext_rejected")?;
    if expected_bytes.as_slice().ct_eq(&supplied).unwrap_u8() != 1 {
        return Err(reject("mailbox_ciphertext_rejected"));
    }
    let value: Plaintext =
        serde_json::from_slice(&plaintext).map_err(|_| reject("mailbox_ciphertext_rejected"))?;
    let canonical_plaintext =
        serde_json::to_vec(&value).map_err(|_| reject("mailbox_ciphertext_rejected"))?;
    if canonical_plaintext != plaintext || value.protocol != CAPTURE_PROTOCOL {
        return Err(reject("mailbox_ciphertext_rejected"));
    }
    validate_command_url(&value.url)
}

pub fn body_sha256(body: &[u8]) -> Result<String> {
    if body.len() > MAX_BODY_BYTES {
        return Err(reject("mailbox_request_rejected"));
    }
    Ok(hex::encode(Sha256::digest(body)))
}

pub fn canonical_auth_input(
    principal: &str,
    key_id: &str,
    method: &str,
    target: &str,
    timestamp: &str,
    nonce: &str,
    body: &[u8],
) -> Result<(String, String)> {
    if !matches!(method, "GET" | "POST")
        || !timestamp.bytes().all(|byte| byte.is_ascii_digit())
        || timestamp.len() != 10
        || !target.starts_with('/')
        || target.starts_with("//")
        || target.len() > MAX_TARGET_BYTES
        || target.contains(['#', '\r', '\n'])
        || !valid_name(principal)
        || !valid_name(key_id)
    {
        return Err(reject("mailbox_request_rejected"));
    }
    validate_nonce(nonce)?;
    let digest = body_sha256(body)?;
    Ok((
        format!(
            "memex-mailbox-hmac-v1\nconsumer\n{principal}\n{key_id}\n{method}\n{target}\n{timestamp}\n{nonce}\n{digest}"
        ),
        digest,
    ))
}

#[allow(clippy::too_many_arguments)]
pub fn signed_headers(
    principal: &str,
    key_id: &str,
    key: &[u8],
    method: &str,
    target: &str,
    timestamp: &str,
    nonce: &str,
    body: &[u8],
) -> Result<BTreeMap<String, String>> {
    if !(32..=64).contains(&key.len()) {
        return Err(reject("mailbox_request_rejected"));
    }
    let (input, digest) =
        canonical_auth_input(principal, key_id, method, target, timestamp, nonce, body)?;
    let mut mac =
        HmacSha256::new_from_slice(key).map_err(|_| reject("mailbox_request_rejected"))?;
    mac.update(input.as_bytes());
    let mut headers = BTreeMap::new();
    for (name, value) in [
        ("x-memex-auth-role", "consumer".to_owned()),
        ("x-memex-auth-id", principal.to_owned()),
        ("x-memex-auth-key-id", key_id.to_owned()),
        ("x-memex-auth-timestamp", timestamp.to_owned()),
        ("x-memex-auth-nonce", nonce.to_owned()),
        ("x-memex-content-sha256", digest),
        (
            "x-memex-signature",
            URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()),
        ),
    ] {
        headers.insert(name.to_owned(), value);
    }
    Ok(headers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::{collections::BTreeSet, path::Path, str};

    const VECTORS: &str =
        include_str!("../../../interfaces/fixtures/memex-capture-mailbox-v1-vectors.json");

    fn vectors() -> Value {
        serde_json::from_str(VECTORS).expect("synthetic vectors are valid JSON")
    }

    fn text<'a>(value: &'a Value, key: &str) -> &'a str {
        value["canonical_json"][key]
            .as_str()
            .expect("canonical fixture string")
    }

    fn field<'a>(value: &'a Value, section: &str, key: &str) -> &'a str {
        value[section][key].as_str().expect("fixture field string")
    }

    fn fixture_crypto_inputs() -> (MailboxRequest, Vec<u8>, Vec<u8>, String) {
        let value = vectors();
        let request = parse_mailbox_request(text(&value, "enqueue_request_utf8").as_bytes())
            .expect("fixture request validates");
        let private = URL_SAFE_NO_PAD
            .decode(field(&value, "hpke", "recipient_private_key_base64url"))
            .expect("synthetic private key encoding");
        let binding = URL_SAFE_NO_PAD
            .decode(field(&value, "command_commitment", "key_base64url"))
            .expect("synthetic binding key encoding");
        let pairing = parse_pairing_manifest(text(&value, "pairing_manifest_utf8").as_bytes())
            .expect("synthetic pairing manifest");
        (request, private, binding, pairing.producer_id)
    }

    #[allow(clippy::too_many_arguments)]
    fn open_fixture(
        envelope: &Envelope,
        command_id: &str,
        producer: &str,
        recipient_key_id: &str,
        binding_key_id: &str,
        private: &[u8],
        binding: &[u8],
        commitment: &str,
    ) -> Result<String> {
        open_command(
            envelope,
            command_id,
            producer,
            recipient_key_id,
            binding_key_id,
            private,
            binding,
            commitment,
        )
    }

    fn assert_ciphertext_rejected(result: Result<String>) {
        let error = result.expect_err("mutation must reject").to_string();
        assert_eq!(error, "mailbox_ciphertext_rejected");
        for sentinel in [
            "https://capture.invalid/article",
            "memex-capture-hpke-v1",
            "recipient_private_key_base64url",
            "ciphertext_and_tag_base64url",
        ] {
            assert!(
                !error.contains(sentinel),
                "error leaked sentinel: {sentinel}"
            );
        }
    }

    fn flip_last(value: &str) -> String {
        let mut bytes = value.as_bytes().to_vec();
        let last = bytes.last_mut().expect("non-empty base64 value");
        *last = if *last == b'A' { b'B' } else { b'A' };
        String::from_utf8(bytes).expect("ASCII base64 value")
    }

    fn assert_canonical_round_trip<T, F>(raw: &str, parse: F)
    where
        T: Serialize,
        F: Fn(&[u8]) -> Result<T>,
    {
        let value = parse(raw.as_bytes()).expect("canonical wire");
        assert_eq!(
            serde_json::to_vec(&value).expect("serialize wire"),
            raw.as_bytes()
        );
    }

    #[test]
    fn historical_fixture_is_exact_and_synthetic() {
        let value = vectors();
        assert_eq!(VECTORS.len(), 13982);
        assert_eq!(
            hex::encode(Sha256::digest(VECTORS.as_bytes())),
            "b82e2846165c3c18df3a91d88d7eea0c9340a8b5c002f49fa8ba76984bfaf85a"
        );
        assert_eq!(value["schema_version"], 1);
        assert!(value["warning"]
            .as_str()
            .expect("synthetic warning")
            .contains("PUBLIC SYNTHETIC TEST DATA ONLY"));
        assert_eq!(value["canonical_json"].as_object().unwrap().len(), 25);
        assert_eq!(value["traces"]["source_only"].as_array().unwrap().len(), 6);
        assert_eq!(
            value["traces"]["translation_succeeded"]
                .as_array()
                .unwrap()
                .len(),
            8
        );
        assert_eq!(
            value["traces"]["translation_retry"]
                .as_array()
                .unwrap()
                .len(),
            10
        );
    }

    #[test]
    fn canonical_wire_types_match_fixture_bytes() {
        let value = vectors();
        assert_canonical_round_trip(text(&value, "enqueue_request_utf8"), parse_mailbox_request);
        assert_canonical_round_trip(
            text(&value, "enqueue_response_utf8"),
            parse_enqueue_response,
        );
        assert_canonical_round_trip(text(&value, "lease_response_utf8"), parse_lease_page);
        assert_canonical_round_trip(
            text(&value, "pairing_manifest_utf8"),
            parse_pairing_manifest,
        );
        assert_canonical_round_trip(
            text(&value, "heartbeat_request_utf8"),
            parse_heartbeat_request,
        );
        assert_canonical_round_trip(
            text(&value, "heartbeat_response_utf8"),
            parse_heartbeat_response,
        );
        for key in [
            "progress_fetching_request_utf8",
            "progress_processing_request_utf8",
            "progress_stored_request_utf8",
            "progress_stored_queued_request_utf8",
            "progress_publishing_queued_request_utf8",
            "progress_source_ready_queued_request_utf8",
        ] {
            assert_canonical_round_trip(text(&value, key), parse_progress_request);
        }
        assert_canonical_round_trip(text(&value, "ack_request_utf8"), parse_ack_request);
        for key in [
            "translation_running_request_utf8",
            "translation_failed_request_utf8",
            "translation_retry_request_utf8",
            "translation_succeeded_request_utf8",
        ] {
            assert_canonical_round_trip(text(&value, key), parse_translation_request);
        }
        for key in ["nack_retry_request_utf8", "nack_dead_letter_request_utf8"] {
            assert_canonical_round_trip(text(&value, key), parse_nack_request);
        }
        for key in [
            "ack_response_utf8",
            "nack_retry_response_utf8",
            "nack_dead_letter_response_utf8",
            "status_stored_response_utf8",
            "status_source_ready_response_utf8",
            "status_published_running_response_utf8",
        ] {
            assert_canonical_round_trip(text(&value, key), parse_mailbox_response);
        }
    }

    #[test]
    fn hpke_commitment_and_hmac_vectors_are_byte_exact() {
        let value = vectors();
        let (request, private, binding, producer) = fixture_crypto_inputs();
        let url = open_fixture(
            &request.envelope,
            &request.command_id,
            &producer,
            &request.envelope.recipient_key_id,
            &request.envelope.binding_key_id,
            &private,
            &binding,
            &request.command_commitment,
        )
        .expect("HPKE fixture opens");
        assert_eq!(url, "https://capture.invalid/article");
        let plaintext = URL_SAFE_NO_PAD
            .decode(field(&value, "hpke", "plaintext_base64url"))
            .expect("synthetic plaintext encoding");
        assert_eq!(
            command_commitment(&binding, &plaintext).unwrap(),
            field(&value, "command_commitment", "commitment_base64url")
        );

        let key = URL_SAFE_NO_PAD
            .decode(field(&value, "hmac", "key_base64url"))
            .expect("synthetic HMAC key encoding");
        let (canonical, digest) = canonical_auth_input(
            field(&value, "hmac", "principal_id"),
            field(&value, "hmac", "key_id"),
            field(&value, "hmac", "method"),
            field(&value, "hmac", "target"),
            field(&value, "hmac", "timestamp"),
            field(&value, "hmac", "nonce_base64url"),
            b"",
        )
        .unwrap();
        assert_eq!(canonical, field(&value, "hmac", "canonical_input_utf8"));
        assert_eq!(digest, field(&value, "hmac", "body_sha256"));
        assert_eq!(body_sha256(b"").unwrap(), digest);
        let headers = signed_headers(
            field(&value, "hmac", "principal_id"),
            field(&value, "hmac", "key_id"),
            &key,
            field(&value, "hmac", "method"),
            field(&value, "hmac", "target"),
            field(&value, "hmac", "timestamp"),
            field(&value, "hmac", "nonce_base64url"),
            b"",
        )
        .unwrap();
        assert_eq!(
            headers["x-memex-signature"],
            field(&value, "hmac", "signature_base64url")
        );

        let translation_body = text(&value, "translation_running_request_utf8").as_bytes();
        let (canonical, digest) = canonical_auth_input(
            field(&value, "translation_hmac", "principal_id"),
            field(&value, "translation_hmac", "key_id"),
            "POST",
            field(&value, "translation_hmac", "target"),
            field(&value, "translation_hmac", "timestamp"),
            field(&value, "translation_hmac", "nonce_base64url"),
            translation_body,
        )
        .unwrap();
        assert_eq!(
            canonical,
            field(&value, "translation_hmac", "canonical_input_utf8")
        );
        assert_eq!(digest, field(&value, "translation_hmac", "body_sha256"));
        let headers = signed_headers(
            field(&value, "translation_hmac", "principal_id"),
            field(&value, "translation_hmac", "key_id"),
            &URL_SAFE_NO_PAD
                .decode(field(&value, "translation_hmac", "key_base64url"))
                .unwrap(),
            "POST",
            field(&value, "translation_hmac", "target"),
            field(&value, "translation_hmac", "timestamp"),
            field(&value, "translation_hmac", "nonce_base64url"),
            translation_body,
        )
        .unwrap();
        assert_eq!(
            headers["x-memex-signature"],
            field(&value, "translation_hmac", "signature_base64url")
        );
    }

    #[test]
    fn hpke_mutations_reject_without_error_disclosure() {
        let (request, private, binding, producer) = fixture_crypto_inputs();
        let recipient = request.envelope.recipient_key_id.clone();
        let binding_id = request.envelope.binding_key_id.clone();
        let mut envelope = request.envelope.clone();
        let open = |candidate: &Envelope,
                    command_id: &str,
                    producer_id: &str,
                    recipient_id: &str,
                    binding_key_id: &str,
                    private_key: &[u8],
                    binding_key: &[u8],
                    commitment: &str| {
            open_fixture(
                candidate,
                command_id,
                producer_id,
                recipient_id,
                binding_key_id,
                private_key,
                binding_key,
                commitment,
            )
        };

        assert_ciphertext_rejected(open(
            &envelope,
            "cmd_AgICAgICAgICAgICAgICAg",
            &producer,
            &recipient,
            &binding_id,
            &private,
            &binding,
            &request.command_commitment,
        ));
        assert_ciphertext_rejected(open(
            &envelope,
            &request.command_id,
            "producer-other",
            &recipient,
            &binding_id,
            &private,
            &binding,
            &request.command_commitment,
        ));
        assert_ciphertext_rejected(open(
            &envelope,
            &request.command_id,
            &producer,
            "agent-key-other",
            &binding_id,
            &private,
            &binding,
            &request.command_commitment,
        ));
        assert_ciphertext_rejected(open(
            &envelope,
            &request.command_id,
            &producer,
            &recipient,
            "binding-key-other",
            &private,
            &binding,
            &request.command_commitment,
        ));

        envelope.info.push('=');
        assert_ciphertext_rejected(open(
            &envelope,
            &request.command_id,
            &producer,
            &recipient,
            &binding_id,
            &private,
            &binding,
            &request.command_commitment,
        ));
        envelope = request.envelope.clone();
        envelope.info = URL_SAFE_NO_PAD.encode(vec![0; INFO.len()]);
        assert_ciphertext_rejected(open(
            &envelope,
            &request.command_id,
            &producer,
            &recipient,
            &binding_id,
            &private,
            &binding,
            &request.command_commitment,
        ));
        envelope = request.envelope.clone();
        let aad = URL_SAFE_NO_PAD.decode(&envelope.aad).expect("fixture AAD");
        envelope.aad = URL_SAFE_NO_PAD.encode(format!(" {}", str::from_utf8(&aad).unwrap()));
        assert_ciphertext_rejected(open(
            &envelope,
            &request.command_id,
            &producer,
            &recipient,
            &binding_id,
            &private,
            &binding,
            &request.command_commitment,
        ));
        envelope = request.envelope.clone();
        envelope.enc = flip_last(&envelope.enc);
        assert_ciphertext_rejected(open(
            &envelope,
            &request.command_id,
            &producer,
            &recipient,
            &binding_id,
            &private,
            &binding,
            &request.command_commitment,
        ));
        envelope = request.envelope.clone();
        envelope.ciphertext = flip_last(&envelope.ciphertext);
        assert_ciphertext_rejected(open(
            &envelope,
            &request.command_id,
            &producer,
            &recipient,
            &binding_id,
            &private,
            &binding,
            &request.command_commitment,
        ));
        let mut wrong_private = private.clone();
        wrong_private[1] ^= 1;
        assert_ciphertext_rejected(open(
            &request.envelope,
            &request.command_id,
            &producer,
            &recipient,
            &binding_id,
            &wrong_private,
            &binding,
            &request.command_commitment,
        ));
        assert_ciphertext_rejected(open(
            &request.envelope,
            &request.command_id,
            &producer,
            &recipient,
            &binding_id,
            &private,
            &binding,
            &flip_last(&request.command_commitment),
        ));
    }

    #[test]
    fn wire_bounds_unknown_fields_padding_urls_and_errors_are_closed() {
        let value = vectors();
        let request_raw = text(&value, "enqueue_request_utf8");
        let mut request_value: Value = serde_json::from_str(request_raw).unwrap();
        request_value
            .as_object_mut()
            .unwrap()
            .insert("unknown".to_owned(), Value::Bool(true));
        let unknown_request = serde_json::to_vec(&request_value).unwrap();
        assert_eq!(
            parse_mailbox_request(&unknown_request)
                .unwrap_err()
                .to_string(),
            "mailbox_request_rejected"
        );
        assert_eq!(
            parse_mailbox_request(format!(" {request_raw}").as_bytes())
                .unwrap_err()
                .to_string(),
            "mailbox_request_rejected"
        );
        let mut response_value: Value =
            serde_json::from_str(text(&value, "ack_response_utf8")).unwrap();
        response_value
            .as_object_mut()
            .unwrap()
            .insert("unknown".to_owned(), Value::Bool(true));
        assert_eq!(
            parse_mailbox_response(&serde_json::to_vec(&response_value).unwrap())
                .unwrap_err()
                .to_string(),
            "mailbox_response_rejected"
        );
        assert!(!validate_wire_id("rct_AgICAgICAgICAgICAgICAg=", "rct_"));
        assert!(validate_nonce(field(&value, "hmac", "nonce_base64url")).is_ok());
        assert!(validate_nonce(&format!("{}=", field(&value, "hmac", "nonce_base64url"))).is_err());
        assert!(validate_command_url("https://capture.invalid/article").is_ok());
        for malformed in [
            "not-a-url",
            "https://user:pass@example.test/",
            "https://example.test/#fragment",
            "https://example.test:8443/",
            "https://a.test https://b.test",
        ] {
            let error = validate_command_url(malformed).unwrap_err().to_string();
            assert_eq!(error, "mailbox_ciphertext_rejected");
            assert!(!error.contains(malformed));
        }
        assert!(
            parse_canonical_json::<Value>(&vec![b' '; MAX_BODY_BYTES + 1], MAX_BODY_BYTES).is_err()
        );
        assert!(canonical_auth_input(
            "principal-test",
            "key-test",
            "POST",
            "/v1/commands",
            "1786414600",
            field(&value, "hmac", "nonce_base64url"),
            &vec![0; MAX_BODY_BYTES + 1],
        )
        .is_err());
    }

    #[test]
    fn lease_fence_and_result_validation_binds_synthetic_state() {
        let value = vectors();
        let page = parse_lease_page(text(&value, "lease_response_utf8").as_bytes()).unwrap();
        let now = 1_786_414_600_000;
        validate_lease_page(&page, 2, "mac-agent-test", now).unwrap();
        let heartbeat =
            parse_heartbeat_response(text(&value, "heartbeat_response_utf8").as_bytes()).unwrap();
        validate_heartbeat_response(
            &heartbeat,
            &page.commands[0].receipt,
            page.commands[0].lease.expires_at,
            now,
        )
        .unwrap();
        assert!(validate_cursor(
            value["cursor"]["base64url"].as_str().unwrap()
        ));

        let mut wrong_owner = page.clone();
        wrong_owner.commands[0].lease.owner = "other-agent".into();
        assert_eq!(
            validate_lease_page(&wrong_owner, 2, "mac-agent-test", now)
                .unwrap_err()
                .to_string(),
            "mailbox_lease_rejected"
        );
        let mut expired = page.clone();
        expired.commands[0].lease.expires_at = now;
        assert!(validate_lease_page(&expired, 2, "mac-agent-test", now).is_err());
        let mut too_long = page.clone();
        too_long.commands[0].lease.expires_at = now + MAX_LEASE_MILLIS + 1;
        assert!(validate_lease_page(&too_long, 2, "mac-agent-test", now).is_err());
        let mut bad_attempt = page.clone();
        bad_attempt.commands[0].lease.attempt = 0;
        assert!(validate_lease_page(&bad_attempt, 2, "mac-agent-test", now).is_err());

        let fence = MailboxFence {
            receipt: page.commands[0].receipt.clone(),
            lease_token: page.commands[0].lease.token.clone(),
        };
        let fence_bytes = serde_json::to_vec(&fence).unwrap();
        assert_eq!(parse_fence(&fence_bytes).unwrap(), fence);
        let mut bad_fence = fence.clone();
        bad_fence.lease_token.push('=');
        assert!(validate_fence(&bad_fence).is_err());
        let mut unknown_fence: Value = serde_json::from_slice(&fence_bytes).unwrap();
        unknown_fence
            .as_object_mut()
            .unwrap()
            .insert("unknown".to_owned(), Value::Bool(true));
        assert!(parse_fence(&serde_json::to_vec(&unknown_fence).unwrap()).is_err());

        let response =
            parse_mailbox_response(text(&value, "ack_response_utf8").as_bytes()).unwrap();
        let expected = response.result.clone().expect("ack result");
        validate_mailbox_response(&response, &response.receipt, "published", Some(&expected))
            .unwrap();
        let mut wrong_revision = response.clone();
        wrong_revision.result.as_mut().unwrap().revision_id = "revision_other".into();
        assert!(validate_mailbox_response(
            &wrong_revision,
            &response.receipt,
            "published",
            Some(&expected),
        )
        .is_err());
    }

    #[test]
    fn dormancy_preserves_agent_shape_and_has_no_runtime_caller() {
        let config = crate::config::AgentConfig::standard(Path::new("/synthetic-home"));
        let value = serde_json::to_value(config).expect("default config serializes");
        let keys: BTreeSet<&str> = value
            .as_object()
            .expect("config object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            BTreeSet::from(["database", "library", "socket", "spool"])
        );
        assert!(!value.as_object().unwrap().contains_key("relay"));
        assert!(!value.as_object().unwrap().contains_key("mailbox"));

        const AGENT_MANIFEST: &str = include_str!("../Cargo.toml");
        const CONFIG_SOURCE: &str = include_str!("config.rs");
        const COORDINATOR_SOURCE: &str = include_str!("coordinator.rs");
        const FETCH_SOURCE: &str = include_str!("fetch.rs");
        const IPC_SOURCE: &str = include_str!("ipc.rs");
        const QUEUE_SOURCE: &str = include_str!("queue.rs");
        assert!(!AGENT_MANIFEST.contains("memex-surface"));
        assert!(!AGENT_MANIFEST.contains("relay"));
        for source in [
            CONFIG_SOURCE,
            COORDINATOR_SOURCE,
            FETCH_SOURCE,
            IPC_SOURCE,
            QUEUE_SOURCE,
        ] {
            assert!(!source.contains("mailbox"));
        }
        assert_eq!(
            include_str!("lib.rs").matches("pub mod mailbox;").count(),
            1
        );
    }
}
