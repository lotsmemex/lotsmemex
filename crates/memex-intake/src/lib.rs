use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use chrono::DateTime;
use memex_capture::{Comments, CommentsState, MAX_COMMENTS_BYTES};
use memex_core::{
    image_signature_matches, new_id, BlobDescriptor, CaptureEnvelope, ContentRights, Creator,
    CreatorRole, DraftAttachmentRef, IdentifierKind, ItemIdentifier, ItemMetadata, MetadataOrigin,
    MetadataProvenance, RightsStatus, Sensitivity, SourceProfile, Sourced, VerifiedBlob,
    VerifiedCapture,
};
use lots_scout_extract::WeixinArticleExtractor;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::{BufRead, Read, Write},
    net::Ipv4Addr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use subtle::ConstantTimeEq;
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};
use url::Url;

pub mod url_jobs;
pub mod web;
use url_jobs::{UrlJobBackend, UrlJobError};

pub const ADAPTER_ID: &str = "memex.loopback-intake";
pub const ADAPTER_VERSION: &str = "1";
pub const SOURCE_ID: &str = "source_loopback_web_capture_v1";
pub const CAPTURE_METHOD: &str = "browser_snapshot_v1";
const PATH: &str = "/api/v1/captures";
const MAX_REQUEST: u64 = 36 * 1024 * 1024;
const MAX_MANIFEST: u64 = 4 * 1024 * 1024;
const MAX_HTML: usize = 8 * 1024 * 1024;
const MAX_IMAGES: usize = 32;
const MAX_IMAGE: usize = 2 * 1024 * 1024;
const MAX_IMAGE_BASE64: usize = 2_796_204;
const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;
const MAX_NONCES: usize = 65_536;
pub const CAPTURE_CONTENT_TYPE_V1: &str = "application/vnd.memex.capture-v1";
pub const CAPTURE_MAGIC_V1: &[u8; 8] = b"MEMEX\0\0\x01";

#[derive(Debug, Serialize)]
pub struct StartupReceipt {
    pub schema_version: u32,
    pub url: String,
    pub loopback_only: bool,
    pub write_capable: bool,
    pub nonce_scope: &'static str,
}

#[derive(Debug, Serialize)]
struct SuccessReceipt {
    schema_version: u32,
    status: &'static str,
    capture_id: String,
    item_id: String,
    revision_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    comments: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    quality_check: Option<serde_json::Value>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommentsDto<T> {
    state: CommentsState,
    html: Option<T>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureDto {
    schema_version: u32,
    external_capture_id: String,
    captured_at: String,
    source_url: String,
    canonical_url: Option<String>,
    title: Option<String>,
    sensitivity: Sensitivity,
    #[serde(default)]
    metadata: MetadataDto,
    #[serde(default, deserialize_with = "deserialize_remote_attachments")]
    attachments: Vec<RemoteAttachmentDto>,
    #[serde(default, deserialize_with = "deserialize_images")]
    images: Vec<ImageDto>,
    comments: Option<CommentsDto<HtmlDto>>,
    quality_check: Option<url_jobs::QualityMode>,
    web: Option<web::WebOptions>,
    html: HtmlDto,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureManifestDto {
    schema_version: u32,
    external_capture_id: String,
    captured_at: String,
    source_url: String,
    canonical_url: Option<String>,
    title: Option<String>,
    sensitivity: Sensitivity,
    #[serde(default)]
    metadata: MetadataDto,
    #[serde(default, deserialize_with = "deserialize_remote_attachments")]
    attachments: Vec<RemoteAttachmentDto>,
    #[serde(default, deserialize_with = "deserialize_image_manifests")]
    images: Vec<ImageManifestDto>,
    comments: Option<CommentsDto<HtmlManifestDto>>,
    quality_check: Option<url_jobs::QualityMode>,
    web: Option<web::WebOptions>,
    html: HtmlManifestDto,
}

#[derive(Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct MetadataDto {
    #[serde(default, deserialize_with = "deserialize_creators")]
    creators: Option<Vec<CreatorDto>>,
    published_date: Option<String>,
    language: Option<String>,
    site_name: Option<String>,
    #[serde(rename = "abstract")]
    abstract_text: Option<String>,
    #[serde(default, deserialize_with = "deserialize_identifiers")]
    identifiers: Option<Vec<IdentifierDto>>,
    #[serde(default, deserialize_with = "deserialize_tags")]
    manual_tags: Option<Vec<String>>,
    content_rights: Option<RightsDto>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreatorDto {
    display_name: String,
    role: CreatorRole,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentifierDto {
    kind: IdentifierKind,
    value: String,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RightsDto {
    status: RightsStatus,
    statement: Option<String>,
    license_url: Option<String>,
}
#[derive(Clone, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
struct RemoteAttachmentDto {
    url: String,
    media_type: Option<String>,
    title: Option<String>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct HtmlDto {
    media_type: String,
    byte_size: u64,
    sha256: String,
    body: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HtmlManifestDto {
    media_type: String,
    byte_size: u64,
    sha256: String,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImageDto {
    source_url: String,
    media_type: String,
    byte_size: u64,
    sha256: String,
    body_base64: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImageManifestDto {
    source_url: String,
    media_type: String,
    byte_size: u64,
    sha256: String,
}

struct CaptureInput {
    schema_version: u32,
    external_capture_id: String,
    captured_at: String,
    source_url: String,
    canonical_url: Option<String>,
    title: Option<String>,
    sensitivity: Sensitivity,
    metadata: MetadataDto,
    attachments: Vec<RemoteAttachmentDto>,
    images: Vec<ImageInput>,
    comments: Option<CommentsDto<HtmlInput>>,
    quality_check: Option<url_jobs::QualityMode>,
    web: Option<web::WebOptions>,
    html: HtmlInput,
}

struct HtmlInput {
    media_type: String,
    byte_size: u64,
    sha256: String,
    bytes: Vec<u8>,
}

struct ImageInput {
    source_url: String,
    media_type: String,
    byte_size: u64,
    sha256: String,
    bytes: Vec<u8>,
}

fn deserialize_bounded_vec<'de, D, T, const LIMIT: usize>(
    deserializer: D,
) -> std::result::Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct BoundedVecVisitor<T, const LIMIT: usize>(std::marker::PhantomData<T>);
    impl<'de, T, const LIMIT: usize> serde::de::Visitor<'de> for BoundedVecVisitor<T, LIMIT>
    where
        T: Deserialize<'de>,
    {
        type Value = Vec<T>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(formatter, "a sequence with at most {LIMIT} entries")
        }

        fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
        where
            A: serde::de::SeqAccess<'de>,
        {
            let mut output = Vec::with_capacity(sequence.size_hint().unwrap_or(0).min(LIMIT));
            while let Some(value) = sequence.next_element()? {
                if output.len() == LIMIT {
                    return Err(serde::de::Error::custom("sequence exceeds entry limit"));
                }
                output.push(value);
            }
            Ok(output)
        }
    }

    deserializer.deserialize_seq(BoundedVecVisitor::<T, LIMIT>(std::marker::PhantomData))
}

fn deserialize_optional_bounded_vec<'de, D, T, const LIMIT: usize>(
    deserializer: D,
) -> std::result::Result<Option<Vec<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct OptionalBoundedVecVisitor<T, const LIMIT: usize>(std::marker::PhantomData<T>);
    impl<'de, T, const LIMIT: usize> serde::de::Visitor<'de> for OptionalBoundedVecVisitor<T, LIMIT>
    where
        T: Deserialize<'de>,
    {
        type Value = Option<Vec<T>>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(formatter, "null or a sequence with at most {LIMIT} entries")
        }

        fn visit_none<E>(self) -> std::result::Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(None)
        }

        fn visit_unit<E>(self) -> std::result::Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(None)
        }

        fn visit_some<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            deserialize_bounded_vec::<D, T, LIMIT>(deserializer).map(Some)
        }
    }

    deserializer.deserialize_option(OptionalBoundedVecVisitor::<T, LIMIT>(
        std::marker::PhantomData,
    ))
}

fn deserialize_creators<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Vec<CreatorDto>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_optional_bounded_vec::<D, CreatorDto, 64>(deserializer)
}

fn deserialize_identifiers<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Vec<IdentifierDto>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_optional_bounded_vec::<D, IdentifierDto, 64>(deserializer)
}

fn deserialize_tags<'de, D>(deserializer: D) -> std::result::Result<Option<Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_optional_bounded_vec::<D, String, 256>(deserializer)
}

fn deserialize_remote_attachments<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<RemoteAttachmentDto>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_bounded_vec::<D, RemoteAttachmentDto, 128>(deserializer)
}

fn deserialize_images<'de, D>(deserializer: D) -> std::result::Result<Vec<ImageDto>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_bounded_vec::<D, ImageDto, MAX_IMAGES>(deserializer)
}

fn deserialize_image_manifests<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<ImageManifestDto>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_bounded_vec::<D, ImageManifestDto, MAX_IMAGES>(deserializer)
}

pub struct IntakeServer {
    server: Server,
    library: PathBuf,
    snapshots: PathBuf,
    secret: [u8; 64],
    origin: String,
    port: u16,
    nonces: Mutex<HashSet<String>>,
    url_jobs: Option<Box<dyn UrlJobBackend>>,
    snapshots_enabled: bool,
    quality_model: Option<Arc<dyn memex_analysis::capture_quality::CaptureQualityModel>>,
}

#[cfg(not(unix))]
pub fn read_pairing_secret(path: impl AsRef<Path>) -> Result<[u8; 64]> {
    let _ = path.as_ref();
    bail!("intake is not supported on this platform");
}

#[cfg(unix)]
pub fn read_pairing_secret(path: impl AsRef<Path>) -> Result<[u8; 64]> {
    use anyhow::Context as _;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    let path = path.as_ref();
    let mut options = OpenOptions::new();
    options.read(true);
    options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    let file = options.open(path).context("invalid pairing secret")?;
    let meta = file.metadata().context("invalid pairing secret")?;
    if !meta.is_file() || meta.len() > 65 {
        bail!("invalid pairing secret");
    }
    if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        bail!("invalid pairing secret");
    }
    let mut bytes = Vec::with_capacity(66);
    file.take(66).read_to_end(&mut bytes)?;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    if bytes.len() != 64
        || !bytes
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
    {
        bail!("invalid pairing secret");
    }
    Ok(bytes.try_into().expect("checked length"))
}

impl IntakeServer {
    #[cfg(not(unix))]
    pub fn bind(
        library: impl AsRef<Path>,
        snapshots: impl AsRef<Path>,
        secret: [u8; 64],
        origin: String,
        port: u16,
    ) -> Result<Self> {
        let _ = (library.as_ref(), snapshots.as_ref(), secret, origin, port);
        bail!("intake is not supported on this platform");
    }

    #[cfg(unix)]
    pub fn bind(
        library: impl AsRef<Path>,
        snapshots: impl AsRef<Path>,
        secret: [u8; 64],
        origin: String,
        port: u16,
    ) -> Result<Self> {
        validate_origin(&origin)?;
        let library = safe_root(library.as_ref(), false)?;
        let snapshots = safe_root(snapshots.as_ref(), true)?;
        if library.starts_with(&snapshots) || snapshots.starts_with(&library) {
            bail!("storage roots overlap");
        }
        let server = Server::http((Ipv4Addr::LOCALHOST, port))
            .map_err(|_| anyhow::anyhow!("failed to bind intake"))?;
        let port = match server.server_addr() {
            tiny_http::ListenAddr::IP(addr) => addr.port(),
            _ => bail!("intake did not bind TCP"),
        };
        Ok(Self {
            server,
            library,
            snapshots,
            secret,
            origin,
            port,
            nonces: Mutex::new(HashSet::new()),
            url_jobs: None,
            snapshots_enabled: true,
            quality_model: None,
        })
    }

    pub fn with_url_jobs(mut self, backend: impl UrlJobBackend + 'static) -> Self {
        self.url_jobs = Some(Box::new(backend));
        self
    }

    /// A limited URL trial must not also expose an unrestricted snapshot writer.
    pub fn without_snapshots(mut self) -> Self {
        self.snapshots_enabled = false;
        self
    }

    pub fn with_quality_model(
        mut self,
        model: Arc<dyn memex_analysis::capture_quality::CaptureQualityModel>,
    ) -> Self {
        self.quality_model = Some(model);
        self
    }

    pub fn startup_receipt(&self) -> StartupReceipt {
        StartupReceipt {
            schema_version: 1,
            url: format!("http://127.0.0.1:{}{PATH}", self.port),
            loopback_only: true,
            write_capable: true,
            nonce_scope: "process_lifetime_no_eviction",
        }
    }

    pub fn serve(self) -> Result<()> {
        for request in self.server.incoming_requests() {
            self.handle(request);
        }
        Ok(())
    }

    fn handle(&self, mut request: Request) {
        let origin = singleton(&request, "Origin")
            .ok()
            .flatten()
            .map(str::to_owned);
        let result = self.process(&mut request);
        let (code, body) = match result {
            Ok((204, _)) => (204, Vec::new()),
            Ok((code, receipt)) => (code, serde_json::to_vec(&receipt).unwrap_or_default()),
            Err(error) => (
                error.status,
                serde_json::to_vec(&serde_json::json!({"schema_version":1,"error":error.code}))
                    .unwrap(),
            ),
        };
        let mut response = Response::from_data(body).with_status_code(StatusCode(code));
        if code != 204 {
            response.add_header(header("Content-Type", "application/json"));
        }
        response.add_header(header("Cache-Control", "no-store"));
        response.add_header(header("X-Content-Type-Options", "nosniff"));
        response.add_header(header("Connection", "close"));
        response.add_header(header("Vary", "Origin"));
        if origin.as_deref() == Some(self.origin.as_str()) {
            response.add_header(header("Access-Control-Allow-Origin", &self.origin));
            if request.method() == &Method::Options && code == 204 {
                response.add_header(header("Access-Control-Allow-Methods", "POST"));
                response.add_header(header(
                    "Access-Control-Allow-Headers",
                    "content-type,x-memex-intake-secret,x-memex-intake-nonce",
                ));
            }
        }
        let _ = request.respond(response);
    }

    fn process(
        &self,
        request: &mut Request,
    ) -> std::result::Result<(u16, serde_json::Value), HttpError> {
        let path = request.url().to_owned();
        if path == PATH && !self.snapshots_enabled {
            return Err(HttpError::new(404));
        }
        if !matches!(
            path.as_str(),
            PATH | "/api/v1/capabilities" | "/api/v1/groups"
                | "/api/v1/active-group"
                | "/api/v1/url-jobs/enqueue"
                | "/api/v1/url-jobs/status"
                | "/api/v1/url-jobs/retry"
                | "/api/v1/url-jobs/cancel"
        ) {
            return Err(HttpError::new(404));
        }
        self.common_headers(request)?;
        if request.method() == &Method::Options {
            for name in [
                "Transfer-Encoding",
                "Upgrade",
                "Expect",
                "Content-Encoding",
                "Trailer",
                "Cookie",
                "Authorization",
                "X-MemeX-Intake-Secret",
                "X-MemeX-Intake-Nonce",
            ] {
                if count(request, name) != 0 {
                    return Err(HttpError::new(400));
                }
            }
            if singleton(request, "Content-Length")
                .map_err(|_| HttpError::new(400))?
                .is_some_and(|length| length != "0")
            {
                return Err(HttpError::new(400));
            }
            let method = required(request, "Access-Control-Request-Method")?;
            if method != "POST" {
                return Err(HttpError::new(400));
            }
            let requested = required(request, "Access-Control-Request-Headers")?;
            let allowed = [
                "content-type",
                "x-memex-intake-secret",
                "x-memex-intake-nonce",
            ];
            let mut seen = HashSet::new();
            for value in requested.split(',').map(|v| v.trim().to_ascii_lowercase()) {
                if value.is_empty() || !allowed.contains(&value.as_str()) || !seen.insert(value) {
                    return Err(HttpError::new(400));
                }
            }
            if seen.is_empty() {
                return Err(HttpError::new(400));
            }
            return Ok((204, serde_json::Value::Null));
        }
        if request.method() != &Method::Post {
            return Err(HttpError::new(405));
        }
        for name in [
            "Transfer-Encoding",
            "Upgrade",
            "Expect",
            "Content-Encoding",
            "Trailer",
            "Cookie",
            "Authorization",
        ] {
            if count(request, name) != 0 {
                return Err(HttpError::new(400));
            }
        }
        let json_transport = match required(request, "Content-Type")? {
            "application/json" => true,
            CAPTURE_CONTENT_TYPE_V1 => false,
            _ => return Err(HttpError::new(415)),
        };
        let length = required(request, "Content-Length")?;
        if length.is_empty()
            || (length.len() > 1 && length.starts_with('0'))
            || !length.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(HttpError::new(400));
        }
        let length: u64 = length.parse().map_err(|_| HttpError::new(400))?;
        if length > MAX_REQUEST {
            return Err(HttpError::new(413));
        }
        if request.body_length() != usize::try_from(length).ok() {
            return Err(HttpError::new(400));
        }
        let supplied = required(request, "X-MemeX-Intake-Secret")?;
        if !lower_hex_64(supplied) || supplied.as_bytes().ct_eq(&self.secret).unwrap_u8() != 1 {
            return Err(HttpError::new(401));
        }
        let nonce = required(request, "X-MemeX-Intake-Nonce")?.to_owned();
        if !lower_hex_64(&nonce) {
            return Err(HttpError::new(400));
        }
        if path != PATH && (!json_transport || length > 16 * 1024) {
            return Err(HttpError::new(400));
        }
        let body = request.as_reader().take(length);
        let mut body = std::io::BufReader::with_capacity(64 * 1024, body);
        if path != PATH {
            let value: serde_json::Value =
                serde_json::from_reader(&mut body).map_err(|_| HttpError::new(400))?;
            if body.get_ref().limit() != 0 {
                return Err(HttpError::new(400));
            }
            self.consume_nonce(nonce)?;
            if path == "/api/v1/capabilities" {
                if value != serde_json::json!({"schema_version":1}) {
                    return Err(HttpError::new(400));
                }
                let mut features = vec!["comments-v1", "quality-check-v1", "web-snapshot-v1"];
                if let Some(backend) = &self.url_jobs {
                    features.push("url-jobs-v1");
                    features.extend_from_slice(backend.features());
                }
                return Ok((
                    200,
                    serde_json::json!({"schema_version":1,"features":features,
                    "comments_max_bytes":MAX_COMMENTS_BYTES}),
                ));
            }
            let command = url_jobs::parse(&path, value).map_err(HttpError::from)?;
            let backend = self
                .url_jobs
                .as_ref()
                .ok_or_else(|| HttpError::from(UrlJobError::Unavailable))?;
            let mut identity = Sha256::new();
            identity.update(b"memex-extension-registration-v1\0");
            identity.update(self.secret);
            identity.update(b"\0");
            identity.update(self.origin.as_bytes());
            let registration = hex::encode(identity.finalize());
            return backend
                .execute(&registration, command)
                .map(|value| (200, value))
                .map_err(HttpError::from);
        }
        let prepared = if json_transport {
            let dto: CaptureDto =
                serde_json::from_reader(&mut body).map_err(|_| HttpError::new(400))?;
            prepare(dto).map_err(|_| HttpError::new(400))?
        } else {
            prepare_binary(&mut body, length).map_err(|_| HttpError::new(400))?
        };
        if body.get_ref().limit() != 0 {
            return Err(HttpError::new(400));
        }
        self.consume_nonce(nonce)?;
        let (capture, metadata, attachments, _digest, comments, quality_mode, web) = prepared;
        let comments_receipt = comments
            .as_ref()
            .map(|comments| serde_json::json!({"state":comments.state,"reason":null}))
            .or_else(|| {
                quality_mode.map(|_| serde_json::json!({"state":"not_requested","reason":null}))
            });
        let mut prepared = memex_capture::prepare_capture(
            memex_capture::CaptureSubmission {
                capture,
                metadata,
                attachments,
            },
            memex_capture::AdmissionContext::default(),
        )
        .map_err(|_| HttpError::new(422))?;
        if let Some(comments) = comments {
            prepared = prepared
                .with_comments(comments, "memex-intake-v1")
                .map_err(|_| HttpError::new(400))?;
        }
        for blob in prepared.source_blobs() {
            install_snapshot(&self.snapshots, &blob.descriptor.sha256, &blob.bytes)
                .map_err(|_| HttpError::new(500))?;
        }
        if let Some(options) = web {
            let receipt = memex_capture::commit_web_capture(&self.library, prepared, options.target_group_id.as_deref())
                .map_err(|error| web::http_error(&error))?;
            return Ok((200, serde_json::json!({"schema_version":1,"status":if receipt.reused {"reused"} else {"captured"},
                "item_id":receipt.item_id,"revision_id":receipt.revision_id,"target_group_id":receipt.target_group_id,"filed":receipt.filed})));
        }
        // Omission is off, not an escape from durable request identity validation.
        let committed = memex_capture::commit_capture_checked(
            &self.library,
            prepared,
            quality_mode == Some(url_jobs::QualityMode::Configured),
            self.quality_model.as_deref(),
        )
        .map(|(receipt, quality)| (receipt, quality_mode.map(|_| quality)));
        let (receipt, quality_receipt) = committed.map_err(|error| {
            if error.is::<memex_store::IngestConflict>()
                || error.to_string() == "request_identity_conflict"
            {
                HttpError::from(UrlJobError::IdentityConflict)
            } else {
                HttpError::new(500)
            }
        })?;
        // The Store commit is already durable. Do not return an ambiguous capture failure (and
        // invite a duplicate toolbar capture) solely because the background analysis queue is
        // temporarily unavailable. The daemon's bounded Store reconciler repairs a missing job;
        // until then its projection remains truthfully not_requested.
        let _ = memex_analysis::enqueue_ingest_translations(&self.library, &receipt);
        let _ = memex_analysis::enqueue_ingest_summaries(&self.library, &receipt);
        let status = if receipt.idempotent_replay {
            "replayed"
        } else {
            "created"
        };
        let code = if receipt.idempotent_replay { 200 } else { 201 };
        Ok((
            code,
            serde_json::to_value(SuccessReceipt {
                schema_version: 1,
                status,
                capture_id: receipt.capture_id,
                item_id: receipt.item_ids[0].clone(),
                revision_id: receipt.revision_ids[0].clone(),
                quality_check: quality_receipt.or_else(|| {
                    comments_receipt.as_ref().map(|_| {
                        serde_json::json!({
                            "state":"not_requested", "reason":null, "analysis_id":null,
                        })
                    })
                }),
                comments: comments_receipt,
            })
            .map_err(|_| HttpError::new(500))?,
        ))
    }

    fn consume_nonce(&self, nonce: String) -> std::result::Result<(), HttpError> {
        let mut nonces = self.nonces.lock().map_err(|_| HttpError::new(503))?;
        if nonces.contains(&nonce) {
            return Err(HttpError::new(409));
        }
        if nonces.len() >= MAX_NONCES {
            return Err(HttpError::new(503));
        }
        nonces.insert(nonce);
        Ok(())
    }

    fn common_headers(&self, request: &Request) -> std::result::Result<(), HttpError> {
        let host = required(request, "Host")?;
        if host != format!("127.0.0.1:{}", self.port) && host != format!("localhost:{}", self.port)
        {
            return Err(HttpError::new(400));
        }
        if required(request, "Origin")? != self.origin {
            return Err(HttpError::new(403));
        }
        Ok(())
    }
}

struct HttpError {
    status: u16,
    code: &'static str,
}
impl HttpError {
    fn new(status: u16) -> Self {
        Self {
            status,
            code: "request_rejected",
        }
    }
}
impl From<UrlJobError> for HttpError {
    fn from(error: UrlJobError) -> Self {
        Self {
            status: error.status(),
            code: error.code(),
        }
    }
}
fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("valid header")
}
fn count(r: &Request, name: &'static str) -> usize {
    r.headers().iter().filter(|h| h.field.equiv(name)).count()
}
fn singleton<'a>(r: &'a Request, name: &'static str) -> Result<Option<&'a str>> {
    let mut values = r.headers().iter().filter(|h| h.field.equiv(name));
    let first = values.next().map(|h| h.value.as_str());
    if values.next().is_some() {
        bail!("duplicate header");
    }
    Ok(first)
}
fn required<'a>(r: &'a Request, name: &'static str) -> std::result::Result<&'a str, HttpError> {
    singleton(r, name)
        .map_err(|_| HttpError::new(400))?
        .ok_or_else(|| HttpError::new(400))
}
fn lower_hex_64(v: &str) -> bool {
    v.len() == 64
        && v.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn validate_origin(v: &str) -> Result<()> {
    let url = Url::parse(v)?;
    if !matches!(
        url.scheme(),
        "http" | "https" | "chrome-extension" | "moz-extension"
    ) || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || !matches!(url.path(), "" | "/")
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("invalid origin");
    }
    Ok(())
}
fn safe_root(path: &Path, owner_only: bool) -> Result<PathBuf> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        bail!("unsafe storage root");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if owner_only && (meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0) {
            bail!("unsafe storage root");
        }
    }
    Ok(path.canonicalize()?)
}
fn clean(value: String, max: usize) -> Result<String> {
    let v = value.trim();
    if v.is_empty() || v.len() > max || v.chars().any(char::is_control) {
        bail!("invalid text");
    }
    Ok(v.to_owned())
}
fn optional(value: Option<String>, max: usize) -> Result<Option<String>> {
    value.map(|v| clean(v, max)).transpose()
}
fn web_url(value: String) -> Result<String> {
    let value = clean(value, 4096)?;
    let url = Url::parse(&value)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        bail!("invalid URL");
    }
    Ok(value)
}
fn image_url(value: String) -> Result<String> {
    let value = web_url(value)?;
    let url = Url::parse(&value)?;
    if url.scheme() != "https"
        || !matches!(
            url.host_str(),
            Some("mmbiz.qpic.cn") | Some("mp.weixin.qq.com")
        )
        || url.port().is_some()
        || url.path().is_empty()
        || url.fragment().is_some()
    {
        bail!("invalid image URL");
    }
    Ok(url.to_string())
}
fn sourced<T>(value: T) -> Sourced<T> {
    Sourced {
        value,
        provenance: MetadataProvenance {
            origin: MetadataOrigin::IntakeProvided,
            provider_id: ADAPTER_ID.into(),
            provider_version: ADAPTER_VERSION.into(),
            source_refs: Vec::new(),
        },
    }
}

type Prepared = (
    VerifiedCapture,
    ItemMetadata,
    Vec<DraftAttachmentRef>,
    String,
    Option<Comments>,
    Option<url_jobs::QualityMode>,
    Option<web::WebOptions>,
);
fn prepare(dto: CaptureDto) -> Result<Prepared> {
    let CaptureDto {
        schema_version,
        external_capture_id,
        captured_at,
        source_url,
        canonical_url,
        title,
        sensitivity,
        metadata,
        attachments,
        images,
        comments,
        quality_check,
        web,
        html,
    } = dto;
    let image_inputs = decode_json_images(images, MAX_IMAGE_BYTES)?;
    prepare_input(CaptureInput {
        schema_version,
        external_capture_id,
        captured_at,
        source_url,
        canonical_url,
        title,
        sensitivity,
        metadata,
        attachments,
        images: image_inputs,
        quality_check,
        web,
        comments: comments.map(|comments| CommentsDto {
            state: comments.state,
            html: comments.html.map(|html| HtmlInput {
                media_type: html.media_type,
                byte_size: html.byte_size,
                sha256: html.sha256,
                bytes: html.body.into_bytes(),
            }),
        }),
        html: HtmlInput {
            media_type: html.media_type,
            byte_size: html.byte_size,
            sha256: html.sha256,
            bytes: html.body.into_bytes(),
        },
    })
}

fn decode_json_images(images: Vec<ImageDto>, aggregate_limit: usize) -> Result<Vec<ImageInput>> {
    let mut image_inputs = Vec::with_capacity(images.len());
    let mut aggregate_bytes = 0usize;
    for image in images {
        if image.body_base64.len() > MAX_IMAGE_BASE64 {
            bail!("invalid captured image");
        }
        let bytes = BASE64
            .decode(&image.body_base64)
            .map_err(|_| anyhow::anyhow!("invalid captured image"))?;
        if bytes.len() > MAX_IMAGE {
            bail!("invalid captured image");
        }
        aggregate_bytes = aggregate_bytes
            .checked_add(bytes.len())
            .context("captured image budget overflow")?;
        if aggregate_bytes > aggregate_limit {
            bail!("captured images exceed aggregate size limit");
        }
        if BASE64.encode(&bytes) != image.body_base64 {
            bail!("invalid captured image");
        }
        image_inputs.push(ImageInput {
            source_url: image.source_url,
            media_type: image.media_type,
            byte_size: image.byte_size,
            sha256: image.sha256,
            bytes,
        });
    }
    Ok(image_inputs)
}

fn prepare_binary<R: BufRead>(reader: &mut R, outer_length: u64) -> Result<Prepared> {
    let mut magic = [0u8; 8];
    reader.read_exact(&mut magic)?;
    if &magic != CAPTURE_MAGIC_V1 {
        bail!("invalid binary capture magic");
    }
    let mut manifest_length = [0u8; 4];
    reader.read_exact(&mut manifest_length)?;
    let manifest_length = u32::from_be_bytes(manifest_length) as u64;
    if manifest_length == 0 || manifest_length > MAX_MANIFEST {
        bail!("invalid binary capture manifest length");
    }
    let mut manifest_reader = (&mut *reader).take(manifest_length);
    let manifest: CaptureManifestDto = serde_json::from_reader(&mut manifest_reader)?;
    if manifest_reader.limit() != 0 {
        bail!("binary capture manifest length mismatch");
    }
    if manifest.schema_version != 1
        || manifest.html.byte_size > MAX_HTML as u64
        || !lower_hex_64(&manifest.html.sha256)
        || !matches!(
            manifest.html.media_type.as_str(),
            "text/html" | "application/xhtml+xml"
        )
    {
        bail!("invalid binary capture manifest");
    }
    let mut image_bytes = 0u64;
    for image in &manifest.images {
        if image.byte_size > MAX_IMAGE as u64
            || !lower_hex_64(&image.sha256)
            || !matches!(
                image.media_type.as_str(),
                "image/gif" | "image/jpeg" | "image/png" | "image/webp"
            )
        {
            bail!("invalid binary capture image manifest");
        }
        image_bytes = image_bytes
            .checked_add(image.byte_size)
            .context("binary capture image budget overflow")?;
    }
    if image_bytes > MAX_IMAGE_BYTES as u64 {
        bail!("binary capture images exceed aggregate size limit");
    }
    let comments_bytes = manifest
        .comments
        .as_ref()
        .and_then(|c| c.html.as_ref())
        .map_or(0, |html| html.byte_size);
    if comments_bytes > MAX_COMMENTS_BYTES as u64 {
        bail!("comments exceed size limit");
    }
    let expected_length = 12u64
        .checked_add(manifest_length)
        .and_then(|length| length.checked_add(manifest.html.byte_size))
        .and_then(|length| length.checked_add(image_bytes))
        .and_then(|length| length.checked_add(comments_bytes))
        .context("binary capture length overflow")?;
    if expected_length != outer_length {
        bail!("binary capture length mismatch");
    }

    let CaptureManifestDto {
        schema_version,
        external_capture_id,
        captured_at,
        source_url,
        canonical_url,
        title,
        sensitivity,
        metadata,
        attachments,
        images,
        comments,
        quality_check,
        web,
        html,
    } = manifest;
    let html_bytes = read_exact_bytes(reader, html.byte_size)?;
    let mut image_inputs = Vec::with_capacity(images.len());
    for image in images {
        let bytes = read_exact_bytes(reader, image.byte_size)?;
        image_inputs.push(ImageInput {
            source_url: image.source_url,
            media_type: image.media_type,
            byte_size: image.byte_size,
            sha256: image.sha256,
            bytes,
        });
    }
    let comments = comments
        .map(|comments| -> Result<_> {
            Ok(CommentsDto {
                state: comments.state,
                html: comments
                    .html
                    .map(|html| -> Result<_> {
                        Ok(HtmlInput {
                            bytes: read_exact_bytes(reader, html.byte_size)?,
                            media_type: html.media_type,
                            byte_size: html.byte_size,
                            sha256: html.sha256,
                        })
                    })
                    .transpose()?,
            })
        })
        .transpose()?;
    prepare_input(CaptureInput {
        schema_version,
        external_capture_id,
        captured_at,
        source_url,
        canonical_url,
        title,
        sensitivity,
        metadata,
        attachments,
        images: image_inputs,
        comments,
        quality_check,
        web,
        html: HtmlInput {
            media_type: html.media_type,
            byte_size: html.byte_size,
            sha256: html.sha256,
            bytes: html_bytes,
        },
    })
}

fn read_exact_bytes(reader: &mut impl Read, length: u64) -> Result<Vec<u8>> {
    let length = usize::try_from(length).context("binary capture part is too large")?;
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn prepare_input(input: CaptureInput) -> Result<Prepared> {
    let CaptureInput {
        schema_version,
        external_capture_id,
        captured_at,
        source_url,
        canonical_url,
        title,
        sensitivity,
        metadata: m,
        attachments: remote_attachments,
        images,
        comments,
        quality_check,
        web,
        html,
    } = input;
    let comments = comments
        .map(|comments| -> Result<Comments> {
            let bytes = comments
                .html
                .map(|html| -> Result<Vec<u8>> {
                    if html.media_type != "text/html"
                        || html.byte_size != html.bytes.len() as u64
                        || html.bytes.len() > MAX_COMMENTS_BYTES
                        || !lower_hex_64(&html.sha256)
                        || html.sha256 != hex::encode(Sha256::digest(&html.bytes))
                    {
                        bail!("invalid comments artifact");
                    }
                    Ok(html.bytes)
                })
                .transpose()?;
            let comments = Comments {
                state: comments.state,
                html: bytes,
            };
            comments.validate()?;
            Ok(comments)
        })
        .transpose()?;
    if schema_version != 1
        || !lower_hex_64(&html.sha256)
        || html.bytes.len() > MAX_HTML
        || html.byte_size != html.bytes.len() as u64
        || !matches!(
            html.media_type.as_str(),
            "text/html" | "application/xhtml+xml"
        )
        || std::str::from_utf8(&html.bytes).is_err()
    {
        bail!("invalid capture");
    }
    let digest = hex::encode(Sha256::digest(&html.bytes));
    if digest != html.sha256 {
        bail!("invalid capture");
    }
    let captured_at = clean(captured_at, 4096)?;
    DateTime::parse_from_rfc3339(&captured_at)?;
    let external_id = clean(external_capture_id, 4096)?;
    let source_url = web_url(source_url)?;
    if web.is_some() && (WeixinArticleExtractor::supports_url(&source_url) || comments.is_some() || quality_check.is_some()) {
        bail!("web snapshot cannot override site-specific capture or request generation");
    }
    if let Some(target) = web.as_ref().and_then(|w| w.target_group_id.as_deref()) {
        if !target.starts_with("group_") || target.len() > 128 || !target.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            bail!("invalid web target");
        }
    }
    if !images.is_empty() && web.is_none() && !WeixinArticleExtractor::supports_url(&source_url) {
        bail!("captured images require a supported site translator");
    }
    let canonical_url = canonical_url.map(web_url).transpose()?;
    let title = optional(title, 4096)?;
    let creators = m
        .creators
        .map(|v| {
            if v.is_empty() || v.len() > 64 {
                bail!("invalid creators")
            }
            v.into_iter()
                .map(|c| Ok(Creator::new(clean(c.display_name, 4096)?, c.role)))
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?
        .map(sourced);
    let identifiers = m
        .identifiers
        .map(|v| {
            if v.is_empty() || v.len() > 64 {
                bail!("invalid identifiers")
            }
            let mut out = v
                .into_iter()
                .map(|i| Ok(ItemIdentifier::new(i.kind, clean(i.value, 4096)?)))
                .collect::<Result<Vec<_>>>()?;
            out.sort();
            out.dedup();
            Ok(out)
        })
        .transpose()?
        .map(sourced);
    let tags = m
        .manual_tags
        .map(|v| {
            if v.is_empty() || v.len() > 256 {
                bail!("invalid tags")
            }
            let mut out = v
                .into_iter()
                .map(|x| clean(x, 4096))
                .collect::<Result<Vec<_>>>()?;
            out.sort();
            out.dedup();
            Ok(out)
        })
        .transpose()?
        .map(sourced);
    let rights = m
        .content_rights
        .map(|r| {
            Ok::<_, anyhow::Error>(ContentRights {
                status: r.status,
                statement: optional(r.statement, 4096)?,
                license_url: r.license_url.map(web_url).transpose()?,
            })
        })
        .transpose()?
        .map(sourced);
    let metadata = ItemMetadata {
        creators,
        published_date: optional(m.published_date, 4096)?.map(sourced),
        language: optional(m.language, 4096)?.map(sourced),
        site_name: optional(m.site_name, 4096)?.map(sourced),
        abstract_text: optional(m.abstract_text, 65536)?.map(sourced),
        identifiers,
        manual_tags: tags,
        content_rights: rights,
        ..Default::default()
    };
    let mut remote = remote_attachments
        .into_iter()
        .map(|a| {
            Ok(RemoteAttachmentDto {
                url: web_url(a.url)?,
                media_type: optional(a.media_type, 4096)?,
                title: optional(a.title, 4096)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    if remote.len() > 128 {
        bail!("too many attachments");
    }
    remote.sort();
    remote.dedup();
    let attachments = remote
        .into_iter()
        .map(|a| DraftAttachmentRef::Remote {
            url: a.url,
            media_type: a.media_type,
            title: a.title,
        })
        .collect::<Vec<_>>();
    if images.len() > MAX_IMAGES {
        bail!("too many captured images");
    }
    let mut image_urls = HashSet::new();
    let mut image_digests = HashSet::new();
    let mut image_bytes = 0usize;
    let mut image_blobs = Vec::with_capacity(images.len());
    for (index, image) in images.into_iter().enumerate() {
        if !lower_hex_64(&image.sha256)
            || !matches!(
                image.media_type.as_str(),
                "image/gif" | "image/jpeg" | "image/png" | "image/webp"
            )
        {
            bail!("invalid captured image");
        }
        if image.bytes.len() > MAX_IMAGE
            || image.bytes.len() as u64 != image.byte_size
            || !image_signature_matches(&image.media_type, &image.bytes)
        {
            bail!("invalid captured image");
        }
        image_bytes = image_bytes
            .checked_add(image.bytes.len())
            .context("captured image budget overflow")?;
        if image_bytes > MAX_IMAGE_BYTES {
            bail!("captured images exceed aggregate size limit");
        }
        let source_url = if web.is_some() { web_url(image.source_url)? } else { image_url(image.source_url)? };
        let actual_digest = hex::encode(Sha256::digest(&image.bytes));
        if actual_digest != image.sha256
            || !image_urls.insert(source_url.clone())
            || (web.is_none() && !image_digests.insert(actual_digest.clone()))
        {
            bail!("invalid captured image");
        }
        image_blobs.push(VerifiedBlob {
            descriptor: BlobDescriptor {
                schema_version: 1,
                locator: format!("inline_image_{index}"),
                media_type: image.media_type,
                byte_size: image.bytes.len() as u64,
                sha256: actual_digest.clone(),
                source_url: Some(source_url),
            },
            blob_id: format!("sha256:{actual_digest}"),
            // Distinct URLs may share bytes. Every source occurrence keeps
            // durable evidence even when only one local attachment is needed.
            source_backend_ref: web.as_ref().map(|_| format!("memex-intake-v1:sha256:{actual_digest}")),
            bytes: image.bytes,
        });
    }
    let blob = VerifiedBlob {
        descriptor: BlobDescriptor {
            schema_version: 1,
            locator: "inline_html".into(),
            media_type: html.media_type,
            byte_size: html.byte_size,
            sha256: digest.clone(),
            source_url: None,
        },
        blob_id: format!("sha256:{digest}"),
        source_backend_ref: Some(format!("memex-intake-v1:sha256:{digest}")),
        bytes: html.bytes,
    };
    let mut blobs = Vec::with_capacity(image_blobs.len() + 1);
    blobs.push(blob);
    blobs.extend(image_blobs);
    let artifacts = blobs.iter().map(|blob| blob.descriptor.clone()).collect();
    let capture = VerifiedCapture {
        source: SourceProfile {
            schema_version: 1,
            source_id: SOURCE_ID.into(),
            name: "MemeX loopback web capture".into(),
            source_kind: "web".into(),
            canonical_uris: vec![],
            evidence_roles: vec!["primary".into()],
            default_sensitivity: Sensitivity::Public,
            preferred_extractor: None,
        },
        capture: CaptureEnvelope {
            schema_version: 1,
            adapter_id: ADAPTER_ID.into(),
            adapter_version: if web.is_some() { "1-web-snapshot-v1" } else { ADAPTER_VERSION }.into(),
            external_capture_id: external_id,
            source_id: SOURCE_ID.into(),
            captured_at,
            capture_method: CAPTURE_METHOD.into(),
            sensitivity,
            route_summary: None,
            title,
            source_url,
            canonical_url,
            artifacts,
        },
        blobs,
    };
    Ok((
        capture,
        metadata,
        attachments,
        digest,
        comments,
        quality_check,
        web,
    ))
}

fn install_snapshot(root: &Path, digest: &str, bytes: &[u8]) -> Result<()> {
    let target = root.join(format!("{digest}.html"));
    if target.exists() {
        return verify_snapshot(&target, digest, bytes);
    }
    let temp = root.join(format!(".{digest}.{}.tmp", new_id("snapshot")));
    let result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        match fs::hard_link(&temp, &target) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                verify_snapshot(&target, digest, bytes)
            }
            Err(e) => Err(e.into()),
        }
    })();
    let _ = fs::remove_file(&temp);
    result?;
    // The Store may reference these bytes immediately after installation.
    // Persist the directory entry as well as the file before committing it.
    fs::File::open(root)?.sync_all()?;
    Ok(())
}
fn verify_snapshot(path: &Path, digest: &str, expected: &[u8]) -> Result<()> {
    #[cfg(not(unix))]
    {
        let meta = fs::symlink_metadata(path)?;
        if meta.file_type().is_symlink() {
            bail!("unsafe snapshot");
        }
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.len() != expected.len() as u64 {
        bail!("unsafe snapshot");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
            bail!("unsafe snapshot");
        }
    }
    let mut bytes = Vec::with_capacity(expected.len() + 1);
    file.take(expected.len() as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes != expected || hex::encode(Sha256::digest(&bytes)) != digest {
        bail!("snapshot mismatch");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lots_scout_extract::Extractor;
    use serde_json::json;
    use std::{
        io::{Read, Write},
        net::TcpStream,
        thread,
        time::Duration,
    };

    fn roots() -> (tempfile::TempDir, tempfile::TempDir) {
        let library = tempfile::tempdir().unwrap();
        let snapshots = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(snapshots.path(), fs::Permissions::from_mode(0o700)).unwrap();
        }
        (library, snapshots)
    }
    fn payload(id: &str) -> Vec<u8> {
        let html = "<!doctype html><html><head><title>Synthetic</title></head><body><article><h1>Synthetic</h1><p>Useful synthetic text. This harmless public article contains the facts and context that the reader needs to understand the example. The text is deliberately deterministic for a local background translation queue test.</p></article></body></html>";
        let digest = hex::encode(Sha256::digest(html.as_bytes()));
        serde_json::to_vec(&json!({
            "schema_version":1, "external_capture_id":id,
            "captured_at":"2026-08-10T12:00:00Z", "source_url":"https://example.invalid/page",
            "canonical_url":"https://example.invalid/canonical", "title":" Synthetic ",
            "sensitivity":"internal",
            "metadata":{"creators":[{"display_name":" Author One ","role":"author"}],
                "published_date":"2026-08", "language":"zh-CN", "site_name":"Synthetic Journal",
                "abstract":"A synthetic abstract for typed metadata proof.",
                "identifiers":[{"kind":"doi","value":" 10.1/test "}], "manual_tags":["z","a","a"],
                "content_rights":{"status":"licensed","statement":"Synthetic license",
                    "license_url":"https://example.invalid/license"}},
            "attachments":[{"url":"https://example.invalid/file.pdf","media_type":"application/pdf","title":"Paper"}],
            "html":{"media_type":"text/html","byte_size":html.len(),"sha256":digest,"body":html}
        })).unwrap()
    }
    fn weixin_payload(id: &str) -> Vec<u8> {
        let mut image_html = String::new();
        let mut images = Vec::new();
        for index in 0..15u8 {
            let source_url = format!("https://mmbiz.qpic.cn/assets/diagram-{index}.png");
            let mut image = b"\x89PNG\r\n\x1a\n".to_vec();
            if index != 0 {
                image.push(index);
            }
            let image_digest = hex::encode(Sha256::digest(&image));
            image_html.push_str(&format!(
                r#"<img data-src="{source_url}" alt="资料图{index}">"#
            ));
            images.push(json!({
                "source_url":source_url, "media_type":"image/png",
                "byte_size":image.len(), "sha256":image_digest,
                "body_base64":BASE64.encode(&image)
            }));
        }
        let html = format!(
            r#"<!doctype html><html><head><meta property="og:title" content="Fallback"></head>
<body><h1 id="activity-name">微信资料演习</h1><nav>WEIXIN_NAV_SENTINEL</nav>
<section id="js_content"><p>这是微信公众号资料摄取演习的合成正文，供人类和 AI 阅读。</p>
{image_html}<script>WEIXIN_SCRIPT_SENTINEL</script></section>
</body></html>"#
        );
        let digest = hex::encode(Sha256::digest(html.as_bytes()));
        serde_json::to_vec(&json!({
            "schema_version":1, "external_capture_id":id,
            "captured_at":"2026-08-10T12:00:00Z",
            "source_url":"https://mp.weixin.qq.com/s/synthetic-article",
            "canonical_url":"https://mp.weixin.qq.com/s/synthetic-article",
            "title":null, "sensitivity":"internal",
            "images":images,
            "html":{"media_type":"text/html","byte_size":html.len(),"sha256":digest,"body":html}
        }))
        .unwrap()
    }
    fn binary_payload(json_payload: &[u8]) -> Vec<u8> {
        let mut manifest: serde_json::Value = serde_json::from_slice(json_payload).unwrap();
        let html = manifest["html"]
            .as_object_mut()
            .unwrap()
            .remove("body")
            .unwrap()
            .as_str()
            .unwrap()
            .as_bytes()
            .to_vec();
        let mut images = Vec::new();
        if let Some(entries) = manifest
            .get_mut("images")
            .and_then(|value| value.as_array_mut())
        {
            for image in entries {
                let body = image
                    .as_object_mut()
                    .unwrap()
                    .remove("body_base64")
                    .unwrap();
                images.push(BASE64.decode(body.as_str().unwrap()).unwrap());
            }
        }
        let comments = manifest
            .get_mut("comments")
            .and_then(|comments| comments.get_mut("html"))
            .and_then(|html| html.as_object_mut())
            .map(|html| {
                html.remove("body")
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .as_bytes()
                    .to_vec()
            })
            .unwrap_or_default();
        let manifest = serde_json::to_vec(&manifest).unwrap();
        let mut output = Vec::with_capacity(
            12 + manifest.len() + html.len() + images.iter().map(Vec::len).sum::<usize>(),
        );
        output.extend_from_slice(CAPTURE_MAGIC_V1);
        output.extend_from_slice(&(manifest.len() as u32).to_be_bytes());
        output.extend_from_slice(&manifest);
        output.extend_from_slice(&html);
        for image in images {
            output.extend_from_slice(&image);
        }
        output.extend_from_slice(&comments);
        output
    }

    #[test]
    fn generic_web_binary_preserves_local_poster_and_raw_date_without_weakening_weixin() {
        let (library, snapshots) = roots();
        let destination = library.path().join("dry-run-must-not-create");
        let image = b"\x89PNG\r\n\x1a\n";
        let html = format!(r#"<article data-memex-snapshot="1"><h2 id="demo">Demo</h2><p>{}</p><p><a href="https://example.invalid/page#demo"><img src="https://example.invalid/poster.png" alt="Demo poster">Watch demo</a></p><img src="https://example.invalid/poster.png?alias=1" alt="Alias"><table><tr><th>A</th><th>B</th></tr><tr><td>3</td><td>7</td></tr></table></article>"#, "Readable paragraph. ".repeat(20));
        let mut payload: serde_json::Value = serde_json::from_slice(&payload("web-binary-1")).unwrap();
        payload["canonical_url"] = serde_json::Value::Null;
        payload["web"] = json!({"target_group_id":null});
        payload["metadata"]["published_date"] = json!("2026-11-09");
        payload["html"] = json!({"media_type":"text/html","byte_size":html.len(),"sha256":hex::encode(Sha256::digest(html.as_bytes())),"body":html});
        payload["images"] = json!([{"source_url":"https://example.invalid/poster.png","media_type":"image/png","byte_size":image.len(),"sha256":hex::encode(Sha256::digest(image)),"body_base64":BASE64.encode(image)}]);
        let mut alias = payload["images"][0].clone();
        alias["source_url"] = json!("https://example.invalid/poster.png?alias=1");
        payload["images"].as_array_mut().unwrap().push(alias);
        let bytes = binary_payload(&serde_json::to_vec(&payload).unwrap());
        assert!(web::collect_binary(&destination, None, &bytes, true).unwrap()["dry_run"].as_bool().unwrap());
        assert!(!destination.exists());
        assert_eq!(fs::read_dir(snapshots.path()).unwrap().count(), 0);
        let receipt = web::collect_binary(library.path(), Some(snapshots.path()), &bytes, false).unwrap();
        assert_eq!(fs::read_dir(snapshots.path()).unwrap().count(), 2);
        let id = receipt["item_id"].as_str().unwrap();
        let (_, revision) = memex_store::read_item(library.path(), id).unwrap();
        assert!(revision.content_markdown.contains("attachments/"));
        assert_eq!(revision.content_markdown.matches("attachments/").count(), 2);
        assert!(revision.content_markdown.contains("https://example.invalid/page#demo"));
        assert!(revision.provenance.iter().any(|p| p.tool_id == "generic.webpage" && p.tool_version == "3-web-structure-v1"));
        assert_eq!(revision.metadata.published_date.unwrap().value, "2026-11-09");
        assert_eq!(revision.attachments.iter().filter(|a| matches!(a, memex_core::AttachmentRef::Evidence { .. })).count(), 1);
        assert_eq!(web::collect_binary(library.path(), Some(snapshots.path()), &bytes, false).unwrap()["item_id"], id);
        let attachment = revision.attachments.iter().find_map(|a| match a {
            memex_core::AttachmentRef::Evidence { relative_path, .. } => Some(library.path().join("items").join(id).join(relative_path)),
            _ => None,
        }).unwrap();
        fs::remove_file(&attachment).unwrap();
        assert!(web::collect_binary(library.path(), Some(snapshots.path()), &bytes, false).is_err());
        fs::write(&attachment, image).unwrap();
        assert_eq!(web::collect_binary(library.path(), Some(snapshots.path()), &bytes, false).unwrap()["item_id"], id);
        payload["external_capture_id"] = json!("web-binary-2");
        let again = web::collect_binary(library.path(), Some(snapshots.path()), &binary_payload(&serde_json::to_vec(&payload).unwrap()), false).unwrap();
        assert_eq!(again["item_id"], id);
        assert_eq!(again["reused"], true);
        payload["source_url"] = json!("https://mp.weixin.qq.com/s/site-boundary");
        assert!(web::collect_binary(library.path(), None, &binary_payload(&serde_json::to_vec(&payload).unwrap()), true).is_err());
        payload["source_url"] = json!("https://example.invalid/page");
        payload.as_object_mut().unwrap().remove("web");
        assert!(prepare(serde_json::from_value(payload.clone()).unwrap()).is_err());
        payload["images"] = json!([]);
        let (legacy, ..) = prepare(serde_json::from_value(payload).unwrap()).unwrap();
        let legacy_draft = lots_scout_extract::Extractor::extract(&lots_scout_extract::GenericWebpageExtractor, &legacy).unwrap();
        assert_eq!(legacy_draft.extractor_version, "2");
    }
    fn start() -> (u16, tempfile::TempDir, tempfile::TempDir) {
        let (library, snapshots) = roots();
        let server = IntakeServer::bind(
            library.path(),
            snapshots.path(),
            *b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "https://extension.invalid".into(),
            0,
        )
        .unwrap();
        let port = server.port;
        assert!(server.startup_receipt().loopback_only);
        thread::spawn(move || server.serve().unwrap());
        (port, library, snapshots)
    }
    fn raw(port: u16, request: &[u8]) -> String {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream.write_all(request).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }
    fn post(port: u16, nonce: char, body: &[u8]) -> String {
        let request = format!("POST {PATH} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: https://extension.invalid\r\nContent-Type: application/json\r\nContent-Length: {}\r\nX-MemeX-Intake-Secret: {}\r\nX-MemeX-Intake-Nonce: {}\r\nConnection: close\r\n\r\n", body.len(), "a".repeat(64), nonce.to_string().repeat(64));
        let mut bytes = request.into_bytes();
        bytes.extend_from_slice(body);
        raw(port, &bytes)
    }

    #[test]
    fn url_only_trial_rejects_authenticated_json_and_binary_snapshots() {
        let (library, snapshots) = roots();
        let server = IntakeServer::bind(
            library.path(),
            snapshots.path(),
            [b'a'; 64],
            "https://extension.invalid".into(),
            0,
        )
        .unwrap()
        .without_snapshots();
        let port = server.port;
        thread::spawn(move || server.serve().unwrap());
        let body = payload("not-on-trial-allowlist");
        assert!(post(port, 'a', &body).starts_with("HTTP/1.1 404"));
        assert!(post_binary(port, 'b', &binary_payload(&body)).starts_with("HTTP/1.1 404"));
        assert!(memex_store::list_items(library.path()).unwrap().is_empty());
        assert_eq!(fs::read_dir(snapshots.path()).unwrap().count(), 0);
    }

    fn post_binary(port: u16, nonce: char, body: &[u8]) -> String {
        custom_post(
            port,
            nonce,
            body,
            CAPTURE_CONTENT_TYPE_V1,
            &body.len().to_string(),
            "",
        )
    }
    fn custom_post(
        port: u16,
        nonce: char,
        body: &[u8],
        content_type: &str,
        content_length: &str,
        extra_headers: &str,
    ) -> String {
        let request = format!("POST {PATH} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: https://extension.invalid\r\nContent-Type: {content_type}\r\nContent-Length: {content_length}\r\nX-MemeX-Intake-Secret: {}\r\nX-MemeX-Intake-Nonce: {}\r\n{extra_headers}Connection: close\r\n\r\n", "a".repeat(64), nonce.to_string().repeat(64));
        let mut bytes = request.into_bytes();
        bytes.extend_from_slice(body);
        raw(port, &bytes)
    }

    #[test]
    fn secret_file_permissions_and_syntax() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret");
        fs::write(&path, format!("{}\n", "a".repeat(64))).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert_eq!(read_pairing_secret(&path).unwrap(), [b'a'; 64]);
        fs::write(&path, "BAD").unwrap();
        assert!(read_pairing_secret(&path).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let linked = dir.path().join("linked-secret");
            symlink(&path, &linked).unwrap();
            assert!(read_pairing_secret(linked).is_err());
        }
    }

    #[test]
    fn live_preflight_first_replay_nonce_and_readback() {
        let (port, library, snapshots) = start();
        let options = format!("OPTIONS {PATH} HTTP/1.1\r\nHost: localhost:{port}\r\nOrigin: https://extension.invalid\r\nAccess-Control-Request-Method: POST\r\nAccess-Control-Request-Headers: content-type,x-memex-intake-secret,x-memex-intake-nonce\r\nConnection: close\r\n\r\n");
        let response = raw(port, options.as_bytes());
        assert!(response.starts_with("HTTP/1.1 204"));
        assert!(response.contains("Access-Control-Allow-Origin: https://extension.invalid"));
        let body = payload("capture-synthetic-1");
        let first = post(port, 'b', &body);
        assert!(first.starts_with("HTTP/1.1 201"), "{first}");
        let replay = post(port, 'c', &body);
        assert!(replay.starts_with("HTTP/1.1 200"), "{replay}");
        let duplicate = post(port, 'c', &body);
        assert!(duplicate.starts_with("HTTP/1.1 409"));
        let first_json: serde_json::Value =
            serde_json::from_str(first.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        let replay_json: serde_json::Value =
            serde_json::from_str(replay.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(first_json["item_id"], replay_json["item_id"]);
        let (_, revision) =
            memex_store::read_item(library.path(), first_json["item_id"].as_str().unwrap())
                .unwrap();
        assert_eq!(
            memex_analysis::translation_status(
                library.path(),
                first_json["item_id"].as_str().unwrap(),
                &revision.revision_id,
            )
            .unwrap()
            .unwrap()
            .state,
            memex_analysis::TranslationJobState::Queued
        );
        assert_eq!(
            revision.metadata.creators.unwrap().value[0].display_name,
            "Author One"
        );
        assert_eq!(revision.attachments.len(), 1);
        assert_eq!(revision.evidence_ids.len(), 1);
        assert_eq!(revision.metadata.language.unwrap().value, "zh-CN");
        assert_eq!(
            revision.metadata.manual_tags.unwrap().value,
            vec!["a".to_string(), "z".to_string()]
        );
        assert_eq!(fs::read_dir(snapshots.path()).unwrap().count(), 1);
        for private in [
            "Useful synthetic text",
            "Author One",
            "example.invalid/file.pdf",
            &"a".repeat(64),
        ] {
            assert!(!first.contains(private), "receipt leaked request data");
        }
    }

    #[cfg(unix)]
    #[test]
    fn durable_capture_receipt_survives_analysis_queue_failure() {
        use std::os::unix::fs::symlink;

        let (library, snapshots) = roots();
        symlink(snapshots.path(), library.path().join("analysis")).unwrap();
        let server = IntakeServer::bind(
            library.path(),
            snapshots.path(),
            *b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "https://extension.invalid".into(),
            0,
        )
        .unwrap();
        let port = server.port;
        thread::spawn(move || server.serve().unwrap());

        let body = payload("capture-analysis-unavailable");
        let first = post(port, 'f', &body);
        assert!(first.starts_with("HTTP/1.1 201"), "{first}");
        let replay = post(port, 'e', &body);
        assert!(replay.starts_with("HTTP/1.1 200"), "{replay}");
        let receipt: serde_json::Value =
            serde_json::from_str(first.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        let item_id = receipt["item_id"].as_str().unwrap();
        let revision_id = receipt["revision_id"].as_str().unwrap();
        let (item, revision) = memex_store::read_item(library.path(), item_id).unwrap();
        assert_eq!(item.current_revision, revision_id);
        assert_eq!(revision.revision_id, revision_id);
        assert!(memex_analysis::translation_status(library.path(), item_id, revision_id).is_err());
    }

    #[test]
    fn routes_exact_weixin_articles_to_site_translator() {
        let (port, library, snapshots) = start();
        let body = weixin_payload("capture-weixin");
        let first = post(port, 'd', &body);
        assert!(first.starts_with("HTTP/1.1 201"), "{first}");
        let replay = post(port, 'e', &body);
        assert!(replay.starts_with("HTTP/1.1 200"), "{replay}");
        let first_json: serde_json::Value =
            serde_json::from_str(first.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        let replay_json: serde_json::Value =
            serde_json::from_str(replay.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(first_json["item_id"], replay_json["item_id"]);
        let (_, revision) =
            memex_store::read_item(library.path(), first_json["item_id"].as_str().unwrap())
                .unwrap();
        assert_eq!(revision.title, "微信资料演习");
        assert_eq!(revision.provenance[0].tool_id, "weixin.article");
        assert_eq!(revision.provenance[0].tool_version, "3");
        assert!(revision.content_markdown.contains("微信公众号资料摄取演习"));
        assert!(revision
            .content_markdown
            .contains("![资料图0](attachments/sha256-4c4b6a3be1314ab86138bef4314dde022e600960d8689a2c8f8631802d20dab6.png)"));
        assert_eq!(
            revision
                .content_markdown
                .matches("attachments/sha256-")
                .count(),
            15
        );
        assert!(!revision.content_markdown.contains("mmbiz.qpic.cn"));
        assert_eq!(revision.attachments.len(), 15);
        assert_eq!(revision.evidence_ids.len(), 16);
        let attachment = library
            .path()
            .join("items")
            .join(first_json["item_id"].as_str().unwrap())
            .join("attachments/sha256-4c4b6a3be1314ab86138bef4314dde022e600960d8689a2c8f8631802d20dab6.png");
        assert_eq!(fs::read(attachment).unwrap(), b"\x89PNG\r\n\x1a\n");
        assert_eq!(
            fs::read_dir(
                library
                    .path()
                    .join("items")
                    .join(first_json["item_id"].as_str().unwrap())
                    .join("attachments")
            )
            .unwrap()
            .count(),
            15
        );
        assert!(!revision.content_markdown.contains("WEIXIN_NAV_SENTINEL"));
        assert!(!revision.content_markdown.contains("WEIXIN_SCRIPT_SENTINEL"));
        assert_eq!(fs::read_dir(snapshots.path()).unwrap().count(), 1);
    }

    #[test]
    fn comments_are_separate_durable_evidence_and_cross_transport_identity() {
        let (port, library, snapshots) = start();
        // First establish the legacy source profile; adding comments must not rewrite it.
        assert!(post(port, '0', &payload("legacy")).starts_with("HTTP/1.1 201"));
        let comment = "<section><p>COMMENT-ONLY 紫藤🙂 distinct evidence.</p></section>";
        let digest = hex::encode(Sha256::digest(comment));
        let mut value: serde_json::Value =
            serde_json::from_slice(&weixin_payload("comments-1")).unwrap();
        value["sensitivity"] = json!("public");
        value["comments"] = json!({"state":"captured", "html": {
            "media_type":"text/html", "byte_size":comment.len(), "sha256":digest, "body":comment
        }});
        let bytes = serde_json::to_vec(&value).unwrap();
        let response = post_binary(port, '1', &binary_payload(&bytes));
        assert!(response.starts_with("HTTP/1.1 201"), "{response}");
        let receipt: serde_json::Value =
            serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(
            receipt["comments"],
            json!({"state":"captured", "reason":null})
        );
        assert_eq!(receipt["quality_check"]["state"], "not_requested");
        assert!(!response.contains("COMMENT-ONLY"));
        let (item, revision) =
            memex_store::read_item(library.path(), receipt["item_id"].as_str().unwrap()).unwrap();
        assert_eq!(item.sensitivity, Sensitivity::Internal);
        assert!(!revision.content_markdown.contains("COMMENT-ONLY"));
        assert_eq!(revision.attachments.len(), 15);
        assert_eq!(revision.evidence_ids.len(), 18);
        let evidence: Vec<serde_json::Value> = revision
            .evidence_ids
            .iter()
            .map(|id| {
                serde_json::from_slice(
                    &fs::read(
                        library
                            .path()
                            .join("evidence")
                            .join(id)
                            .join("evidence.json"),
                    )
                    .unwrap(),
                )
                .unwrap()
            })
            .collect();
        let saved = evidence
            .iter()
            .find(|v| v["logical_locator"] == "comments_html_v1")
            .unwrap();
        assert_eq!(
            saved["backend_ref"],
            format!("memex-intake-v1:sha256:{digest}")
        );
        assert_eq!(saved["descriptor"]["sha256"], digest);
        assert_eq!(
            fs::read(snapshots.path().join(format!("{digest}.html"))).unwrap(),
            comment.as_bytes()
        );
        assert!(post(port, '2', &bytes).starts_with("HTTP/1.1 200"));
        value["comments"] = json!({"state":"none_found", "html":null});
        assert!(post(port, '3', &serde_json::to_vec(&value).unwrap()).starts_with("HTTP/1.1 409"));
        assert_eq!(
            fs::read_dir(library.path().join("items")).unwrap().count(),
            2
        );
        value["external_capture_id"] = json!("comments-negative");
        let bytes = serde_json::to_vec(&value).unwrap();
        assert!(post(port, '4', &bytes).starts_with("HTTP/1.1 201"));
        assert!(post_binary(port, '5', &binary_payload(&bytes)).starts_with("HTTP/1.1 200"));
        value["comments"]["state"] = json!("in_frame");
        assert!(post(port, '6', &serde_json::to_vec(&value).unwrap()).starts_with("HTTP/1.1 409"));
        assert_eq!(memex_store::list_items(library.path()).unwrap().len(), 3);
    }

    #[test]
    fn invalid_comments_reject_before_snapshot_or_store_writes() {
        let (port, library, snapshots) = start();
        let html = "<p>comment</p>";
        let base = json!({"state":"captured", "html": {"media_type":"text/html", "byte_size":html.len(),
            "sha256":hex::encode(Sha256::digest(html)), "body":html}});
        let mut cases = vec![
            json!({"state":"captured","html":null}),
            json!({"state":"pending","html":null}),
        ];
        for (field, value) in [
            ("sha256", json!("0".repeat(64))),
            ("byte_size", json!(2)),
            ("media_type", json!("application/json")),
        ] {
            let mut bad = base.clone();
            bad["html"][field] = value;
            cases.push(bad);
        }
        let mut contradictory = base.clone();
        contradictory["state"] = json!("none_found");
        cases.push(contradictory);
        for html in [
            "<script>active()</script>".to_owned(),
            "<form>private</form>".into(),
            "<iframe></iframe>".into(),
            "<p onclick='act()'>text</p>".into(),
            "x".repeat(MAX_COMMENTS_BYTES + 1),
        ] {
            cases.push(json!({"state":"captured", "html": {"media_type":"text/html", "byte_size":html.len(),
                "sha256":hex::encode(Sha256::digest(&html)), "body":html}}));
        }
        for comments in cases {
            let mut value: serde_json::Value =
                serde_json::from_slice(&payload("invalid-comments")).unwrap();
            value["comments"] = comments;
            let bytes = serde_json::to_vec(&value).unwrap();
            assert!(post(port, '7', &bytes).starts_with("HTTP/1.1 400"));
            assert!(post_binary(port, '7', &binary_payload(&bytes)).starts_with("HTTP/1.1 400"));
        }
        assert_eq!(fs::read_dir(library.path()).unwrap().count(), 0);
        assert_eq!(fs::read_dir(snapshots.path()).unwrap().count(), 0);
    }

    #[test]
    fn binary_transport_cross_replays_with_json_without_canonical_drift() {
        for (case, json_first) in [("json-first", true), ("binary-first", false)] {
            let (port, library, snapshots) = start();
            let json = weixin_payload(&format!("capture-weixin-{case}"));
            let binary = binary_payload(&json);
            assert!(binary.len() < json.len());

            let first = if json_first {
                post(port, '1', &json)
            } else {
                post_binary(port, '1', &binary)
            };
            assert!(first.starts_with("HTTP/1.1 201"), "{first}");
            let first_receipt: serde_json::Value =
                serde_json::from_str(first.split("\r\n\r\n").nth(1).unwrap()).unwrap();
            let item_id = first_receipt["item_id"].as_str().unwrap();
            let capture_id = first_receipt["capture_id"].as_str().unwrap();
            let revision_id = first_receipt["revision_id"].as_str().unwrap();
            let (_, revision) = memex_store::read_item(library.path(), item_id).unwrap();
            assert_eq!(revision.attachments.len(), 15);
            assert_eq!(revision.evidence_ids.len(), 16);

            let item_dir = library.path().join("items").join(item_id);
            let mut canonical_paths = vec![
                library
                    .path()
                    .join("captures")
                    .join(capture_id)
                    .join("capture.json"),
                library
                    .path()
                    .join("receipts")
                    .join(format!("{capture_id}.json")),
                item_dir.join("item.json"),
                item_dir.join("links.json"),
                item_dir
                    .join("revisions")
                    .join(format!("{revision_id}.json")),
                library
                    .path()
                    .join("integrity/items")
                    .join(format!("{item_id}.json")),
            ];
            canonical_paths.extend(revision.evidence_ids.iter().map(|evidence_id| {
                library
                    .path()
                    .join("evidence")
                    .join(evidence_id)
                    .join("evidence.json")
            }));
            let canonical_before = canonical_paths
                .iter()
                .map(|path| fs::read(path).unwrap())
                .collect::<Vec<_>>();
            let snapshot_before = fs::read(
                fs::read_dir(snapshots.path())
                    .unwrap()
                    .next()
                    .unwrap()
                    .unwrap()
                    .path(),
            )
            .unwrap();

            let duplicate_nonce = if json_first {
                post_binary(port, '1', &binary)
            } else {
                post(port, '1', &json)
            };
            assert!(
                duplicate_nonce.starts_with("HTTP/1.1 409"),
                "{duplicate_nonce}"
            );
            let replay = if json_first {
                post_binary(port, '2', &binary)
            } else {
                post(port, '2', &json)
            };
            assert!(replay.starts_with("HTTP/1.1 200"), "{replay}");
            let replay_receipt: serde_json::Value =
                serde_json::from_str(replay.split("\r\n\r\n").nth(1).unwrap()).unwrap();
            assert_eq!(first_receipt["status"], "created");
            assert_eq!(replay_receipt["status"], "replayed");
            for field in ["schema_version", "capture_id", "item_id", "revision_id"] {
                assert_eq!(first_receipt[field], replay_receipt[field]);
            }
            for (path, expected) in canonical_paths.iter().zip(canonical_before) {
                assert_eq!(fs::read(path).unwrap(), expected);
            }
            assert_eq!(fs::read_dir(snapshots.path()).unwrap().count(), 1);
            assert_eq!(
                fs::read(
                    fs::read_dir(snapshots.path())
                        .unwrap()
                        .next()
                        .unwrap()
                        .unwrap()
                        .path()
                )
                .unwrap(),
                snapshot_before
            );
            for private in [
                "微信公众号资料摄取演习",
                "mmbiz.qpic.cn",
                &"a".repeat(64),
                &"1".repeat(64),
            ] {
                assert!(!first.contains(private));
                assert!(!replay.contains(private));
            }
        }
    }

    #[test]
    fn binary_transport_rejects_bad_framing_without_consuming_nonce() {
        let (port, library, snapshots) = start();
        let json = payload("capture-binary-framing");
        let valid = binary_payload(&json);
        let mut wrong_magic = valid.clone();
        wrong_magic[0] ^= 1;
        let mut wrong_version = valid.clone();
        wrong_version[7] = 2;
        let mut zero_manifest = CAPTURE_MAGIC_V1.to_vec();
        zero_manifest.extend_from_slice(&0u32.to_be_bytes());
        let mut oversized_manifest = CAPTURE_MAGIC_V1.to_vec();
        oversized_manifest.extend_from_slice(&((MAX_MANIFEST as u32) + 1).to_be_bytes());
        let mut trailing = valid.clone();
        trailing.push(0);
        let truncated = valid[..valid.len() - 1].to_vec();
        let mut bad_hash_json: serde_json::Value = serde_json::from_slice(&json).unwrap();
        bad_hash_json["html"]["sha256"] = json!("0".repeat(64));
        let bad_hash = binary_payload(&serde_json::to_vec(&bad_hash_json).unwrap());
        let mut too_many_tags_json: serde_json::Value = serde_json::from_slice(&json).unwrap();
        too_many_tags_json["metadata"]["manual_tags"] = json!(vec!["x"; 257]);
        let too_many_tags = binary_payload(&serde_json::to_vec(&too_many_tags_json).unwrap());

        for body in [
            wrong_magic,
            wrong_version,
            zero_manifest,
            oversized_manifest,
            trailing,
            truncated,
            bad_hash,
            too_many_tags,
        ] {
            let response = post_binary(port, 'a', &body);
            assert!(response.starts_with("HTTP/1.1 400"), "{response}");
        }
        for content_type in [
            "application/vnd.memex.capture-v1; charset=utf-8",
            "Application/Vnd.MemeX.Capture-V1",
            "application/vnd.memex.capture-v2",
        ] {
            let response = custom_post(
                port,
                'a',
                &valid,
                content_type,
                &valid.len().to_string(),
                "",
            );
            assert!(response.starts_with("HTTP/1.1 415"), "{response}");
        }
        let accepted = post_binary(port, 'a', &valid);
        assert!(accepted.starts_with("HTTP/1.1 201"), "{accepted}");
        assert_eq!(memex_store::list_items(library.path()).unwrap().len(), 1);
        assert_eq!(fs::read_dir(snapshots.path()).unwrap().count(), 1);
    }

    #[test]
    fn json_transport_checks_decoded_image_budget_during_decode() {
        let images = [b"12345678".as_slice(), b"123456789".as_slice()]
            .into_iter()
            .enumerate()
            .map(|(index, bytes)| ImageDto {
                source_url: format!("https://mmbiz.qpic.cn/test/{index}.png"),
                media_type: "image/png".into(),
                byte_size: bytes.len() as u64,
                sha256: hex::encode(Sha256::digest(bytes)),
                body_base64: BASE64.encode(bytes),
            })
            .collect();
        assert!(decode_json_images(images, 16).is_err());
    }

    #[test]
    fn verified_binary_translation_failure_consumes_nonce_without_writes() {
        let (port, library, snapshots) = start();
        let mut blocked: serde_json::Value =
            serde_json::from_slice(&weixin_payload("capture-binary-translation-failure")).unwrap();
        let html = "<html><body><section id='js_content'> </section></body></html>";
        blocked["html"]["body"] = json!(html);
        blocked["html"]["byte_size"] = json!(html.len());
        blocked["html"]["sha256"] = json!(hex::encode(Sha256::digest(html.as_bytes())));
        blocked["images"] = json!([]);
        let body = binary_payload(&serde_json::to_vec(&blocked).unwrap());

        assert!(post_binary(port, 'b', &body).starts_with("HTTP/1.1 422"));
        assert!(post_binary(port, 'b', &body).starts_with("HTTP/1.1 409"));
        assert_eq!(fs::read_dir(library.path()).unwrap().count(), 0);
        assert_eq!(fs::read_dir(snapshots.path()).unwrap().count(), 0);
    }

    #[test]
    fn captured_images_fail_closed_during_prepare_or_translation() {
        let valid: serde_json::Value =
            serde_json::from_slice(&weixin_payload("capture-weixin-image-negative")).unwrap();
        let mut invalid_cases = Vec::new();

        let mut lookalike_host = valid.clone();
        lookalike_host["images"][0]["source_url"] =
            json!("https://mmbiz.qpic.cn.evil.invalid/assets/diagram.png");
        invalid_cases.push(lookalike_host);

        let mut alternate_port = valid.clone();
        alternate_port["images"][0]["source_url"] =
            json!("https://mmbiz.qpic.cn:444/assets/diagram.png");
        invalid_cases.push(alternate_port);

        let mut wrong_digest = valid.clone();
        wrong_digest["images"][0]["sha256"] = json!("0".repeat(64));
        invalid_cases.push(wrong_digest);

        let mut active_media = valid.clone();
        active_media["images"][0]["media_type"] = json!("image/svg+xml");
        invalid_cases.push(active_media);

        let mut duplicate = valid.clone();
        duplicate["images"] = json!([valid["images"][0].clone(), valid["images"][0].clone()]);
        invalid_cases.push(duplicate);

        let mut unsupported_source = valid.clone();
        unsupported_source["source_url"] = json!("https://example.invalid/article");
        invalid_cases.push(unsupported_source);

        let mut noncanonical_base64 = valid.clone();
        let encoded = noncanonical_base64["images"][0]["body_base64"]
            .as_str()
            .unwrap()
            .to_owned();
        noncanonical_base64["images"][0]["body_base64"] = json!(format!("{encoded}\n"));
        invalid_cases.push(noncanonical_base64);

        let mut too_many = valid.clone();
        too_many["images"] =
            serde_json::Value::Array(vec![valid["images"][0].clone(); MAX_IMAGES + 1]);

        for (index, body) in invalid_cases.into_iter().enumerate() {
            let dto: CaptureDto = serde_json::from_value(body).unwrap();
            assert!(prepare(dto).is_err(), "case {index} passed image preflight");
        }
        assert!(serde_json::from_value::<CaptureDto>(too_many).is_err());

        let mut unmatched = valid;
        unmatched["images"][0]["source_url"] =
            json!("https://mmbiz.qpic.cn/assets/not-in-the-article.png");
        let dto: CaptureDto = serde_json::from_value(unmatched).unwrap();
        let (capture, _, _, _, _, _, _) = prepare(dto).unwrap();
        assert!(WeixinArticleExtractor.extract(&capture).is_err());
    }

    #[test]
    fn weixin_missing_duplicate_and_empty_body_fail_before_owner_writes() {
        for (index, html) in [
            "<html><body><h1>环境异常</h1><p>完成验证后即可访问。</p></body></html>",
            "<html><body><section id='js_content'>one</section><section id='js_content'>two</section></body></html>",
            "<html><body><section id='js_content'> \n\t </section></body></html>",
        ]
        .into_iter()
        .enumerate()
        {
            let (port, library, snapshots) = start();
            let mut blocked: serde_json::Value =
                serde_json::from_slice(&weixin_payload("capture-weixin-negative")).unwrap();
            blocked["external_capture_id"] = json!(format!("capture-weixin-negative-{index}"));
            blocked["html"]["body"] = json!(html);
            blocked["html"]["byte_size"] = json!(html.len());
            blocked["html"]["sha256"] = json!(hex::encode(Sha256::digest(html.as_bytes())));
            assert!(post(port, 'f', &serde_json::to_vec(&blocked).unwrap())
                .starts_with("HTTP/1.1 422"));
            assert_eq!(fs::read_dir(library.path()).unwrap().count(), 0);
            assert_eq!(fs::read_dir(snapshots.path()).unwrap().count(), 0);
        }
    }

    #[test]
    fn strict_auth_origin_and_translation_before_writes() {
        let (port, library, snapshots) = start();
        let body = payload("capture-bad");
        let bad_origin = format!("POST {PATH} HTTP/1.1\r\nHost: localhost:{port}\r\nOrigin: https://wrong.invalid\r\nContent-Type: application/json\r\nContent-Length: {}\r\nX-MemeX-Intake-Secret: {}\r\nX-MemeX-Intake-Nonce: {}\r\nConnection: close\r\n\r\n", body.len(), "a".repeat(64), "d".repeat(64));
        let mut request = bad_origin.into_bytes();
        request.extend_from_slice(&body);
        assert!(raw(port, &request).starts_with("HTTP/1.1 403"));
        let mut malformed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let html = "<html><body></body></html>";
        malformed["html"]["body"] = json!(html);
        malformed["html"]["byte_size"] = json!(html.len());
        malformed["html"]["sha256"] = json!(hex::encode(Sha256::digest(html.as_bytes())));
        assert!(
            post(port, 'e', &serde_json::to_vec(&malformed).unwrap()).starts_with("HTTP/1.1 422")
        );
        assert_eq!(fs::read_dir(snapshots.path()).unwrap().count(), 0);
        assert!(!library.path().join("items").exists());
    }

    #[test]
    fn strict_post_headers_and_immutable_capture_conflict() {
        let (port, library, _snapshots) = start();
        let body = payload("capture-immutable");
        let bad_secret = format!("POST {PATH} HTTP/1.1\r\nHost: localhost:{port}\r\nOrigin: https://extension.invalid\r\nContent-Type: application/json\r\nContent-Length: {}\r\nX-MemeX-Intake-Secret: {}\r\nX-MemeX-Intake-Nonce: {}\r\nConnection: close\r\n\r\n", body.len(), "f".repeat(64), "1".repeat(64));
        let mut request = bad_secret.into_bytes();
        request.extend_from_slice(&body);
        assert!(raw(port, &request).starts_with("HTTP/1.1 401"));
        assert!(!library.path().join("items").exists());

        let cookie = format!("POST {PATH} HTTP/1.1\r\nHost: localhost:{port}\r\nOrigin: https://extension.invalid\r\nContent-Type: application/json\r\nContent-Length: {}\r\nX-MemeX-Intake-Secret: {}\r\nX-MemeX-Intake-Nonce: {}\r\nCookie: should-never-enter-intake\r\nConnection: close\r\n\r\n", body.len(), "a".repeat(64), "2".repeat(64));
        let mut request = cookie.into_bytes();
        request.extend_from_slice(&body);
        assert!(raw(port, &request).starts_with("HTTP/1.1 400"));
        assert!(post(port, '3', &body).starts_with("HTTP/1.1 201"));

        let mut changed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        changed["title"] = json!("Changed immutable capture");
        let changed = serde_json::to_vec(&changed).unwrap();
        assert!(post(port, '4', &changed).starts_with("HTTP/1.1 409"));
        assert_eq!(memex_store::list_items(library.path()).unwrap().len(), 1);

        let mut changed_media: serde_json::Value = serde_json::from_slice(&body).unwrap();
        changed_media["html"]["media_type"] = json!("application/xhtml+xml");
        assert!(
            post(port, '5', &serde_json::to_vec(&changed_media).unwrap())
                .starts_with("HTTP/1.1 409")
        );
        assert_eq!(memex_store::list_items(library.path()).unwrap().len(), 1);

        let duplicate_origin = format!("POST {PATH} HTTP/1.1\r\nHost: localhost:{port}\r\nOrigin: https://extension.invalid\r\nOrigin: https://extension.invalid\r\nContent-Type: application/json\r\nContent-Length: {}\r\nX-MemeX-Intake-Secret: {}\r\nX-MemeX-Intake-Nonce: {}\r\nConnection: close\r\n\r\n", body.len(), "a".repeat(64), "6".repeat(64));
        let mut request = duplicate_origin.into_bytes();
        request.extend_from_slice(&body);
        assert!(raw(port, &request).starts_with("HTTP/1.1 400"));
    }

    #[test]
    fn framing_credentials_and_html_manifest_fail_before_writes() {
        let (port, library, snapshots) = start();
        let body = payload("capture-negative-matrix");
        let length = body.len().to_string();
        let leading_zero_length = format!("0{length}");
        let oversized_length = (MAX_REQUEST + 1).to_string();
        let cases = [
            (
                '7',
                "application/json",
                length.as_str(),
                "Authorization: Bearer synthetic\r\n",
                400,
            ),
            (
                '8',
                "application/json",
                length.as_str(),
                "Transfer-Encoding: chunked\r\n",
                400,
            ),
            (
                '9',
                "application/json",
                length.as_str(),
                "Upgrade: websocket\r\n",
                400,
            ),
            (
                'a',
                "application/json",
                length.as_str(),
                "Expect: 100-continue\r\n",
                400,
            ),
            (
                'b',
                "application/json",
                length.as_str(),
                "Content-Type: application/json\r\n",
                400,
            ),
            ('c', "text/plain", length.as_str(), "", 415),
            (
                '1',
                "application/json",
                length.as_str(),
                "Content-Encoding: identity\r\n",
                400,
            ),
            (
                '2',
                "application/json",
                length.as_str(),
                "Trailer: X-Synthetic\r\n",
                400,
            ),
            (
                'd',
                "application/json",
                leading_zero_length.as_str(),
                "",
                400,
            ),
            ('e', "application/json", oversized_length.as_str(), "", 413),
        ];
        for (nonce, content_type, declared, extra, status) in cases {
            let response = custom_post(port, nonce, &body, content_type, declared, extra);
            assert!(
                response.starts_with(&format!("HTTP/1.1 {status}")),
                "unexpected response for {extra:?}: {response}"
            );
        }

        let mut bad_hash: serde_json::Value = serde_json::from_slice(&body).unwrap();
        bad_hash["html"]["sha256"] = json!("0".repeat(64));
        assert!(
            post(port, 'f', &serde_json::to_vec(&bad_hash).unwrap()).starts_with("HTTP/1.1 400")
        );
        let mut bad_size: serde_json::Value = serde_json::from_slice(&body).unwrap();
        bad_size["html"]["byte_size"] = json!(1);
        assert!(
            post(port, '0', &serde_json::to_vec(&bad_size).unwrap()).starts_with("HTTP/1.1 400")
        );
        assert!(!library.path().join("items").exists());
        assert_eq!(fs::read_dir(snapshots.path()).unwrap().count(), 0);
    }

    #[test]
    fn snapshot_cas_reuses_exact_and_rejects_mismatch() {
        let (_library, snapshots) = roots();
        let body = b"synthetic snapshot";
        let digest = hex::encode(Sha256::digest(body));
        install_snapshot(snapshots.path(), &digest, body).unwrap();
        install_snapshot(snapshots.path(), &digest, body).unwrap();
        let target = snapshots.path().join(format!("{digest}.html"));
        fs::write(&target, b"mismatched bytes!").unwrap();
        assert!(install_snapshot(snapshots.path(), &digest, body).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"mismatched bytes!");

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let linked_body = b"linked snapshot";
            let linked_digest = hex::encode(Sha256::digest(linked_body));
            let outside = tempfile::NamedTempFile::new().unwrap();
            fs::write(outside.path(), linked_body).unwrap();
            symlink(
                outside.path(),
                snapshots.path().join(format!("{linked_digest}.html")),
            )
            .unwrap();
            assert!(install_snapshot(snapshots.path(), &linked_digest, linked_body).is_err());
        }
    }
}
