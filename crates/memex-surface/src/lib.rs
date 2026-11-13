use anyhow::{anyhow, Result};
use chrono::DateTime;
use memex_core::Sensitivity;
use pulldown_cmark::{html, CowStr, Event, Options, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::{
    io::Read,
    net::{Ipv4Addr, TcpListener},
    path::PathBuf,
};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};
use url::{form_urlencoded, Url};


pub const SCHEMA_VERSION: u8 = 1;
pub const SURFACE_VERSION: &str = "local_v0";
const MOBILE_SURFACE_VERSION: &str = "mobile_library_v0";
const MOBILE_HOMEPAGE_VERSION: &str = "mobile_library_homepage_capture_v1";
const INDEX_BUDGET: usize = 64 * 1024 * 1024;
const MAX_CAPTURE_BODY: u64 = 64 * 1024;
const CSS: &str = include_str!("../assets/mobile-library-v0.css");
const JS: &str = include_str!("../assets/mobile-library-v0.js");

#[derive(Clone, Serialize)]
struct LibraryItem {
    item_id: String,
    revision_id: String,
    item_type: String,
    title: String,
    creators: Vec<String>,
    published_date: Option<String>,
    captured_at: Option<String>,
    #[serde(skip)]
    captured_at_epoch: Option<i64>,
    site_name: Option<String>,
    manual_tags: Vec<String>,
    sensitivity: Sensitivity,
}

#[derive(Clone, Serialize)]
struct TagFacet {
    tag: String,
    count: usize,
}

#[derive(Clone)]
pub struct SurfaceConfig {
    pub library: PathBuf,
    pub max_sensitivity: Sensitivity,
    pub port: u16,
}

#[derive(Debug, Serialize)]
pub struct StartupReceipt {
    pub schema_version: u8,
    pub surface_version: &'static str,
    pub url: String,
    pub max_sensitivity: Sensitivity,
    pub read_only: bool,
}

pub struct SurfaceServer {
    server: Server,
    config: SurfaceConfig,
    port: u16,
    orb_portal: Option<OrbPortalOrigin>,
    lan_origin: Option<String>,
    agent_socket: Option<PathBuf>,
}

struct MobileIndex {
    items: Vec<LibraryItem>,
    tags: Vec<TagFacet>,
    available: bool,
}

#[derive(Clone)]
struct OrbPortalOrigin {
    origin: String,
    authority: String,
}

struct PrivateLanConfig {
    address: Ipv4Addr,
    hostname: String,
    socket: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureRequest {
    schema_version: u8,
    command: String,
    text: String,
}

fn parse_orb_portal_origin(public_url: &str) -> Result<OrbPortalOrigin> {
    let url = Url::parse(public_url).map_err(|_| anyhow!("invalid orb portal URL"))?;
    let host = url
        .host_str()
        .filter(|host| host.ends_with(".onamp.dev"))
        .ok_or_else(|| anyhow!("invalid orb portal host"))?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(anyhow!("invalid orb portal origin"));
    }
    Ok(OrbPortalOrigin {
        origin: format!("https://{host}"),
        authority: host.to_owned(),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResponseKind {
    Plain,
    Legacy,
    Mobile,
}

#[derive(Serialize)]
struct SourceDto {
    id: String,
    name: String,
    kind: String,
    canonical_uris: Vec<String>,
    evidence_roles: Vec<String>,
    sensitivity: Sensitivity,
    governance: SourceGovernanceDto,
}
#[derive(Serialize)]
struct SourceGovernanceDto {
    admission_state: memex_registry::AdmissionState,
    category: String,
    topic_tags: Vec<String>,
    region: String,
    language: String,
    access_type: String,
    capture_mode: String,
}
#[derive(Serialize)]
struct ItemDto {
    id: String,
    item_type: String,
    current_revision_id: String,
    revision_number: u32,
    title: String,
    source_url: String,
    canonical_url: Option<String>,
    body: String,
    sensitivity: Sensitivity,
}
#[derive(Serialize)]
struct AnalysisInputDto {
    item_id: String,
    revision_id: String,
}
#[derive(Serialize)]
struct AnalysisDto {
    id: String,
    kind: memex_analysis::AnalysisKind,
    title: String,
    body: String,
    topics: Vec<String>,
    created_at: String,
    sensitivity: Sensitivity,
    inputs: Vec<AnalysisInputDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generated_text: Option<memex_analysis::GeneratedTextDescriptor>,
}

#[derive(Clone)]
struct GeneratedValue {
    analysis_id: String,
    body: String,
    summary_job_id: Option<String>,
    translation_job_id: Option<String>,
    model_id: String,
}

#[derive(Clone, Default)]
struct ItemGeneratedText {
    source_language: Option<String>,
    source_language_tag: Option<String>,
    title_translations: BTreeMap<String, Option<GeneratedValue>>,
    summaries: BTreeMap<String, Option<GeneratedValue>>,
    document_translations: BTreeMap<String, Option<GeneratedValue>>,
}

enum TranslationDisplay {
    None,
    Job {
        view: Box<memex_analysis::TranslationJobView>,
        validated_pair: Option<memex_analysis::TranslationAnalysisPair>,
    },
    Unavailable,
}
enum SummaryDisplay {
    None,
    Job {
        view: Box<memex_analysis::SummaryJobView>,
        validated: Option<memex_analysis::SummaryAnalysis>,
    },
    Unavailable,
}

struct ReaderRenderInputs<'a> {
    id: &'a str,
    revision: &'a memex_core::ItemRevision,
    generated: &'a ItemGeneratedText,
    summary: &'a SummaryDisplay,
    translation: &'a TranslationDisplay,
    theme: Option<&'a str>,
    font: Option<&'a str>,
    language: Option<&'a str>,
    source_first: bool,
    leading_html: &'a str,
    route_prefix: &'a str,
    asset_prefix: &'a str,
}

struct MetadataRenderInputs<'a> {
    item: &'a memex_store::ItemReadMetadata,
    generated: &'a ItemGeneratedText,
    summary: &'a SummaryDisplay,
    translation: &'a TranslationDisplay,
    close_url: &'a str,
    route_prefix: &'a str,
    theme: Option<&'a str>,
    font: Option<&'a str>,
}

fn generated_text_from_records(
    item_id: &str,
    revision: &memex_core::ItemRevision,
    records: &[memex_analysis::AnalysisRecord],
    summary_display: &SummaryDisplay,
) -> std::result::Result<ItemGeneratedText, RouteError> {
    let mut generated = ItemGeneratedText::default();
    for record in records {
        if record.inputs.len() != 1
            || record.inputs[0].item_id != item_id
            || record.inputs[0].revision_id != revision.revision_id
        {
            continue;
        }
        if !memex_analysis::item_revision_ref_matches(&record.inputs[0], item_id, revision)
            .map_err(|_| RouteError::Internal)?
        {
            return Ok(ItemGeneratedText::default());
        }
        let Some(descriptor) = record.generated_text.clone() else {
            continue;
        };
        let source_language_tag = descriptor.source_language.to_ascii_lowercase();
        if generated
            .source_language_tag
            .as_ref()
            .is_some_and(|existing| existing != &source_language_tag)
        {
            return Ok(ItemGeneratedText::default());
        }
        generated.source_language_tag = Some(source_language_tag.clone());
        let Some(source_language) = language_family(&source_language_tag) else {
            return Ok(ItemGeneratedText::default());
        };
        if generated
            .source_language
            .as_ref()
            .is_some_and(|existing| existing != source_language)
        {
            return Ok(ItemGeneratedText::default());
        }
        generated.source_language = Some(source_language.to_owned());
        let Some(target_language) = language_family(&descriptor.target_language) else {
            continue;
        };
        let value = GeneratedValue {
            analysis_id: record.analysis_id.clone(),
            body: record.body_markdown.clone(),
            summary_job_id: descriptor.summary_job_id,
            translation_job_id: descriptor.translation_job_id,
            model_id: descriptor.model_id,
        };
        match descriptor.purpose {
            memex_analysis::GeneratedTextPurpose::Summary => {
                if summary_allows_generated(summary_display, &value) {
                    insert_unique_generated(&mut generated.summaries, target_language, value)
                }
            }
            memex_analysis::GeneratedTextPurpose::TitleTranslation => {
                insert_unique_generated(&mut generated.title_translations, target_language, value)
            }
            memex_analysis::GeneratedTextPurpose::DocumentTranslation => insert_unique_generated(
                &mut generated.document_translations,
                target_language,
                value,
            ),
        }
    }
    Ok(generated)
}

#[derive(Serialize)]
struct PublicationDto {
    id: String,
    kind: memex_publication::PublicationKind,
    product: String,
    title: String,
    summary: String,
    body: String,
    topics: Vec<String>,
    created_at: String,
    sensitivity: Sensitivity,
    input_count: usize,
}

fn source_dto(r: memex_registry::Record) -> SourceDto {
    let governance = r.governance;
    SourceDto {
        id: r.source_id,
        name: r.source_profile.name,
        kind: r.source_profile.source_kind,
        canonical_uris: r.source_profile.canonical_uris,
        evidence_roles: r.source_profile.evidence_roles,
        sensitivity: r.source_profile.default_sensitivity,
        governance: SourceGovernanceDto {
            admission_state: governance.admission_state,
            category: governance.category,
            topic_tags: governance.topic_tags,
            region: governance.region,
            language: governance.language,
            access_type: governance.access_type,
            capture_mode: governance.capture_mode,
        },
    }
}
fn item_dto((i, r): (memex_core::Item, memex_core::ItemRevision)) -> ItemDto {
    ItemDto {
        id: i.item_id,
        item_type: i.item_type,
        current_revision_id: i.current_revision,
        revision_number: r.revision_number,
        title: r.title,
        source_url: r.source_url,
        canonical_url: r.canonical_url,
        body: r.content_markdown,
        sensitivity: i.sensitivity.max(r.sensitivity),
    }
}
fn analysis_dto(r: memex_analysis::AnalysisRecord) -> AnalysisDto {
    AnalysisDto {
        id: r.analysis_id,
        kind: r.analysis_kind,
        title: r.title,
        body: r.body_markdown,
        topics: r.topics,
        created_at: r.created_at,
        sensitivity: r.sensitivity,
        inputs: r
            .inputs
            .into_iter()
            .map(|i| AnalysisInputDto {
                item_id: i.item_id,
                revision_id: i.revision_id,
            })
            .collect(),
        generated_text: r.generated_text,
    }
}
fn publication_dto(r: memex_publication::PublicationRecord) -> PublicationDto {
    let input_count = r.inputs.len();
    PublicationDto {
        id: r.publication_id,
        kind: r.publication_kind,
        product: r.product_id,
        title: r.title,
        summary: r.summary,
        body: r.body_markdown,
        topics: r.topics,
        created_at: r.created_at,
        sensitivity: r.sensitivity,
        input_count,
    }
}

fn load_mobile_index(config: &SurfaceConfig) -> Result<MobileIndex> {
    let mut items = Vec::new();
    let mut retained = 0usize;
    let mut available = true;
    let scan = memex_store::scan_item_metadata_at_sensitivity(
        &config.library,
        config.max_sensitivity,
        |m| {
            let creators = m
                .metadata
                .creators
                .as_ref()
                .map(|v| v.value.iter().map(|c| c.display_name.clone()).collect())
                .unwrap_or_default();
            let published_date = m.metadata.published_date.as_ref().map(|v| v.value.clone());
            let captured_at_epoch = m
                .captured_at
                .as_deref()
                .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                .map(|value| value.timestamp());
            let site_name = m.metadata.site_name.as_ref().map(|v| v.value.clone());
            let manual_tags = m
                .metadata
                .manual_tags
                .as_ref()
                .map(|v| v.value.clone())
                .unwrap_or_default();
            let row = LibraryItem {
                item_id: m.item_id,
                revision_id: m.revision_id,
                item_type: m.item_type,
                title: m.title,
                creators,
                published_date,
                captured_at: m.captured_at,
                captured_at_epoch,
                site_name,
                manual_tags,
                sensitivity: m.sensitivity,
            };
            reserve_index_budget(&mut retained, index_cost(&row))?;
            items.push(row);
            Ok(())
        },
    );
    // Preserve the legacy surface's request-time handling of an unsafe optional
    // `items` parent. All ordinary scan/integrity failures still fail closed.
    if let Err(error) = scan {
        let legacy_unsafe_parent = std::fs::symlink_metadata(config.library.join("items"))
            .is_ok_and(|m| m.file_type().is_symlink());
        if !legacy_unsafe_parent {
            return Err(error);
        }
        items.clear();
        available = false;
    }
    items.sort_by(|a, b| {
        b.captured_at_epoch
            .cmp(&a.captured_at_epoch)
            .then_with(|| b.item_id.cmp(&a.item_id))
    });
    let mut tag_counts = BTreeMap::<&str, usize>::new();
    for row in &items {
        for tag in &row.manual_tags {
            *tag_counts.entry(tag).or_default() += 1;
        }
    }
    let mut tags = Vec::with_capacity(tag_counts.len());
    for (tag, count) in tag_counts {
        let facet = TagFacet {
            tag: tag.to_owned(),
            count,
        };
        reserve_index_budget(
            &mut retained,
            std::mem::size_of::<TagFacet>() + facet.tag.capacity(),
        )?;
        tags.push(facet);
    }
    items.shrink_to_fit();
    tags.shrink_to_fit();
    if retained_index_cost(&items, items.capacity(), &tags, tags.capacity())? > INDEX_BUDGET {
        return Err(anyhow!("mobile library index budget exceeded"));
    }
    Ok(MobileIndex {
        items,
        tags,
        available,
    })
}

impl SurfaceServer {
    pub fn new(config: SurfaceConfig) -> Result<Self> {
        Self::new_with_network(config, None, None)
    }

    pub fn new_for_orb_portal(config: SurfaceConfig, public_url: &str) -> Result<Self> {
        Self::new_with_network(config, Some(parse_orb_portal_origin(public_url)?), None)
    }

    pub fn new_for_private_lan(
        config: SurfaceConfig,
        address: Ipv4Addr,
        hostname: &str,
        socket: PathBuf,
    ) -> Result<Self> {
        if socket.is_relative() {
            return Err(anyhow!("invalid private LAN configuration"));
        }
        let hostname = memex_agent::validate_private_lan(address, hostname, config.port)
            .map_err(|_| anyhow!("invalid private LAN configuration"))?;
        Self::new_with_network(
            config,
            None,
            Some(PrivateLanConfig {
                address,
                hostname,
                socket,
            }),
        )
    }

    #[cfg(test)]
    fn new_for_direct_test(config: SurfaceConfig, socket: PathBuf) -> Result<Self> {
        Self::new_with_network(
            config,
            None,
            Some(PrivateLanConfig {
                address: Ipv4Addr::LOCALHOST,
                hostname: "localhost".into(),
                socket,
            }),
        )
    }

    fn new_with_network(
        config: SurfaceConfig,
        orb_portal: Option<OrbPortalOrigin>,
        private_lan: Option<PrivateLanConfig>,
    ) -> Result<Self> {
        let _ = load_mobile_index(&config)?;
        let bind_address = private_lan
            .as_ref()
            .map_or(Ipv4Addr::LOCALHOST, |lan| lan.address);
        let listener = TcpListener::bind((bind_address, config.port))?;
        let port = listener.local_addr()?.port();
        let server = Server::from_listener(listener, None).map_err(|e| anyhow!(e))?;
        let lan_origin = private_lan
            .as_ref()
            .map(|lan| format!("http://{}:{port}", lan.hostname));
        let agent_socket = private_lan.map(|lan| lan.socket);
        Ok(Self {
            server,
            config,
            port,
            orb_portal,
            lan_origin,
            agent_socket,
        })
    }

    pub fn startup_receipt(&self) -> StartupReceipt {
        StartupReceipt {
            schema_version: SCHEMA_VERSION,
            surface_version: SURFACE_VERSION,
            url: self.lan_origin.as_ref().map_or_else(
                || {
                    self.orb_portal.as_ref().map_or_else(
                        || format!("http://127.0.0.1:{}/", self.port),
                        |portal| format!("{}/", portal.origin),
                    )
                },
                |origin| format!("{origin}/"),
            ),
            max_sensitivity: self.config.max_sensitivity,
            read_only: self.agent_socket.is_none(),
        }
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn serve(self) -> Result<()> {
        for request in self.server.incoming_requests() {
            self.handle(request);
        }
        Ok(())
    }

    fn handle(&self, request: Request) {
        let method = request.method().clone();
        if !self.allowed_origin(&request) {
            return respond(
                request,
                StatusCode(404),
                "application/json",
                not_found().to_string(),
                method == Method::Head,
                None,
                ResponseKind::Plain,
            );
        }
        let url = request.url().to_string();
        let (path, query) = match url.split_once('?') {
            Some((path, query)) => (path, Some(query)),
            None => (url.as_str(), None),
        };
        if method == Method::Post && path == "/memex/capture" && query.is_none() {
            return self.handle_capture(request);
        }
        if method == Method::Post && query.is_none() {
            if let Some(job_id) = path.strip_prefix("/memex/capture/retry/") {
                return self.handle_capture_action(request, job_id, false);
            }
            if let Some(job_id) = path.strip_prefix("/memex/capture/approve/") {
                return self.handle_capture_action(request, job_id, true);
            }
        }
        if !matches!(method, Method::Get | Method::Head) {
            return respond(request, StatusCode(405), "application/json", json!({"schema_version":SCHEMA_VERSION,"error":{"code":"method_not_allowed","message":"method not allowed"}}).to_string(), false, Some("GET, HEAD"), ResponseKind::Plain);
        }
        if method == Method::Get && query.is_none() {
            if let Some(job_id) = path.strip_prefix("/memex/capture/status/") {
                return self.handle_capture_status(request, job_id);
            }
        }
        if let Some((item_id, filename)) = parse_item_attachment_path(path) {
            let result = if query.is_some() {
                Ok(None)
            } else {
                memex_store::read_item_attachment_at_sensitivity(
                    &self.config.library,
                    item_id,
                    &format!("attachments/{filename}"),
                    self.config.max_sensitivity,
                )
            };
            return match result {
                Ok(Some(attachment)) => respond_bytes(
                    request,
                    StatusCode(200),
                    &attachment.media_type,
                    attachment.bytes,
                    method == Method::Head,
                    None,
                    ResponseKind::Plain,
                ),
                Ok(None) => respond(
                    request,
                    StatusCode(404),
                    "application/json",
                    not_found().to_string(),
                    method == Method::Head,
                    None,
                    ResponseKind::Plain,
                ),
                Err(_) => respond(
                    request,
                    StatusCode(500),
                    "application/json",
                    json!({"schema_version":SCHEMA_VERSION,"error":{"code":"internal_error","message":"internal server error"}}).to_string(),
                    method == Method::Head,
                    None,
                    ResponseKind::Plain,
                ),
            };
        }
        if query.is_none()
            && (path == "/assets/mobile-library-v0.css" || path == "/assets/mobile-library-v0.js")
        {
            let (mime, body) = if path.ends_with("css") {
                ("text/css; charset=utf-8", CSS)
            } else {
                ("text/javascript; charset=utf-8", JS)
            };
            return respond(
                request,
                StatusCode(200),
                mime,
                body.into(),
                method == Method::Head,
                None,
                ResponseKind::Plain,
            );
        }
        let result = self.route(path, query);
        match result {
            Ok((kind, body)) => respond(request, StatusCode(200), if kind == ResponseKind::Plain { "application/json" } else { "text/html; charset=utf-8" }, body, method == Method::Head, None, kind),
            Err(RouteError::NotFound) => respond(request, StatusCode(404), "application/json", not_found().to_string(), method == Method::Head, None, ResponseKind::Plain),
            Err(RouteError::Internal) => respond(request, StatusCode(500), "application/json", json!({"schema_version":SCHEMA_VERSION,"error":{"code":"internal_error","message":"internal server error"}}).to_string(), method == Method::Head, None, ResponseKind::Plain),
        }
    }

    fn handle_capture(&self, mut request: Request) {
        let Some(socket) = self.agent_socket.as_ref() else {
            return capture_error(
                request,
                StatusCode(405),
                "method_not_allowed",
                Some("GET, HEAD"),
            );
        };
        let Some(content_type) = exact_header(&request, "Content-Type") else {
            return capture_error(request, StatusCode(400), "invalid_request", None);
        };
        let Some(idempotency_key) = exact_header(&request, "Idempotency-Key").map(str::to_owned)
        else {
            return capture_error(request, StatusCode(400), "invalid_request", None);
        };
        if content_type != "application/json" || !valid_opaque_id(&idempotency_key) {
            return capture_error(request, StatusCode(400), "invalid_request", None);
        }
        let Some(content_length) = exact_header(&request, "Content-Length") else {
            return capture_error(request, StatusCode(400), "invalid_request", None);
        };
        let Some(content_length) = content_length.parse::<u64>().ok().filter(|length| {
            *length > 0
                && *length <= MAX_CAPTURE_BODY
                && content_length == length.to_string()
                && header_count(&request, "Transfer-Encoding") == 0
                && request.body_length() == Some(*length as usize)
        }) else {
            return capture_error(request, StatusCode(400), "invalid_request", None);
        };
        let mut body = Vec::new();
        if request
            .as_reader()
            .take(content_length + 1)
            .read_to_end(&mut body)
            .is_err()
            || body.len() as u64 != content_length
        {
            return capture_error(request, StatusCode(400), "invalid_request", None);
        }
        let Ok(capture) = serde_json::from_slice::<CaptureRequest>(&body) else {
            return capture_error(request, StatusCode(400), "invalid_request", None);
        };
        if capture.schema_version != SCHEMA_VERSION || capture.command != "capture_public_url" {
            return capture_error(request, StatusCode(400), "invalid_request", None);
        }
        let Ok((url, redacted_url)) = memex_agent::extract_url(&capture.text) else {
            return capture_error(request, StatusCode(400), "invalid_request", None);
        };
        let response = match memex_agent::send_command(
            socket,
            &memex_agent::Command::Enqueue {
                producer: memex_agent::Producer::LocalSurface,
                producer_job_id: idempotency_key,
                url,
            },
        ) {
            Ok(response) => response,
            Err(_) => return capture_error(request, StatusCode(503), "daemon_unavailable", None),
        };
        if !response.ok {
            let conflict = response.reason_code.as_deref() == Some("request_identity_conflict");
            return capture_error(
                request,
                if conflict {
                    StatusCode(409)
                } else {
                    StatusCode(400)
                },
                if conflict {
                    "idempotency_conflict"
                } else {
                    "invalid_request"
                },
                None,
            );
        }
        let Some(job) = response.job else {
            return capture_error(request, StatusCode(500), "internal_error", None);
        };
        respond(
            request,
            StatusCode(202),
            "application/json",
            json!({
                "schema_version": SCHEMA_VERSION,
                "job_id": job.job_id,
                "state": capture_state(&job),
                "redacted_url": redacted_url,
            })
            .to_string(),
            false,
            None,
            ResponseKind::Plain,
        );
    }

    fn handle_capture_status(&self, request: Request, job_id: &str) {
        let Some(socket) = self.agent_socket.as_ref() else {
            return capture_error(request, StatusCode(404), "not_found", None);
        };
        if !valid_opaque_id(job_id) {
            return capture_error(request, StatusCode(404), "not_found", None);
        }
        let response = match memex_agent::send_command(
            socket,
            &memex_agent::Command::Status {
                job_id: job_id.to_owned(),
            },
        ) {
            Ok(response) => response,
            Err(_) => return capture_error(request, StatusCode(503), "daemon_unavailable", None),
        };
        if !response.ok {
            return capture_error(
                request,
                if response.reason_code.as_deref() == Some("job_not_found") {
                    StatusCode(404)
                } else {
                    StatusCode(500)
                },
                "not_found",
                None,
            );
        }
        let Some(job) = response.job else {
            return capture_error(request, StatusCode(500), "internal_error", None);
        };
        respond(
            request,
            StatusCode(200),
            "application/json",
            capture_status_json(&job).to_string(),
            false,
            None,
            ResponseKind::Plain,
        );
    }

    fn handle_capture_action(&self, request: Request, job_id: &str, approve: bool) {
        let Some(socket) = self.agent_socket.as_ref() else {
            return capture_error(request, StatusCode(404), "not_found", None);
        };
        if !valid_opaque_id(job_id)
            || exact_header(&request, "Content-Length") != Some("0")
            || header_count(&request, "Transfer-Encoding") != 0
            || request.body_length() != Some(0)
        {
            return capture_error(request, StatusCode(400), "invalid_request", None);
        }
        let command = if approve {
            memex_agent::Command::Approve {
                job_id: job_id.to_owned(),
            }
        } else {
            memex_agent::Command::Retry {
                job_id: job_id.to_owned(),
            }
        };
        let response = match memex_agent::send_command(socket, &command) {
            Ok(response) => response,
            Err(_) => return capture_error(request, StatusCode(503), "daemon_unavailable", None),
        };
        if !response.ok {
            return capture_error(
                request,
                if response.reason_code.as_deref() == Some("job_not_found") {
                    StatusCode(404)
                } else {
                    StatusCode(409)
                },
                if approve {
                    "approval_rejected"
                } else {
                    "retry_rejected"
                },
                None,
            );
        }
        let Some(job) = response.job else {
            return capture_error(request, StatusCode(500), "internal_error", None);
        };
        respond(
            request,
            StatusCode(202),
            "application/json",
            capture_status_json(&job).to_string(),
            false,
            None,
            ResponseKind::Plain,
        );
    }

    fn allowed_origin(&self, request: &Request) -> bool {
        let local = [
            (format!("127.0.0.1:{}", self.port), "http"),
            (format!("localhost:{}", self.port), "http"),
        ];
        let headers = |name| {
            request
                .headers()
                .iter()
                .filter(|h| h.field.equiv(name))
                .collect::<Vec<_>>()
        };
        let hosts = headers("Host");
        let expected_origin = match hosts.as_slice() {
            [header] => {
                let host = header.value.as_str().to_ascii_lowercase();
                let local_scheme = self
                    .lan_origin
                    .is_none()
                    .then(|| {
                        local
                            .iter()
                            .find(|(allowed, _)| allowed == &host)
                            .map(|(_, scheme)| *scheme)
                    })
                    .flatten();
                if let Some(scheme) = local_scheme {
                    format!("{scheme}://{host}")
                } else if self
                    .lan_origin
                    .as_ref()
                    .is_some_and(|origin| origin.strip_prefix("http://") == Some(host.as_str()))
                {
                    format!("http://{host}")
                } else if let Some(portal) = self
                    .orb_portal
                    .as_ref()
                    .filter(|portal| portal.authority == host)
                {
                    portal.origin.clone()
                } else {
                    return false;
                }
            }
            _ => return false,
        };
        match headers("Origin").as_slice() {
            [] => request.method() != &Method::Post,
            [header] => header.value.as_str() == expected_origin,
            _ => false,
        }
    }

    fn route(
        &self,
        path: &str,
        query: Option<&str>,
    ) -> std::result::Result<(ResponseKind, String), RouteError> {
        if path.starts_with("/api/v2/") {
            return Ok((ResponseKind::Plain, self.api_v2(path, query)?.to_string()));
        }
        if matches!(path, "/library" | "/memex" | "/capture") {
            return Ok((ResponseKind::Mobile, self.library_page(query, None)?));
        }
        if let Some(id) = path
            .strip_prefix("/library/items/")
            .or_else(|| path.strip_prefix("/memex/items/"))
            .and_then(|v| v.strip_suffix("/read"))
        {
            if id.is_empty() || id.contains('/') || query.is_some_and(invalid_percent_encoding) {
                return Err(RouteError::NotFound);
            }
            return Ok((ResponseKind::Mobile, self.reader_page(id, query)?));
        }
        let (api, route) = if path.strip_prefix("/api/v1/").is_some() {
            (true, &path[7..])
        } else if path.starts_with("/api/v1") {
            return Err(RouteError::NotFound);
        } else {
            (false, path)
        };
        let (base, detail) = parse_route(route)?;
        if base != "/recall" && query.is_some() {
            return Err(RouteError::NotFound);
        }
        let data = match (base, detail) {
            ("/health", None) if api => json!({"read_only":true,"loopback_only":true}),
            ("/catalog", None) if api => self.catalog()?,
            ("/", None) if !api => self.catalog()?,
            ("/sources", None) => to_value(self.sources()?)?,
            ("/sources", Some(id)) => to_value(self.source(&id)?)?,
            ("/items", None) => to_value(
                memex_store::list_items_at_sensitivity(
                    &self.config.library,
                    self.config.max_sensitivity,
                )
                .map_err(|_| RouteError::Internal)?
                .into_iter()
                .map(item_dto)
                .collect::<Vec<_>>(),
            )?,
            ("/items", Some(id)) => to_value(item_dto(
                memex_store::read_item_at_sensitivity(
                    &self.config.library,
                    &id,
                    self.config.max_sensitivity,
                )
                .map_err(|_| RouteError::Internal)?
                .ok_or(RouteError::NotFound)?,
            ))?,
            ("/analyses", None) => to_value(self.analyses()?)?,
            ("/analyses", Some(id)) => to_value(self.analysis(&id)?)?,
            ("/publications", None) => to_value(self.publications()?)?,
            ("/publications", Some(id)) => to_value(self.publication(&id)?)?,
            ("/public-releases", Some(id)) => to_value(
                memex_publication::read_public_projection_optional(&self.config.library, &id)
                    .map_err(|_| RouteError::Internal)?
                    .ok_or(RouteError::NotFound)?,
            )?,
            ("/recall", None) => self.recall(query.ok_or(RouteError::NotFound)?)?,
            _ => return Err(RouteError::NotFound),
        };
        if api {
            Ok((
                ResponseKind::Plain,
                envelope(self.config.max_sensitivity, data).to_string(),
            ))
        } else {
            Ok((ResponseKind::Legacy, html_page(path, &data)))
        }
    }

    fn catalog(&self) -> std::result::Result<Value, RouteError> {
        let sources = self.sources()?;
        let items = memex_store::list_items_at_sensitivity(
            &self.config.library,
            self.config.max_sensitivity,
        )
        .map_err(|_| RouteError::Internal)?;
        let analyses = self.analyses()?;
        let publications = self.publications()?;
        Ok(
            json!({"schema_version":SCHEMA_VERSION,"surface_version":SURFACE_VERSION,"max_sensitivity":self.config.max_sensitivity,"source_count":sources.len(),"item_count":items.len(),"analysis_count":analyses.len(),"publication_count":publications.len()}),
        )
    }
    fn sources(&self) -> std::result::Result<Vec<SourceDto>, RouteError> {
        Ok(memex_registry::list_current(&self.config.library)
            .map_err(|_| RouteError::Internal)?
            .into_iter()
            .filter(|r| r.source_profile.default_sensitivity <= self.config.max_sensitivity)
            .map(source_dto)
            .collect())
    }
    fn source(&self, id: &str) -> std::result::Result<SourceDto, RouteError> {
        let r = memex_registry::read_current_optional(&self.config.library, id)
            .map_err(|_| RouteError::Internal)?
            .ok_or(RouteError::NotFound)?;
        if r.source_profile.default_sensitivity <= self.config.max_sensitivity {
            Ok(source_dto(r))
        } else {
            Err(RouteError::NotFound)
        }
    }
    fn analyses(&self) -> std::result::Result<Vec<AnalysisDto>, RouteError> {
        Ok(memex_analysis::list_records_at_sensitivity(
            &self.config.library,
            self.config.max_sensitivity,
        )
        .map_err(|_| RouteError::Internal)?
        .into_iter()
        .map(analysis_dto)
        .collect())
    }
    fn analysis(&self, id: &str) -> std::result::Result<AnalysisDto, RouteError> {
        Ok(analysis_dto(
            memex_analysis::read_record_at_sensitivity(
                &self.config.library,
                id,
                self.config.max_sensitivity,
            )
            .map_err(|_| RouteError::Internal)?
            .ok_or(RouteError::NotFound)?,
        ))
    }

    fn generated_text_for_item(
        &self,
        item_id: &str,
        revision: &memex_core::ItemRevision,
    ) -> std::result::Result<ItemGeneratedText, RouteError> {
        let summary_display = self.summary_display(item_id, &revision.revision_id);
        let records = memex_analysis::list_records_at_sensitivity(
            &self.config.library,
            self.config.max_sensitivity,
        );
        let Ok(records) = records else {
            return Ok(ItemGeneratedText::default());
        };
        generated_text_from_records(item_id, revision, &records, &summary_display)
    }

    fn summary_display(&self, item_id: &str, revision_id: &str) -> SummaryDisplay {
        match memex_analysis::summary_status(&self.config.library, item_id, revision_id) {
            Ok(Some(view)) => {
                let validated = if view.state == memex_analysis::SummaryJobState::Succeeded {
                    match memex_analysis::validated_summary(
                        &self.config.library,
                        item_id,
                        revision_id,
                    ) {
                        Ok(summary) => summary,
                        Err(_) => return SummaryDisplay::Unavailable,
                    }
                } else {
                    None
                };
                SummaryDisplay::Job {
                    view: Box::new(view),
                    validated,
                }
            }
            Ok(None) => SummaryDisplay::None,
            Err(_) => SummaryDisplay::Unavailable,
        }
    }

    fn translation_display(&self, item_id: &str, revision_id: &str) -> TranslationDisplay {
        match memex_analysis::translation_status(&self.config.library, item_id, revision_id) {
            Ok(Some(view)) => {
                let validated_pair =
                    if view.state == memex_analysis::TranslationJobState::Succeeded {
                        match memex_analysis::validated_translation_pair(
                            &self.config.library,
                            item_id,
                            revision_id,
                        ) {
                            Ok(pair) => pair,
                            Err(_) => return TranslationDisplay::Unavailable,
                        }
                    } else {
                        None
                    };
                TranslationDisplay::Job {
                    view: Box::new(view),
                    validated_pair,
                }
            }
            Ok(None) => TranslationDisplay::None,
            Err(_) => TranslationDisplay::Unavailable,
        }
    }

    fn publications(&self) -> std::result::Result<Vec<PublicationDto>, RouteError> {
        Ok(memex_publication::list_publications_at_sensitivity(
            &self.config.library,
            self.config.max_sensitivity,
        )
        .map_err(|_| RouteError::Internal)?
        .into_iter()
        .map(publication_dto)
        .collect())
    }
    fn publication(&self, id: &str) -> std::result::Result<PublicationDto, RouteError> {
        Ok(publication_dto(
            memex_publication::read_publication_at_sensitivity(
                &self.config.library,
                id,
                self.config.max_sensitivity,
            )
            .map_err(|_| RouteError::Internal)?
            .ok_or(RouteError::NotFound)?,
        ))
    }

    fn api_v2(&self, path: &str, query: Option<&str>) -> std::result::Result<Value, RouteError> {
        let mobile = load_mobile_index(&self.config).map_err(|_| RouteError::Internal)?;
        if !mobile.available {
            return Err(RouteError::Internal);
        }
        let data = match path {
            "/api/v2/items" => {
                let q = parse_query(query, &["limit", "cursor", "tag"])?;
                let limit = parse_limit(q.get("limit"))?;
                let tag = q.get("tag");
                if tag.is_some_and(|selected| {
                    selected.is_empty() || !mobile.tags.iter().any(|facet| &facet.tag == selected)
                }) {
                    return Err(RouteError::NotFound);
                }
                let rows: Vec<_> = mobile
                    .items
                    .iter()
                    .filter(|r| tag.is_none_or(|t| r.manual_tags.contains(t)))
                    .collect();
                let start = cursor_start(&rows, q.get("cursor"), |r| &r.item_id)?;
                let items: Vec<_> = rows.iter().skip(start).take(limit).cloned().collect();
                let next_cursor = (start + items.len() < rows.len())
                    .then(|| items.last().unwrap().item_id.clone());
                json!({"items":items,"next_cursor":next_cursor,"visible_count":rows.len()})
            }
            "/api/v2/tags" => {
                let q = parse_query(query, &["limit", "cursor"])?;
                let limit = parse_limit(q.get("limit"))?;
                let start = cursor_start(&mobile.tags, q.get("cursor"), |facet| &facet.tag)?;
                let page: Vec<_> = mobile
                    .tags
                    .iter()
                    .skip(start)
                    .take(limit)
                    .cloned()
                    .collect();
                let next_cursor = (start + page.len() < mobile.tags.len())
                    .then(|| mobile.tags[start + page.len() - 1].tag.clone());
                json!({"tags":page,"next_cursor":next_cursor})
            }
            _ if path.starts_with("/api/v2/items/") => {
                if query.is_some() {
                    return Err(RouteError::NotFound);
                }
                let tail = &path[14..];
                let (id, kind) = tail.rsplit_once('/').ok_or(RouteError::NotFound)?;
                if id.is_empty() || id.contains('/') {
                    return Err(RouteError::NotFound);
                }
                match kind {
                    "metadata" => to_value(
                        memex_store::read_item_metadata_at_sensitivity(
                            &self.config.library,
                            id,
                            self.config.max_sensitivity,
                        )
                        .map_err(|_| RouteError::Internal)?
                        .ok_or(RouteError::NotFound)?,
                    )?,
                    "content" => {
                        let (item, rev) = memex_store::read_item_at_sensitivity(
                            &self.config.library,
                            id,
                            self.config.max_sensitivity,
                        )
                        .map_err(|_| RouteError::Internal)?
                        .ok_or(RouteError::NotFound)?;
                        json!({"item_id":item.item_id,"revision_id":rev.revision_id,"title":rev.title,"markdown":rev.content_markdown})
                    }
                    "summary" => {
                        let (item, revision) = memex_store::read_item_at_sensitivity(
                            &self.config.library,
                            id,
                            self.config.max_sensitivity,
                        )
                        .map_err(|_| RouteError::Internal)?
                        .ok_or(RouteError::NotFound)?;
                        let summary = memex_analysis::summary_status(
                            &self.config.library,
                            id,
                            &revision.revision_id,
                        )
                        .map_err(|_| RouteError::Internal)?;
                        json!({"item_id":item.item_id,"revision_id":revision.revision_id,"summary":summary})
                    }
                    "translation" => {
                        let (item, revision) = memex_store::read_item_at_sensitivity(
                            &self.config.library,
                            id,
                            self.config.max_sensitivity,
                        )
                        .map_err(|_| RouteError::Internal)?
                        .ok_or(RouteError::NotFound)?;
                        let translation = memex_analysis::translation_status(
                            &self.config.library,
                            id,
                            &revision.revision_id,
                        )
                        .map_err(|_| RouteError::Internal)?;
                        json!({"item_id":item.item_id,"revision_id":revision.revision_id,"translation":translation})
                    }
                    _ => return Err(RouteError::NotFound),
                }
            }
            _ => return Err(RouteError::NotFound),
        };
        Ok(
            json!({"schema_version":SCHEMA_VERSION,"surface_version":MOBILE_SURFACE_VERSION,"max_sensitivity":self.config.max_sensitivity,"data":data}),
        )
    }

    fn library_page(
        &self,
        query: Option<&str>,
        _: Option<&str>,
    ) -> std::result::Result<String, RouteError> {
        let mobile = load_mobile_index(&self.config).map_err(|_| RouteError::Internal)?;
        if !mobile.available {
            return Err(RouteError::Internal);
        }
        let q = parse_query(
            query,
            &["limit", "cursor", "tag", "detail", "theme", "font"],
        )?;
        let limit = parse_limit(q.get("limit"))?;
        validate_pref(q.get("theme"), &["light", "dark"])?;
        validate_pref(q.get("font"), &["sans", "serif"])?;
        let tag = q.get("tag");
        if tag.is_some_and(|selected| {
            selected.is_empty() || !mobile.tags.iter().any(|facet| &facet.tag == selected)
        }) {
            return Err(RouteError::NotFound);
        }
        let rows: Vec<_> = mobile
            .items
            .iter()
            .filter(|r| tag.is_none_or(|t| r.manual_tags.contains(t)))
            .collect();
        let start = cursor_start(&rows, q.get("cursor"), |r| &r.item_id)?;
        let page_rows = rows
            .iter()
            .skip(start)
            .take(limit)
            .copied()
            .collect::<Vec<_>>();
        let detail = match q.get("detail") {
            Some(id) => {
                let metadata = memex_store::read_item_metadata_at_sensitivity(
                    &self.config.library,
                    id,
                    self.config.max_sensitivity,
                )
                .map_err(|_| RouteError::Internal)?
                .ok_or(RouteError::NotFound)?;
                let (_, revision) = memex_store::read_item_at_sensitivity(
                    &self.config.library,
                    id,
                    self.config.max_sensitivity,
                )
                .map_err(|_| RouteError::Internal)?
                .ok_or(RouteError::NotFound)?;
                if metadata.revision_id != revision.revision_id {
                    return Err(RouteError::Internal);
                }
                Some((metadata, revision))
            }
            None => None,
        };
        let theme = q.get("theme").map(String::as_str);
        let font = q.get("font").map(String::as_str);
        let cursor = q.get("cursor").map(String::as_str);
        let mut list = String::new();
        for r in &page_rows {
            let info = [
                r.creators.join(", "),
                r.published_date
                    .clone()
                    .or_else(|| r.captured_at.clone())
                    .unwrap_or_default(),
                r.site_name.clone().unwrap_or_default(),
                r.item_type.clone(),
            ]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
            let metadata_url = query_href(
                "/library",
                &[
                    ("limit", Some(limit.to_string().as_str())),
                    ("cursor", cursor),
                    ("tag", tag.map(String::as_str)),
                    ("detail", Some(&r.item_id)),
                    ("theme", theme),
                    ("font", font),
                ],
            );
            let reader_url = reader_href(&r.item_id, theme, font, None);
            let selected = detail
                .as_ref()
                .is_some_and(|(metadata, _)| metadata.item_id == r.item_id);
            list.push_str(&format!(
                "<a class=\"row{}\" href=\"{}\" data-read-url=\"{}\"><span class=\"type\" aria-hidden=\"true\">{}</span><span><strong class=\"title\">{}</strong><small class=\"meta\">{}</small></span></a>",
                if selected { " selected" } else { "" },
                escape(&metadata_url),
                escape(&reader_url),
                item_type_icon(&r.item_type),
                escape(&r.title),
                escape(&info)
            ));
        }
        if list.is_empty() {
            list = "<p class=\"empty\">这里还没有资料。</p>".into()
        }
        let all_items_url = query_href(
            "/library",
            &[
                ("limit", Some(limit.to_string().as_str())),
                ("theme", theme),
                ("font", font),
            ],
        );
        let tag_html = mobile
            .tags
            .iter()
            .map(|facet| {
                let tag_url = query_href(
                    "/library",
                    &[
                        ("limit", Some(limit.to_string().as_str())),
                        ("tag", Some(&facet.tag)),
                        ("theme", theme),
                        ("font", font),
                    ],
                );
                format!(
                    "<a{} href=\"{}\"><span>{}</span><small>{}</small></a>",
                    if tag == Some(&facet.tag) {
                        " class=\"active\""
                    } else {
                        ""
                    },
                    escape(&tag_url),
                    escape(&facet.tag),
                    facet.count
                )
            })
            .collect::<String>();
        let previous = if start == 0 {
            None
        } else {
            let previous_start = start.saturating_sub(limit);
            let previous_cursor = previous_start
                .checked_sub(1)
                .map(|position| rows[position].item_id.as_str());
            Some(query_href(
                "/library",
                &[
                    ("limit", Some(limit.to_string().as_str())),
                    ("cursor", previous_cursor),
                    ("tag", tag.map(String::as_str)),
                    ("theme", theme),
                    ("font", font),
                ],
            ))
        };
        let next = (start + page_rows.len() < rows.len()).then(|| {
            query_href(
                "/library",
                &[
                    ("limit", Some(limit.to_string().as_str())),
                    ("cursor", page_rows.last().map(|row| row.item_id.as_str())),
                    ("tag", tag.map(String::as_str)),
                    ("theme", theme),
                    ("font", font),
                ],
            )
        });
        let pager = format!(
            "<nav class=\"pager\" aria-label=\"分页\">{}<span>{}–{} / {}</span>{}</nav>",
            previous
                .as_ref()
                .map(|url| format!("<a href=\"{}\">上一页</a>", escape(url)))
                .unwrap_or_else(|| "<span></span>".into()),
            if page_rows.is_empty() { 0 } else { start + 1 },
            start + page_rows.len(),
            rows.len(),
            next.as_ref()
                .map(|url| format!("<a href=\"{}\">下一页</a>", escape(url)))
                .unwrap_or_else(|| "<span></span>".into())
        );
        let close_url = query_href(
            "/library",
            &[
                ("limit", Some(limit.to_string().as_str())),
                ("cursor", cursor),
                ("tag", tag.map(String::as_str)),
                ("theme", theme),
                ("font", font),
            ],
        );
        let sheet = match detail.as_ref() {
            Some((metadata, revision)) => {
                let generated = self.generated_text_for_item(&metadata.item_id, revision)?;
                let summary = self.summary_display(&metadata.item_id, &revision.revision_id);
                let translation =
                    self.translation_display(&metadata.item_id, &revision.revision_id);
                metadata_sheet(MetadataRenderInputs {
                    item: metadata,
                    generated: &generated,
                    summary: &summary,
                    translation: &translation,
                    close_url: &close_url,
                    route_prefix: "/library",
                    theme,
                    font,
                })
            }
            None => "<aside class=\"inspector\" aria-label=\"条目元数据\"><div class=\"inspector-empty\"><strong>条目信息</strong><p>选择一条资料查看元数据；正文只在点击“阅读全文”后加载。</p></div></aside>".into(),
        };
        let heading = tag
            .map(|selected| format!("标签：{}", escape(selected)))
            .unwrap_or_else(|| "全部资料".into());
        let current_theme = theme.unwrap_or("light");
        let theme_link = |value: &str, label: &str| {
            let url = query_href(
                "/library",
                &[
                    ("limit", Some(limit.to_string().as_str())),
                    ("cursor", cursor),
                    ("tag", tag.map(String::as_str)),
                    (
                        "detail",
                        detail.as_ref().map(|(item, _)| item.item_id.as_str()),
                    ),
                    ("theme", Some(value)),
                    ("font", font),
                ],
            );
            preference_anchor("theme", value, label, &url, current_theme == value)
        };
        let theme_switch = format!(
            "<nav class=\"theme-switch\" aria-label=\"日夜模式\">{}{}</nav>",
            theme_link("light", "日"),
            theme_link("dark", "夜")
        );
        let body = format!(
            "<header class=\"top\"><a class=\"brand\" href=\"{}\">MemeX</a><div class=\"search\" aria-label=\"搜索将在后续版本提供\"><span aria-hidden=\"true\">⌕</span><span>浏览资料库</span></div><div class=\"top-actions\"><span class=\"count\">{} 条</span>{}</div></header><div class=\"layout\"><aside class=\"sidebar\"><details open><summary>资料库</summary><nav class=\"nav\"><a{} href=\"{}\"><span>全部资料</span><small>{}</small></a></nav></details><section class=\"tags\"><h2>标签</h2>{}</section></aside><main class=\"list\" aria-label=\"资料列表\"><header class=\"list-heading\"><div><p>资料库</p><h1>{}</h1></div><small>单击查看元数据，双击或点“阅读全文”进入正文</small></header>{}{}{}</main>{}</div>",
            escape(&all_items_url),
            rows.len(),
            theme_switch,
            if tag.is_none() { " class=\"active\"" } else { "" },
            escape(&all_items_url),
            mobile.items.len(),
            tag_html,
            heading,
            capture_card(),
            list,
            pager,
            sheet
        );
        Ok(page_shell(q.get("theme"), q.get("font"), &body))
    }

    fn reader_page(
        &self,
        id: &str,
        query: Option<&str>,
    ) -> std::result::Result<String, RouteError> {
        let q = parse_query(query, &["theme", "font", "language"])?;
        validate_pref(q.get("theme"), &["light", "dark"])?;
        validate_pref(q.get("font"), &["sans", "serif"])?;
        validate_pref(q.get("language"), &["zh", "en", "zh-en", "en-zh"])?;
        let (_, r) = memex_store::read_item_at_sensitivity(
            &self.config.library,
            id,
            self.config.max_sensitivity,
        )
        .map_err(|_| RouteError::Internal)?
        .ok_or(RouteError::NotFound)?;
        let generated = self.generated_text_for_item(id, &r)?;
        let summary = self.summary_display(id, &r.revision_id);
        let translation = self.translation_display(id, &r.revision_id);
        render_reader_page(ReaderRenderInputs {
            id,
            revision: &r,
            generated: &generated,
            summary: &summary,
            translation: &translation,
            theme: q.get("theme").map(String::as_str),
            font: q.get("font").map(String::as_str),
            language: q.get("language").map(String::as_str),
            source_first: false,
            leading_html: "",
            route_prefix: "/library",
            asset_prefix: "/assets",
        })
    }

    fn recall(&self, query: &str) -> std::result::Result<Value, RouteError> {
        if invalid_percent_encoding(query) {
            return Err(RouteError::NotFound);
        }
        let mut q = None;
        let mut limit = None;
        for (key, value) in form_urlencoded::parse(query.as_bytes()).into_owned() {
            match key.as_str() {
                "q" if q.is_none() => q = Some(value),
                "limit" if limit.is_none() => {
                    limit = Some(value.parse::<usize>().map_err(|_| RouteError::NotFound)?)
                }
                _ => return Err(RouteError::NotFound),
            }
        }
        let limit = limit.unwrap_or(20);
        if !(1..=100).contains(&limit) {
            return Err(RouteError::NotFound);
        }
        let query = q.ok_or(RouteError::NotFound)?;
        let recall_query = memex_analysis::RecallQuery {
            query,
            max_sensitivity: self.config.max_sensitivity,
            limit,
        };
        memex_analysis::validate_recall_query(&recall_query).map_err(|_| RouteError::NotFound)?;
        to_value(
            memex_analysis::recall(&self.config.library, &recall_query)
                .map_err(|_| RouteError::Internal)?,
        )
    }
}

fn render_reader_page(input: ReaderRenderInputs<'_>) -> std::result::Result<String, RouteError> {
    let ReaderRenderInputs {
        id,
        revision,
        generated,
        summary,
        translation,
        theme,
        font,
        language,
        source_first,
        leading_html,
        route_prefix,
        asset_prefix,
    } = input;
    let library_url = query_href(route_prefix, &[("theme", theme), ("font", font)]);
    let source_language = generated.source_language.as_deref();
    let target_language = match source_language {
        Some("zh") => Some("en"),
        Some("en") => Some("zh"),
        _ => None,
    };
    let generated_pair = target_language.map(|target| {
        (
            unique_generated(&generated.title_translations, target),
            unique_generated(&generated.document_translations, target),
        )
    });
    let actual_bilingual_ready = source_language.is_some()
        && generated_pair.is_some_and(|(title, document)| {
            translation_allows_generated(translation, title, document)
        });
    let bilingual_ready = !source_first && actual_bilingual_ready;
    let selected_language = if source_first {
        "source"
    } else {
        language.or(source_language).unwrap_or("source")
    };
    let available_modes = if source_first {
        vec!["source"]
    } else {
        match (source_language, bilingual_ready) {
            (Some("zh"), true) => vec!["zh", "en", "zh-en", "en-zh"],
            (Some("en"), true) => vec!["en", "zh", "en-zh", "zh-en"],
            (Some(source), _) => vec![source],
            (None, _) => vec!["source"],
        }
    };
    if !available_modes.contains(&selected_language) {
        return Err(RouteError::NotFound);
    }
    let selected_languages = selected_language.split('-').collect::<Vec<_>>();
    let title_for = |language: &str| -> Option<(&str, Option<&GeneratedValue>)> {
        if source_language == Some(language) || language == "source" {
            Some((&revision.title, None))
        } else {
            unique_generated(&generated.title_translations, language)
                .map(|value| (value.body.as_str(), Some(value)))
        }
    };
    let document_for = |language: &str| -> Option<&str> {
        if source_language == Some(language) || language == "source" {
            Some(revision.content_markdown.as_str())
        } else {
            unique_generated(&generated.document_translations, language)
                .map(|value| value.body.as_str())
        }
    };
    let mut titles = String::new();
    let mut summaries = String::new();
    for (index, language) in selected_languages.iter().enumerate() {
        let (title, provenance) = title_for(language).ok_or(RouteError::NotFound)?;
        let language_label = reader_language_label(language);
        if index == 0 {
            titles.push_str(&format!(
                "<div lang=\"{}\"><small>{}</small><h1>{}</h1></div>",
                escape(language),
                language_label,
                escape(title)
            ));
        } else {
            titles.push_str(&format!(
                "<div class=\"alternate-title\" lang=\"{}\"><small>{}</small><p>{}</p></div>",
                escape(language),
                language_label,
                escape(title)
            ));
        }
        if let Some(provenance) = provenance {
            titles.push_str(&format!(
                "<span class=\"derivative-note\" data-analysis-id=\"{}\">AI 标题译文 · {}</span>",
                escape(&provenance.analysis_id),
                escape(&provenance.model_id)
            ));
        }
        if let Some(summary) = unique_generated(&generated.summaries, language) {
            summaries.push_str(&format!(
                "<section class=\"ai-summary\" lang=\"{}\" data-analysis-id=\"{}\"><p class=\"derivative-note\">AI 摘要 · {} · {}</p>{}",
                escape(language),
                escape(&summary.analysis_id),
                language_label,
                escape(&summary.model_id),
                render_markdown(&summary.body, None)
            ));
        }
    }
    let rendered = match selected_languages.as_slice() {
        [language] => render_markdown(
            document_for(language).ok_or(RouteError::NotFound)?,
            Some(id),
        ),
        [first, second] => render_interleaved_markdown(
            document_for(first).ok_or(RouteError::NotFound)?,
            document_for(second).ok_or(RouteError::NotFound)?,
            id,
            first,
            second,
        ),
        _ => return Err(RouteError::NotFound),
    };
    let preference = |name: &str, value: &str| {
        let (next_theme, next_font) = if name == "theme" {
            (Some(value), font)
        } else {
            (theme, Some(value))
        };
        let active = if name == "theme" {
            theme.unwrap_or("light") == value
        } else {
            font.unwrap_or("sans") == value
        };
        preference_anchor(
            name,
            value,
            match value {
                "light" => "日间",
                "dark" => "夜间",
                "sans" => "无衬线",
                "serif" => "衬线",
                _ => value,
            },
            &reader_href_at(
                route_prefix,
                id,
                next_theme,
                next_font,
                (selected_language != "source").then_some(selected_language),
            ),
            active,
        )
    };
    let language_controls = if bilingual_ready {
        format!(
            "<span><strong>语言</strong>{}</span>",
            available_modes
                .iter()
                .map(|mode| format!(
                    "<a{} href=\"{}\">{}</a>",
                    if selected_language == *mode {
                        " class=\"active\" aria-current=\"true\""
                    } else {
                        ""
                    },
                    escape(&reader_href_at(route_prefix, id, theme, font, Some(mode))),
                    reader_mode_label(mode)
                ))
                .collect::<String>()
        )
    } else {
        String::new()
    };
    let controls = format!(
        "<nav class=\"controls\" aria-label=\"阅读设置\"><a class=\"back\" href=\"{}\">← 资料库</a>{}<span><strong>模式</strong>{}{}</span><span><strong>字体</strong>{}{}</span></nav>",
        escape(&library_url),
        language_controls,
        preference("theme", "light"),
        preference("theme", "dark"),
        preference("font", "sans"),
        preference("font", "serif")
    );
    let translation_status =
        translation_status_html(translation, actual_bilingual_ready, &revision.revision_id);
    let summary_ready = generated.summaries.values().any(Option::is_some);
    let summary_status = summary_status_html(summary, summary_ready, &revision.revision_id);
    Ok(page_shell_at(
        theme.map(str::to_owned).as_ref(),
        font.map(str::to_owned).as_ref(),
        &format!(
            "{}{}<article class=\"reader\"><header><p>Markdown · 原文为准 · AI 衍生可追溯</p>{}{}{}{}</header>{}</article>",
            leading_html,
            controls,
            translation_status,
            summary_status,
            titles,
            summaries,
            rendered
        ),
        asset_prefix,
    ))
}

#[derive(Debug)]
enum RouteError {
    NotFound,
    Internal,
}
fn language_family(value: &str) -> Option<&'static str> {
    let value = value.to_ascii_lowercase();
    if value == "en" || value.starts_with("en-") {
        Some("en")
    } else if value == "zh" || value.starts_with("zh-") {
        Some("zh")
    } else {
        None
    }
}
fn insert_unique_generated(
    values: &mut BTreeMap<String, Option<GeneratedValue>>,
    language: &str,
    value: GeneratedValue,
) {
    if values.contains_key(language) {
        values.insert(language.to_owned(), None);
    } else {
        values.insert(language.to_owned(), Some(value));
    }
}
fn unique_generated<'a>(
    values: &'a BTreeMap<String, Option<GeneratedValue>>,
    language: &str,
) -> Option<&'a GeneratedValue> {
    values.get(language).and_then(Option::as_ref)
}
fn reader_language_label(language: &str) -> &'static str {
    match language {
        "zh" => "中文",
        "en" => "English",
        _ => "原文",
    }
}
fn reader_mode_label(mode: &str) -> &'static str {
    match mode {
        "zh" => "中文",
        "en" => "English",
        "zh-en" => "中/英",
        "en-zh" => "英/中",
        _ => "原文",
    }
}
fn invalid_percent_encoding(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len()
                || !bytes[i + 1].is_ascii_hexdigit()
                || !bytes[i + 2].is_ascii_hexdigit()
            {
                return true;
            }
            let high = (bytes[i + 1] as char).to_digit(16).expect("checked") as u8;
            let low = (bytes[i + 2] as char).to_digit(16).expect("checked") as u8;
            decoded.push((high << 4) | low);
            i += 3;
        } else {
            decoded.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
            i += 1;
        }
    }
    std::str::from_utf8(&decoded).is_err()
}
fn index_cost(r: &LibraryItem) -> usize {
    std::mem::size_of::<LibraryItem>()
        + r.item_id.capacity()
        + r.revision_id.capacity()
        + r.item_type.capacity()
        + r.title.capacity()
        + r.creators.capacity() * std::mem::size_of::<String>()
        + r.creators.iter().map(String::capacity).sum::<usize>()
        + r.published_date.as_ref().map_or(0, String::capacity)
        + r.captured_at.as_ref().map_or(0, String::capacity)
        + r.site_name.as_ref().map_or(0, String::capacity)
        + r.manual_tags.capacity() * std::mem::size_of::<String>()
        + r.manual_tags.iter().map(String::capacity).sum::<usize>()
}
fn retained_index_cost(
    rows: &[LibraryItem],
    row_capacity: usize,
    tags: &[TagFacet],
    tag_capacity: usize,
) -> Result<usize> {
    let row_slots = row_capacity
        .checked_mul(std::mem::size_of::<LibraryItem>())
        .ok_or_else(|| anyhow!("mobile library index budget exceeded"))?;
    let rows_cost = rows.iter().try_fold(row_slots, |total, row| {
        total
            .checked_add(index_cost(row) - std::mem::size_of::<LibraryItem>())
            .ok_or_else(|| anyhow!("mobile library index budget exceeded"))
    })?;
    let tag_slots = tag_capacity
        .checked_mul(std::mem::size_of::<TagFacet>())
        .and_then(|cost| rows_cost.checked_add(cost))
        .ok_or_else(|| anyhow!("mobile library index budget exceeded"))?;
    tags.iter().try_fold(tag_slots, |total, facet| {
        total
            .checked_add(facet.tag.capacity())
            .ok_or_else(|| anyhow!("mobile library index budget exceeded"))
    })
}
fn reserve_index_budget(retained: &mut usize, cost: usize) -> Result<()> {
    *retained = retained
        .checked_add(cost)
        .ok_or_else(|| anyhow!("mobile library index budget exceeded"))?;
    if *retained > INDEX_BUDGET {
        return Err(anyhow!("mobile library index budget exceeded"));
    }
    Ok(())
}
fn parse_query(
    query: Option<&str>,
    allowed: &[&str],
) -> std::result::Result<BTreeMap<String, String>, RouteError> {
    let Some(raw) = query else {
        return Ok(BTreeMap::new());
    };
    if raw.is_empty() || raw.len() > 2048 || invalid_percent_encoding(raw) {
        return Err(RouteError::NotFound);
    }
    let mut out = BTreeMap::new();
    for (k, v) in form_urlencoded::parse(raw.as_bytes()).into_owned() {
        if !allowed.contains(&k.as_str()) || out.contains_key(&k) || v.len() > 512 {
            return Err(RouteError::NotFound);
        }
        out.insert(k, v);
    }
    Ok(out)
}
fn parse_limit(v: Option<&String>) -> std::result::Result<usize, RouteError> {
    let n = match v {
        Some(value)
            if !value.is_empty()
                && (value.len() == 1 || !value.starts_with('0'))
                && value.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            value.parse().map_err(|_| RouteError::NotFound)?
        }
        Some(_) => return Err(RouteError::NotFound),
        None => 50,
    };
    if !(1..=100).contains(&n) {
        Err(RouteError::NotFound)
    } else {
        Ok(n)
    }
}
fn cursor_start<T, F>(
    rows: &[T],
    cursor: Option<&String>,
    key: F,
) -> std::result::Result<usize, RouteError>
where
    F: Fn(&T) -> &str,
{
    match cursor {
        None => Ok(0),
        Some(c) if c.is_empty() => Err(RouteError::NotFound),
        Some(c) => rows
            .iter()
            .position(|r| key(r) == c)
            .map(|i| i + 1)
            .ok_or(RouteError::NotFound),
    }
}
fn validate_pref(v: Option<&String>, allowed: &[&str]) -> std::result::Result<(), RouteError> {
    if v.is_some_and(|x| !allowed.contains(&x.as_str())) {
        Err(RouteError::NotFound)
    } else {
        Ok(())
    }
}
fn query_href(path: &str, values: &[(&str, Option<&str>)]) -> String {
    let mut query = form_urlencoded::Serializer::new(String::new());
    for (name, value) in values {
        if let Some(value) = value {
            query.append_pair(name, value);
        }
    }
    let query = query.finish();
    if query.is_empty() {
        path.to_owned()
    } else {
        format!("{path}?{query}")
    }
}
fn reader_href(
    item_id: &str,
    theme: Option<&str>,
    font: Option<&str>,
    language: Option<&str>,
) -> String {
    reader_href_at("/library", item_id, theme, font, language)
}
fn reader_href_at(
    route_prefix: &str,
    item_id: &str,
    theme: Option<&str>,
    font: Option<&str>,
    language: Option<&str>,
) -> String {
    query_href(
        &format!("{route_prefix}/items/{item_id}/read"),
        &[("theme", theme), ("font", font), ("language", language)],
    )
}
fn preference_anchor(name: &str, value: &str, label: &str, href: &str, active: bool) -> String {
    format!(
        "<a{} data-pref=\"{}\" data-value=\"{}\" href=\"{}\"{}>{}</a>",
        if active { " class=\"active\"" } else { "" },
        escape(name),
        escape(value),
        escape(href),
        if active { " aria-current=\"true\"" } else { "" },
        escape(label)
    )
}
fn item_type_icon(item_type: &str) -> &'static str {
    match item_type {
        "book" | "book_section" => "▤",
        "journal_article" | "article" => "◫",
        "thesis" => "◇",
        "webpage" => "⌁",
        _ => "·",
    }
}
fn enum_label(value: &impl Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}
fn metadata_row(output: &mut String, label: &str, value: &str) {
    if !value.is_empty() {
        output.push_str(&format!(
            "<div><dt>{}</dt><dd>{}</dd></div>",
            escape(label),
            escape(value)
        ));
    }
}
fn metadata_sheet(input: MetadataRenderInputs<'_>) -> String {
    let MetadataRenderInputs {
        item,
        generated,
        summary,
        translation,
        close_url,
        route_prefix,
        theme,
        font,
    } = input;
    let metadata = &item.metadata;
    let creators = metadata
        .creators
        .as_ref()
        .map(|value| {
            value
                .value
                .iter()
                .map(|creator| format!("{} ({})", creator.display_name, enum_label(&creator.role)))
                .collect::<Vec<_>>()
                .join("，")
        })
        .unwrap_or_default();
    let identifiers = metadata
        .identifiers
        .as_ref()
        .map(|value| {
            value
                .value
                .iter()
                .map(|identifier| format!("{}: {}", enum_label(&identifier.kind), identifier.value))
                .collect::<Vec<_>>()
                .join("，")
        })
        .unwrap_or_default();
    let rights = metadata
        .content_rights
        .as_ref()
        .map(|value| {
            [
                enum_label(&value.value.status),
                value.value.statement.clone().unwrap_or_default(),
                value.value.license_url.clone().unwrap_or_default(),
            ]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" · ")
        })
        .unwrap_or_default();
    let mut fields = String::new();
    metadata_row(&mut fields, "类型", &item.item_type);
    metadata_row(&mut fields, "创建者", &creators);
    metadata_row(
        &mut fields,
        "发布日期",
        metadata
            .published_date
            .as_ref()
            .map(|value| value.value.as_str())
            .unwrap_or(""),
    );
    metadata_row(
        &mut fields,
        "采集时间",
        item.captured_at.as_deref().unwrap_or(""),
    );
    metadata_row(
        &mut fields,
        "语言",
        metadata
            .language
            .as_ref()
            .map(|value| value.value.as_str())
            .unwrap_or(""),
    );
    metadata_row(
        &mut fields,
        "来源",
        metadata
            .site_name
            .as_ref()
            .map(|value| value.value.as_str())
            .unwrap_or(""),
    );
    metadata_row(&mut fields, "标识符", &identifiers);
    metadata_row(
        &mut fields,
        "标签",
        &metadata
            .manual_tags
            .as_ref()
            .map(|value| value.value.join("，"))
            .unwrap_or_default(),
    );
    metadata_row(&mut fields, "版权", &rights);
    metadata_row(&mut fields, "原始网址", &item.source_url);
    metadata_row(
        &mut fields,
        "规范网址",
        item.canonical_url.as_deref().unwrap_or(""),
    );
    let abstract_text = metadata
        .abstract_text
        .as_ref()
        .map(|value| value.value.as_str())
        .unwrap_or("");
    let abstract_html = if abstract_text.is_empty() {
        String::new()
    } else {
        format!(
            "<section class=\"abstract\"><h3>摘要</h3><p>{}</p></section>",
            escape(abstract_text)
        )
    };
    let title_translation = unique_generated(&generated.title_translations, "zh");
    let document_translation = unique_generated(&generated.document_translations, "zh");
    let translation_ready = generated.source_language.as_deref() == Some("en")
        && translation_allows_generated(translation, title_translation, document_translation);
    let title_translations = if translation_ready {
        generated
            .title_translations
            .iter()
            .filter_map(|(language, value)| {
                value.as_ref().map(|value| {
                    format!(
                        "<div class=\"translated-metadata-title\" lang=\"{}\" data-analysis-id=\"{}\"><small>AI 标题译文 · {} · {}</small><p>{}</p></div>",
                        escape(language),
                        escape(&value.analysis_id),
                        reader_language_label(language),
                        escape(&value.model_id),
                        escape(&value.body)
                    )
                })
            })
            .collect::<String>()
    } else {
        String::new()
    };
    let summaries = generated
        .summaries
        .iter()
        .filter_map(|(language, value)| {
            value.as_ref().map(|value| {
                format!(
                    "<div lang=\"{}\" data-analysis-id=\"{}\"><small>AI 摘要 · {} · {}</small>{}</div>",
                    escape(language),
                    escape(&value.analysis_id),
                    reader_language_label(language),
                    escape(&value.model_id),
                    render_markdown(&value.body, None)
                )
            })
        })
        .collect::<String>();
    let generated_html = if summaries.is_empty() {
        String::new()
    } else {
        format!("<section class=\"generated-metadata\">{summaries}</section>")
    };
    let translation_status =
        translation_status_html(translation, translation_ready, &item.revision_id);
    let summary_ready = generated.summaries.values().any(Option::is_some);
    let summary_status = summary_status_html(summary, summary_ready, &item.revision_id);
    format!(
        "<aside class=\"inspector sheet\" aria-label=\"条目元数据\"><header><p>条目信息</p><a class=\"close\" href=\"{}\" aria-label=\"关闭条目信息\">×</a><h2>{}</h2></header>{}{}{}<a class=\"read\" href=\"{}\">阅读全文 <span aria-hidden=\"true\">→</span></a><dl>{}</dl>{}{}</aside>",
        escape(close_url),
        escape(&item.title),
        translation_status,
        summary_status,
        title_translations,
        escape(&reader_href_at(route_prefix, &item.item_id, theme, font, None)),
        fields,
        abstract_html,
        generated_html
    )
}

fn summary_allows_generated(summary: &SummaryDisplay, value: &GeneratedValue) -> bool {
    match summary {
        SummaryDisplay::None => value.summary_job_id.is_none(),
        SummaryDisplay::Job {
            view,
            validated: Some(validated),
        } => {
            view.state == memex_analysis::SummaryJobState::Succeeded
                && value.summary_job_id.as_deref() == Some(view.job_id.as_str())
                && validated.analysis_id == value.analysis_id
        }
        SummaryDisplay::Job { .. } | SummaryDisplay::Unavailable => false,
    }
}

fn summary_status_html(
    summary: &SummaryDisplay,
    generated_ready: bool,
    revision_id: &str,
) -> String {
    let (state, heading, message) = match summary {
        SummaryDisplay::None => return String::new(),
        SummaryDisplay::Job { view, .. }
            if view.state == memex_analysis::SummaryJobState::Queued =>
        {
            (
                "queued",
                "等待摘要",
                "摘要任务已排队；完成前不会显示未验证内容。",
            )
        }
        SummaryDisplay::Job { view, .. }
            if view.state == memex_analysis::SummaryJobState::Running =>
        {
            ("running", "正在生成摘要", "摘要完成前不会显示未验证内容。")
        }
        SummaryDisplay::Job { view, .. }
            if view.state == memex_analysis::SummaryJobState::Failed =>
        {
            (
                "failed",
                "摘要失败",
                if view.retryable {
                    "原文仍可阅读；此任务可安全重试。"
                } else {
                    "原文仍可阅读；摘要未通过安全校验。"
                },
            )
        }
        SummaryDisplay::Job { view, .. }
            if view.state == memex_analysis::SummaryJobState::Succeeded && generated_ready =>
        {
            (
                "succeeded",
                "摘要完成",
                "摘要已通过当前 revision 绑定校验。",
            )
        }
        SummaryDisplay::Job { .. } => (
            "failed",
            "摘要失败",
            "原文仍可阅读；摘要记录未通过完整性校验。",
        ),
        SummaryDisplay::Unavailable => (
            "failed",
            "摘要状态不可用",
            "原文仍可阅读；当前不会显示无法验证的摘要。",
        ),
    };
    format!(
        "<section class=\"translation-status summary-status\" data-summary-state=\"{}\" data-revision-id=\"{}\" role=\"status\" aria-live=\"polite\"><strong>{}</strong><p>{}</p></section>",
        state,
        escape(revision_id),
        heading,
        message
    )
}

fn translation_allows_generated(
    translation: &TranslationDisplay,
    title: Option<&GeneratedValue>,
    document: Option<&GeneratedValue>,
) -> bool {
    let Some((title, document)) = title.zip(document) else {
        return false;
    };
    match translation {
        TranslationDisplay::None => {
            title.translation_job_id.is_none() && document.translation_job_id.is_none()
        }
        TranslationDisplay::Job {
            view,
            validated_pair: Some(pair),
        } => {
            view.state == memex_analysis::TranslationJobState::Succeeded
                && title.translation_job_id.as_deref() == Some(view.job_id.as_str())
                && document.translation_job_id.as_deref() == Some(view.job_id.as_str())
                && pair.title_analysis_id == title.analysis_id
                && pair.document_analysis_id == document.analysis_id
        }
        TranslationDisplay::Job { .. } => false,
        TranslationDisplay::Unavailable => false,
    }
}

fn translation_status_html(
    translation: &TranslationDisplay,
    generated_ready: bool,
    revision_id: &str,
) -> String {
    let (state, heading, message) = match translation {
        TranslationDisplay::None => return String::new(),
        TranslationDisplay::Job { view: job, .. }
            if job.state == memex_analysis::TranslationJobState::Queued =>
        {
            ("queued", "等待翻译", "原文可继续阅读；翻译任务已排队。")
        }
        TranslationDisplay::Job { view: job, .. }
            if job.state == memex_analysis::TranslationJobState::Running =>
        {
            (
                "running",
                "正在翻译",
                "原文可继续阅读；完成前不会显示未验证的译文。",
            )
        }
        TranslationDisplay::Job { view: job, .. }
            if job.state == memex_analysis::TranslationJobState::Failed =>
        {
            (
                "failed",
                "翻译失败",
                if job.retryable {
                    "原文仍可阅读；此任务可安全重试。"
                } else {
                    "原文仍可阅读；译文未通过安全校验。"
                },
            )
        }
        TranslationDisplay::Job { view: job, .. }
            if job.state == memex_analysis::TranslationJobState::Succeeded && generated_ready =>
        {
            (
                "succeeded",
                "翻译完成",
                "中文标题与全文译文已通过 revision 绑定校验。",
            )
        }
        TranslationDisplay::Job { .. } => (
            "failed",
            "翻译失败",
            "原文仍可阅读；译文记录未通过完整性校验。",
        ),
        TranslationDisplay::Unavailable => (
            "failed",
            "翻译状态不可用",
            "原文仍可阅读；当前不会显示无法验证的译文。",
        ),
    };
    format!(
        "<section class=\"translation-status\" data-translation-state=\"{}\" data-revision-id=\"{}\" role=\"status\" aria-live=\"polite\"><strong>{}</strong><p>{}</p></section>",
        state,
        escape(revision_id),
        heading,
        message
    )
}
fn page_shell(theme: Option<&String>, font: Option<&String>, body: &str) -> String {
    page_shell_at(theme, font, body, "/assets")
}
fn page_shell_at(
    theme: Option<&String>,
    font: Option<&String>,
    body: &str,
    asset_prefix: &str,
) -> String {
    format!("<!doctype html><html lang=\"zh-CN\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><meta name=\"color-scheme\" content=\"light dark\"><title>MemeX 资料库</title><link rel=\"stylesheet\" href=\"{asset_prefix}/mobile-library-v0.css\"><script defer src=\"{asset_prefix}/mobile-library-v0.js\"></script></head><body data-theme=\"{}\" data-font=\"{}\">{}</body></html>",escape(theme.map_or("light",String::as_str)),escape(font.map_or("sans",String::as_str)),body)
}
fn capture_card() -> String {
    format!(
        "<section class=\"capture-card\" data-homepage-version=\"{}\" data-capture-contract=\"memex.capture_command.v1\" aria-labelledby=\"capture-heading\"><p class=\"capture-eyebrow\">新增资料</p><h2 id=\"capture-heading\">抓取一篇文章</h2><p class=\"capture-help\" id=\"capture-help\">输入一句包含一个公开 HTTP(S) 链接的话。链接将由服务端重新验证。</p><form data-capture-form autocomplete=\"off\" novalidate><label for=\"capture-text\">文章链接或包含链接的话</label><textarea id=\"capture-text\" data-capture-input rows=\"4\" maxlength=\"16384\" autocomplete=\"off\" autocapitalize=\"off\" spellcheck=\"false\" aria-describedby=\"capture-help capture-status\"></textarea><button type=\"submit\" data-capture-submit>提交抓取</button><p id=\"capture-status\" class=\"capture-status\" data-capture-status role=\"status\" aria-live=\"polite\"></p></form></section>",
        MOBILE_HOMEPAGE_VERSION
    )
}
fn safe_link(v: &str) -> bool {
    v.starts_with('#')
        || (v.starts_with('/') && !v.starts_with("//"))
        || Url::parse(v).is_ok_and(|url| matches!(url.scheme(), "http" | "https" | "mailto"))
}
fn valid_attachment_filename(filename: &str) -> bool {
    let Some((digest, extension)) = filename
        .strip_prefix("sha256-")
        .and_then(|value| value.rsplit_once('.'))
    else {
        return false;
    };
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        && matches!(extension, "gif" | "jpg" | "png" | "webp")
}
fn parse_item_attachment_path(path: &str) -> Option<(&str, &str)> {
    let library_path = path
        .strip_prefix("/library/items/")
        .or_else(|| path.strip_prefix("/memex/items/"))?;
    let (item_id, filename) = library_path.split_once("/attachments/")?;
    (!item_id.is_empty()
        && !item_id.contains('/')
        && !invalid_percent_encoding(item_id)
        && valid_attachment_filename(filename))
    .then_some((item_id, filename))
}
fn local_image_url(item_id: &str, destination: &str) -> Option<String> {
    let filename = destination.strip_prefix("attachments/")?;
    valid_attachment_filename(filename)
        .then(|| format!("/library/items/{item_id}/attachments/{filename}"))
}
fn markdown_blocks(markdown: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current = String::new();
    let mut fence = None;
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        let marker = if trimmed.starts_with("```") {
            Some("```")
        } else if trimmed.starts_with("~~~") {
            Some("~~~")
        } else {
            None
        };
        if let Some(marker) = marker {
            if fence == Some(marker) {
                fence = None;
            } else if fence.is_none() {
                fence = Some(marker);
            }
        }
        if line.trim().is_empty() && fence.is_none() {
            if !current.trim().is_empty() {
                blocks.push(current.trim_end().to_owned());
                current.clear();
            }
        } else {
            current.push_str(line);
            current.push('\n');
        }
    }
    if !current.trim().is_empty() {
        blocks.push(current.trim_end().to_owned());
    }
    blocks
}
fn render_interleaved_markdown(
    first: &str,
    second: &str,
    item_id: &str,
    first_language: &str,
    second_language: &str,
) -> String {
    let first = markdown_blocks(first);
    let second = markdown_blocks(second);
    let mut output = String::new();
    for index in 0..first.len().max(second.len()) {
        output.push_str("<section class=\"parallel-block\">");
        for (blocks, language) in [(&first, first_language), (&second, second_language)] {
            if let Some(block) = blocks.get(index) {
                output.push_str(&format!(
                    "<div lang=\"{}\"><small class=\"language-label\">{}</small>{}</div>",
                    escape(language),
                    reader_language_label(language),
                    render_markdown(block, Some(item_id))
                ));
            }
        }
        output.push_str("</section>");
    }
    output
}
fn render_markdown(markdown: &str, item_id: Option<&str>) -> String {
    let mut events = Vec::new();
    let mut links = Vec::new();
    let mut images = Vec::new();
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS;
    for event in Parser::new_ext(markdown, options) {
        match event {
            Event::Html(_) | Event::InlineHtml(_) => {}
            Event::Start(Tag::Image {
                link_type,
                dest_url,
                title,
                id,
            }) => {
                let destination = item_id.and_then(|item_id| local_image_url(item_id, &dest_url));
                images.push(destination.is_some());
                if let Some(destination) = destination {
                    events.push(Event::Start(Tag::Image {
                        link_type,
                        dest_url: CowStr::from(destination),
                        title,
                        id,
                    }));
                }
            }
            Event::End(TagEnd::Image) => {
                if images.pop().unwrap_or(false) {
                    events.push(event)
                }
            }
            Event::Start(Tag::Link { ref dest_url, .. }) => {
                let ok = safe_link(dest_url);
                links.push(ok);
                if ok {
                    events.push(event)
                }
            }
            Event::End(TagEnd::Link) => {
                if links.pop().unwrap_or(false) {
                    events.push(event)
                }
            }
            _ => events.push(event),
        }
    }
    let mut out = String::new();
    html::push_html(&mut out, events.into_iter());
    out
}
fn parse_route(path: &str) -> std::result::Result<(&str, Option<String>), RouteError> {
    const BASES: [&str; 9] = [
        "/",
        "/health",
        "/catalog",
        "/sources",
        "/items",
        "/analyses",
        "/publications",
        "/public-releases",
        "/recall",
    ];
    if BASES.contains(&path) {
        return Ok((path, None));
    }
    for base in &BASES[3..8] {
        if let Some(raw) = path.strip_prefix(&format!("{base}/")) {
            if raw.is_empty() || raw.contains('/') || invalid_percent_encoding(raw) {
                return Err(RouteError::NotFound);
            }
            let decoded = form_urlencoded::parse(format!("x={raw}").as_bytes())
                .next()
                .map(|(_, v)| v.into_owned())
                .ok_or(RouteError::NotFound)?;
            if decoded.is_empty()
                || decoded.contains('/')
                || !decoded
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            {
                return Err(RouteError::NotFound);
            }
            return Ok((base, Some(decoded)));
        }
    }
    Err(RouteError::NotFound)
}
fn to_value<T: Serialize>(value: T) -> std::result::Result<Value, RouteError> {
    serde_json::to_value(value).map_err(|_| RouteError::Internal)
}
fn envelope(max_sensitivity: Sensitivity, data: Value) -> Value {
    json!({"schema_version":SCHEMA_VERSION,"surface_version":SURFACE_VERSION,"max_sensitivity":max_sensitivity,"data":data})
}
fn not_found() -> Value {
    json!({"schema_version":SCHEMA_VERSION,"error":{"code":"not_found","message":"resource not found"}})
}
fn exact_header<'a>(request: &'a Request, name: &'static str) -> Option<&'a str> {
    let mut values = request
        .headers()
        .iter()
        .filter(|header| header.field.equiv(name))
        .map(|header| header.value.as_str());
    let value = values.next()?;
    values.next().is_none().then_some(value)
}
fn header_count(request: &Request, name: &'static str) -> usize {
    request
        .headers()
        .iter()
        .filter(|header| header.field.equiv(name))
        .count()
}
fn valid_opaque_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}
fn capture_state(job: &memex_agent::queue::JobView) -> &'static str {
    use memex_agent::queue::{Stage, State};
    match job.state {
        State::Queued | State::RetryWait => "queued",
        State::Leased if matches!(job.stage, Stage::Extract | Stage::Resolve) => "parsing",
        State::Leased if job.stage == Stage::Fetch => "fetching",
        State::Leased if job.stage == Stage::Validate => "validating",
        State::Leased | State::ArtifactReady => "storing",
        State::NeedsUser => "needs_user",
        State::Failed | State::Cancelled => "failed",
        State::Succeeded => "stored",
    }
}
fn capture_retryable(job: &memex_agent::queue::JobView) -> bool {
    use memex_agent::queue::State;
    matches!(job.state, State::Failed | State::RetryWait)
        || (job.state == State::NeedsUser && job.reason_code.as_deref() != Some("needs_preview"))
}
fn capture_approvable(job: &memex_agent::queue::JobView) -> bool {
    use memex_agent::queue::State;
    job.state == State::NeedsUser && job.reason_code.as_deref() == Some("needs_preview")
}
fn capture_status_json(job: &memex_agent::queue::JobView) -> Value {
    json!({
        "schema_version": SCHEMA_VERSION,
        "job_id": job.job_id,
        "state": capture_state(job),
        "reason_code": job.reason_code,
        "item_id": job.item_id,
        "retryable": capture_retryable(job),
        "approvable": capture_approvable(job),
    })
}
fn capture_error(request: Request, status: StatusCode, code: &str, allow: Option<&str>) {
    respond(
        request,
        status,
        "application/json",
        json!({"schema_version":SCHEMA_VERSION,"error":{"code":code,"message":"request not accepted"}}).to_string(),
        false,
        allow,
        ResponseKind::Plain,
    )
}
fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}
fn html_page(path: &str, data: &Value) -> String {
    format!("<!doctype html><html><head><meta charset=\"utf-8\"><title>MemeX Local Surface</title></head><body><h1>MemeX Local Surface</h1><nav><a href=\"/\">Overview</a> <a href=\"/sources\">Sources</a> <a href=\"/items\">Items</a> <a href=\"/analyses\">Analyses</a> <a href=\"/publications\">Publications</a></nav><form action=\"/recall\" method=\"get\"><label>Recall <input type=\"text\" name=\"q\" required></label><label>Limit <input type=\"number\" name=\"limit\" min=\"1\" max=\"100\" value=\"20\"></label><button type=\"submit\">Recall</button></form><h2>{}</h2><pre>{}</pre></body></html>", escape(path), escape(&serde_json::to_string_pretty(data).unwrap_or_default()))
}
fn respond(
    request: Request,
    status: StatusCode,
    content_type: &str,
    body: String,
    head: bool,
    allow: Option<&str>,
    kind: ResponseKind,
) {
    respond_bytes(
        request,
        status,
        content_type,
        body.into_bytes(),
        head,
        allow,
        kind,
    )
}
fn respond_bytes(
    request: Request,
    status: StatusCode,
    content_type: &str,
    body: Vec<u8>,
    head: bool,
    allow: Option<&str>,
    kind: ResponseKind,
) {
    let content_length = body.len().to_string();
    let mut response =
        Response::from_data(if head { Vec::new() } else { body }).with_status_code(status);
    for (name, value) in [
        ("Content-Type", content_type),
        ("Cache-Control", "no-store"),
        ("X-Content-Type-Options", "nosniff"),
        ("Content-Length", &content_length),
    ] {
        if let Ok(header) = Header::from_bytes(name, value) {
            response = response.with_header(header);
        }
    }
    if let Some(value) = allow {
        if let Ok(header) = Header::from_bytes("Allow", value) {
            response = response.with_header(header);
        }
    }
    if kind != ResponseKind::Plain {
        let csp = match kind {
            ResponseKind::Mobile => "default-src 'none'; connect-src 'self'; style-src 'self'; script-src 'self'; img-src 'self'; form-action 'none'; base-uri 'none'; frame-ancestors 'none'",
            ResponseKind::Legacy => "default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'",
            ResponseKind::Plain => unreachable!(),
        };
        for (name, value) in [
            ("Content-Security-Policy", csp),
            ("Referrer-Policy", "no-referrer"),
            ("X-Frame-Options", "DENY"),
        ] {
            if let Ok(header) = Header::from_bytes(name, value) {
                response = response.with_header(header);
            }
        }
    }
    let _ = request.respond(response);
}

#[cfg(test)]
mod tests {
    use super::*;
    use memex_analysis::{
        ActorKind, AnalysisKind, AnalysisRequest, GeneratedTextDescriptor, GeneratedTextPurpose,
        SummaryModel, SummaryModelDescriptor, SummaryModelError, SummaryModelInput, SummaryQueue,
        TextLengthUnit, TextOutputLimit, TranslationModel, TranslationModelDescriptor,
        TranslationModelError, TranslationModelInput, TranslationQueue,
        GENERATED_TEXT_PROMPT_VERSION,
    };
    use memex_core::{
        BlobDescriptor, CaptureEnvelope, ContentRights, Creator, CreatorRole, DraftAttachmentRef,
        IdentifierKind, ItemDraft, ItemIdentifier, ItemMetadata, MetadataOrigin,
        MetadataProvenance, RightsStatus, SourceProfile, Sourced, VerifiedBlob, VerifiedCapture,
    };
    use memex_publication::{
        PublicationInputKind, PublicationInputRequest, PublicationKind, PublicationRequest,
    };
    use std::{
        fs,
        io::{Read, Write},
        net::TcpStream,
        os::unix::net::UnixListener,
        thread,
        time::Duration,
    };

    struct SurfaceTranslationModel {
        error: Option<TranslationModelError>,
    }

    impl TranslationModel for SurfaceTranslationModel {
        fn descriptor(&self) -> TranslationModelDescriptor {
            TranslationModelDescriptor {
                model_id: "synthetic-surface-test-only-v1".into(),
                model_sha256: format!("sha256:{}", "9".repeat(64)),
                adapter_id: "memex.synthetic-surface-test-only".into(),
                adapter_version: "1".into(),
                request_timeout: Duration::from_secs(1),
            }
        }

        fn translate(
            &mut self,
            input: TranslationModelInput<'_>,
        ) -> std::result::Result<String, TranslationModelError> {
            if let Some(error) = self.error.take() {
                return Err(error);
            }
            Ok(match input.purpose() {
                GeneratedTextPurpose::TitleTranslation => "可验证的中文标题".into(),
                GeneratedTextPurpose::DocumentTranslation => {
                    "# 中文全文\n\n这是通过测试模型生成并通过完整性检查的中文全文。".into()
                }
                GeneratedTextPurpose::Summary => unreachable!(),
            })
        }
    }

    struct SurfaceSummaryModel {
        error: Option<SummaryModelError>,
    }

    impl SummaryModel for SurfaceSummaryModel {
        fn descriptor(&self) -> SummaryModelDescriptor {
            SummaryModelDescriptor {
                model_id: "synthetic-summary-surface-test-only-v1".into(),
                model_sha256: format!("sha256:{}", "8".repeat(64)),
                adapter_id: "memex.synthetic-summary-surface-test-only".into(),
                adapter_version: "1".into(),
                request_timeout: Duration::from_secs(1),
            }
        }

        fn summarize(
            &mut self,
            _: SummaryModelInput<'_>,
        ) -> std::result::Result<String, SummaryModelError> {
            if let Some(error) = self.error.take() {
                return Err(error);
            }
            Ok("这份经过校验的摘要概括了合成报告中的事实与确定性验证过程。".into())
        }
    }

    fn live_server() -> u16 {
        let library = tempfile::tempdir().unwrap().keep();
        let server = SurfaceServer::new(SurfaceConfig {
            library,
            max_sensitivity: Sensitivity::Public,
            port: 0,
        })
        .unwrap();
        let port = server.port();
        thread::spawn(move || server.serve().unwrap());
        port
    }
    fn request(port: u16, wire: &str) -> String {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        stream.write_all(wire.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }
    fn request_bytes(port: u16, wire: &str) -> Vec<u8> {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        stream.write_all(wire.as_bytes()).unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        response
    }

    fn capture_wire(port: u16, key: &str, text: &str) -> String {
        let body = json!({
            "schema_version": 1,
            "command": "capture_public_url",
            "text": text,
        })
        .to_string();
        request(
            port,
            &format!(
                "POST /memex/capture HTTP/1.1\r\nHost: localhost:{port}\r\nOrigin: http://localhost:{port}\r\nContent-Type: application/json\r\nIdempotency-Key: {key}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ),
        )
    }

    fn spawn_fake_agent(socket: &std::path::Path) -> thread::JoinHandle<()> {
        let listener = UnixListener::bind(socket).unwrap();
        thread::spawn(move || {
            for index in 0..4 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut length = [0; 4];
                stream.read_exact(&mut length).unwrap();
                let mut frame = vec![0; u32::from_be_bytes(length) as usize];
                stream.read_exact(&mut frame).unwrap();
                let command: Value = serde_json::from_slice(&frame).unwrap();
                if index < 3 {
                    assert_eq!(command["command"], "enqueue");
                    assert_eq!(command["producer"], "local_surface");
                } else {
                    assert_eq!(command["command"], "status");
                }
                let response = if index == 2 {
                    json!({
                        "ok": false,
                        "job": null,
                        "jobs": null,
                        "next_cursor": null,
                        "reason_code": "request_identity_conflict",
                    })
                } else {
                    json!({
                        "ok": true,
                        "job": {
                            "job_id": "job_surface_test",
                            "state": if index == 3 { "succeeded" } else { "queued" },
                            "stage": if index == 3 { "commit" } else { "extract" },
                            "reason_code": null,
                            "item_id": if index == 3 { Some("item_surface_result") } else { None },
                            "created_at": "2026-08-12T00:00:00Z",
                            "updated_at": "2026-08-12T00:00:00Z",
                            "attempt_count": if index == 3 { 1 } else { 0 },
                            "preview": null,
                        },
                        "jobs": null,
                        "next_cursor": null,
                        "reason_code": null,
                    })
                };
                let frame = serde_json::to_vec(&response).unwrap();
                stream
                    .write_all(&(frame.len() as u32).to_be_bytes())
                    .unwrap();
                stream.write_all(&frame).unwrap();
            }
        })
    }

    fn spawn_fake_preview_agent(socket: &std::path::Path) -> thread::JoinHandle<()> {
        let listener = UnixListener::bind(socket).unwrap();
        thread::spawn(move || {
            for index in 0..4 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut length = [0; 4];
                stream.read_exact(&mut length).unwrap();
                let mut frame = vec![0; u32::from_be_bytes(length) as usize];
                stream.read_exact(&mut frame).unwrap();
                let command: Value = serde_json::from_slice(&frame).unwrap();
                assert_eq!(
                    command["command"],
                    match index {
                        0 | 2 => "status",
                        1 => "approve",
                        3 => "retry",
                        _ => unreachable!(),
                    }
                );
                let failed_job = index >= 2;
                let response = json!({
                    "ok": true,
                    "job": {
                        "job_id": if failed_job { "job_failed_test" } else { "job_preview_test" },
                        "state": match index { 0 => "needs_user", 1 => "artifact_ready", 2 => "failed", 3 => "queued", _ => unreachable!() },
                        "stage": match index { 0 | 2 | 3 => "validate", 1 => "translate", _ => unreachable!() },
                        "reason_code": match index { 0 => "needs_preview", 1 => "preview_approved", 2 => "address_not_public", 3 => "manual_retry", _ => unreachable!() },
                        "item_id": null,
                        "created_at": "2026-08-12T00:00:00Z",
                        "updated_at": "2026-08-12T00:00:00Z",
                        "attempt_count": 1,
                        "preview": null,
                    },
                    "jobs": null,
                    "next_cursor": null,
                    "reason_code": null,
                });
                let frame = serde_json::to_vec(&response).unwrap();
                stream
                    .write_all(&(frame.len() as u32).to_be_bytes())
                    .unwrap();
                stream.write_all(&frame).unwrap();
            }
        })
    }

    fn ingest_one(root: &std::path::Path, sensitivity: Sensitivity, marker: &str) -> String {
        ingest_fixture(
            root,
            sensitivity,
            marker,
            ItemMetadata::default(),
            marker.to_owned(),
        )
    }

    fn ingest_fixture(
        root: &std::path::Path,
        sensitivity: Sensitivity,
        marker: &str,
        metadata: ItemMetadata,
        content_markdown: String,
    ) -> String {
        let capture = VerifiedCapture {
            source: SourceProfile {
                schema_version: 1,
                source_id: format!("source_{marker}"),
                name: format!("fixture source {marker}"),
                source_kind: "web".into(),
                canonical_uris: vec![],
                evidence_roles: vec![],
                default_sensitivity: sensitivity,
                preferred_extractor: None,
            },
            capture: CaptureEnvelope {
                schema_version: 1,
                adapter_id: "fixture.adapter".into(),
                adapter_version: "1".into(),
                external_capture_id: format!("fixture.capture.{marker}"),
                source_id: format!("source_{marker}"),
                captured_at: "2026-07-31T00:00:00Z".into(),
                capture_method: "test".into(),
                sensitivity,
                route_summary: None,
                title: Some(format!("{marker} item")),
                source_url: "https://example.invalid/fixture".into(),
                canonical_url: None,
                artifacts: vec![],
            },
            blobs: vec![VerifiedBlob {
                descriptor: BlobDescriptor {
                    schema_version: 1,
                    locator: "body".into(),
                    media_type: "text/plain".into(),
                    byte_size: marker.len() as u64,
                    sha256: format!("sha256:{marker}"),
                    source_url: None,
                },
                blob_id: format!("fixture.blob.{marker}"),
                source_backend_ref: Some("fixture".into()),
                bytes: marker.as_bytes().to_vec(),
            }],
        };
        memex_store::ingest(
            root,
            &capture,
            ItemDraft {
                item_type: "webpage".into(),
                title: format!("{marker} item"),
                source_url: "https://example.invalid/fixture".into(),
                canonical_url: None,
                content_markdown,
                sensitivity,
                translator_id: "fixture.translator".into(),
                translator_version: "1".into(),
                metadata,
                attachments: Vec::new(),
            },
        )
        .unwrap()
        .item_ids
        .remove(0)
    }

    fn ingest_image_fixture(root: &std::path::Path) -> (String, String) {
        let digest = "4c4b6a3be1314ab86138bef4314dde022e600960d8689a2c8f8631802d20dab6";
        let relative = format!("attachments/sha256-{digest}.png");
        let capture = VerifiedCapture {
            source: SourceProfile {
                schema_version: 1,
                source_id: "source_MOBILE_IMAGE".into(),
                name: "fixture source MOBILE_IMAGE".into(),
                source_kind: "web".into(),
                canonical_uris: vec![],
                evidence_roles: vec![],
                default_sensitivity: Sensitivity::Public,
                preferred_extractor: None,
            },
            capture: CaptureEnvelope {
                schema_version: 1,
                adapter_id: "fixture.adapter".into(),
                adapter_version: "1".into(),
                external_capture_id: "fixture.capture.MOBILE_IMAGE".into(),
                source_id: "source_MOBILE_IMAGE".into(),
                captured_at: "2026-08-10T00:00:00Z".into(),
                capture_method: "test".into(),
                sensitivity: Sensitivity::Public,
                route_summary: None,
                title: Some("MOBILE_IMAGE item".into()),
                source_url: "https://example.invalid/image-fixture".into(),
                canonical_url: None,
                artifacts: vec![],
            },
            blobs: vec![
                VerifiedBlob {
                    descriptor: BlobDescriptor {
                        schema_version: 1,
                        locator: "body".into(),
                        media_type: "text/plain".into(),
                        byte_size: 4,
                        sha256: "fixture-body".into(),
                        source_url: None,
                    },
                    blob_id: "fixture.blob.MOBILE_IMAGE".into(),
                    source_backend_ref: Some("fixture".into()),
                    bytes: b"body".to_vec(),
                },
                VerifiedBlob {
                    descriptor: BlobDescriptor {
                        schema_version: 1,
                        locator: "inline_image_0".into(),
                        media_type: "image/png".into(),
                        byte_size: 8,
                        sha256: digest.into(),
                        source_url: Some("https://images.example.invalid/diagram.png".into()),
                    },
                    blob_id: format!("sha256:{digest}"),
                    source_backend_ref: None,
                    bytes: b"\x89PNG\r\n\x1a\n".to_vec(),
                },
            ],
        };
        let item_id = memex_store::ingest(
            root,
            &capture,
            ItemDraft {
                item_type: "webpage".into(),
                title: "MOBILE_IMAGE item".into(),
                source_url: "https://example.invalid/image-fixture".into(),
                canonical_url: None,
                content_markdown: format!("Before\n\n![Diagram]({relative})\n\nAfter"),
                sensitivity: Sensitivity::Public,
                translator_id: "fixture.translator".into(),
                translator_version: "1".into(),
                metadata: ItemMetadata::default(),
                attachments: vec![DraftAttachmentRef::Captured {
                    blob_index: 1,
                    blob_id: format!("sha256:{digest}"),
                    title: Some("Diagram".into()),
                }],
            },
        )
        .unwrap()
        .item_ids
        .remove(0);
        (item_id, relative)
    }

    fn fixture_sourced<T>(value: T, provenance: &MetadataProvenance) -> Sourced<T> {
        Sourced {
            value,
            provenance: provenance.clone(),
        }
    }

    fn mobile_metadata(marker: &str, tags: &[&str]) -> ItemMetadata {
        let provenance = MetadataProvenance {
            origin: MetadataOrigin::IntakeProvided,
            provider_id: "fixture.intake".into(),
            provider_version: "1".into(),
            source_refs: Vec::new(),
        };
        ItemMetadata {
            creators: Some(fixture_sourced(
                vec![Creator::new(
                    format!("{marker} Author"),
                    CreatorRole::Author,
                )],
                &provenance,
            )),
            published_date: Some(fixture_sourced("2026-08-10".into(), &provenance)),
            language: Some(fixture_sourced("zh-CN".into(), &provenance)),
            site_name: Some(fixture_sourced("Synthetic Library".into(), &provenance)),
            abstract_text: Some(fixture_sourced(
                format!("{marker} ABSTRACT_ONLY"),
                &provenance,
            )),
            identifiers: Some(fixture_sourced(
                vec![ItemIdentifier::new(
                    IdentifierKind::Doi,
                    format!("10.1/{marker}"),
                )],
                &provenance,
            )),
            manual_tags: Some(fixture_sourced(
                tags.iter().map(|tag| (*tag).to_owned()).collect(),
                &provenance,
            )),
            content_rights: Some(fixture_sourced(
                ContentRights {
                    status: RightsStatus::Unknown,
                    statement: Some("Synthetic proof only".into()),
                    license_url: None,
                },
                &provenance,
            )),
            ..Default::default()
        }
    }

    fn analysis_one(
        root: &std::path::Path,
        sensitivity: Sensitivity,
        item_id: String,
        marker: &str,
    ) -> String {
        let prepared = memex_analysis::prepare_record(
            root,
            &AnalysisRequest {
                schema_version: 1,
                analysis_kind: AnalysisKind::Summary,
                title: format!("{marker} analysis"),
                body_markdown: marker.into(),
                item_ids: vec![item_id],
                topics: vec!["fixture".into()],
                requested_sensitivity: Some(sensitivity),
                created_at: "2026-07-31T00:00:00Z".into(),
                actor_kind: ActorKind::Agent,
                actor_id: "fixture.agent".into(),
                tool_id: "fixture.tool".into(),
                tool_version: "1".into(),
                config_digest: format!("sha256:{}", "1".repeat(64)),
                generated_text: None,
            },
        )
        .unwrap();
        let id = prepared.analysis_id.clone();
        memex_analysis::apply_record(root, &prepared).unwrap();
        id
    }

    fn generated_one(
        root: &std::path::Path,
        item_id: &str,
        purpose: GeneratedTextPurpose,
        source_language: &str,
        target_language: &str,
        body: &str,
    ) -> String {
        let (analysis_kind, output_limit) = match purpose {
            GeneratedTextPurpose::Summary => (
                AnalysisKind::Summary,
                Some(TextOutputLimit {
                    unit: if target_language == "zh-CN" {
                        TextLengthUnit::Characters
                    } else {
                        TextLengthUnit::Words
                    },
                    maximum: 180,
                }),
            ),
            GeneratedTextPurpose::TitleTranslation => (
                AnalysisKind::Translation,
                Some(TextOutputLimit {
                    unit: TextLengthUnit::Characters,
                    maximum: 240,
                }),
            ),
            GeneratedTextPurpose::DocumentTranslation => (AnalysisKind::Translation, None),
        };
        let (prompt_id, _, prompt_sha256) = memex_analysis::generated_text_prompt(purpose);
        let prepared = memex_analysis::prepare_record(
            root,
            &AnalysisRequest {
                schema_version: 1,
                analysis_kind,
                title: format!("generated {purpose:?} {target_language}"),
                body_markdown: body.into(),
                item_ids: vec![item_id.into()],
                topics: vec!["fixture".into()],
                requested_sensitivity: None,
                created_at: "2026-08-10T00:00:00Z".into(),
                actor_kind: ActorKind::Agent,
                actor_id: "fixture.luna".into(),
                tool_id: "openai.chat-completions".into(),
                tool_version: "v1".into(),
                config_digest: format!("sha256:{}", "3".repeat(64)),
                generated_text: Some(GeneratedTextDescriptor {
                    purpose,
                    source_language: source_language.into(),
                    target_language: target_language.into(),
                    output_limit,
                    summary_job_id: None,
                    translation_job_id: None,
                    model_id: "gpt-5.6-luna".into(),
                    model_sha256: None,
                    prompt_id: prompt_id.into(),
                    prompt_version: GENERATED_TEXT_PROMPT_VERSION.into(),
                    prompt_sha256,
                }),
            },
        )
        .unwrap();
        let id = prepared.analysis_id.clone();
        memex_analysis::apply_record(root, &prepared).unwrap();
        id
    }

    fn publication_one(
        root: &std::path::Path,
        sensitivity: Sensitivity,
        analysis_id: String,
        marker: &str,
    ) -> String {
        let prepared = memex_publication::prepare_publication(
            root,
            &PublicationRequest {
                schema_version: 1,
                publication_kind: PublicationKind::Briefing,
                product_id: "fixture.product".into(),
                title: format!("{marker} publication"),
                summary: marker.into(),
                body_markdown: marker.into(),
                inputs: vec![PublicationInputRequest {
                    kind: PublicationInputKind::Analysis,
                    id: analysis_id,
                    revision_id: None,
                }],
                topics: vec!["fixture".into()],
                requested_sensitivity: Some(sensitivity),
                created_at: "2026-07-31T00:00:00Z".into(),
                actor_kind: ActorKind::Agent,
                actor_id: "fixture.agent".into(),
                tool_id: "fixture.tool".into(),
                tool_version: "1".into(),
                config_digest: format!("sha256:{}", "2".repeat(64)),
            },
        )
        .unwrap();
        let id = prepared.publication_id.clone();
        memex_publication::apply_publication(root, &prepared).unwrap();
        id
    }

    fn api_request(port: u16, path: &str) -> String {
        request(
            port,
            &format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"),
        )
    }
    #[test]
    fn escape_html() {
        assert_eq!(escape("<x a='b'>&"), "&lt;x a=&#x27;b&#x27;&gt;&amp;");
    }
    #[test]
    fn mobile_themes_are_achromatic_day_night_only() {
        assert!(!CSS.contains("sepia"));
        assert!(!JS.contains("sepia"));
        for color in CSS.as_bytes().windows(7).filter(|candidate| {
            candidate[0] == b'#' && candidate[1..].iter().all(u8::is_ascii_hexdigit)
        }) {
            let channel =
                |pair: &[u8]| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap();
            let red = channel(&color[1..3]);
            let green = channel(&color[3..5]);
            let blue = channel(&color[5..7]);
            assert_eq!((red, green), (green, blue), "non-grayscale CSS color");
        }
        for line in CSS.lines().filter(|line| line.contains("rgb(")) {
            assert!(line.contains("rgb(0 0 0 /"), "non-grayscale rgb color");
        }
    }

    #[test]
    fn mobile_capture_homepage_is_versioned_deterministic_and_body_only() {
        let temp = tempfile::tempdir().unwrap();
        let server = SurfaceServer::new(SurfaceConfig {
            library: temp.path().to_path_buf(),
            max_sensitivity: Sensitivity::Public,
            port: 0,
        })
        .unwrap();
        let first = server.library_page(None, None).unwrap();
        let second = server.library_page(None, None).unwrap();
        assert_eq!(first, second);
        for expected in [
            "抓取一篇文章",
            "data-homepage-version=\"mobile_library_homepage_capture_v1\"",
            "data-capture-contract=\"memex.capture_command.v1\"",
            "data-capture-form",
            "data-capture-input",
            "maxlength=\"16384\"",
            "autocomplete=\"off\"",
            "data-capture-status",
        ] {
            assert!(
                first.contains(expected),
                "missing capture marker {expected}"
            );
        }
        assert!(!first.contains("<form action="));
        assert!(!first.contains("name=\"capture"));
        assert!(JS.contains("fetch(\"/memex/capture\""));
        assert!(JS.contains("credentials: \"same-origin\""));
        assert!(JS.contains("\"Idempotency-Key\": idempotencyKey"));
        assert!(CSS.contains("min-height: 48px"));
    }

    #[test]
    fn direct_capture_uses_local_surface_uds_replays_and_reports_status() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("agent.sock");
        let library = temp.path().join("library");
        fs::create_dir(&library).unwrap();
        let fake_agent = spawn_fake_agent(&socket);
        let server = SurfaceServer::new_for_direct_test(
            SurfaceConfig {
                library,
                max_sensitivity: Sensitivity::Internal,
                port: 0,
            },
            socket,
        )
        .unwrap();
        let port = server.port();
        thread::spawn(move || server.serve().unwrap());

        let first = capture_wire(
            port,
            "local_surface_key",
            "please capture https://synthetic.test/private/path?secret=value",
        );
        let replay = capture_wire(
            port,
            "local_surface_key",
            "please capture https://synthetic.test/private/path?secret=value",
        );
        assert!(first.starts_with("HTTP/1.1 202"));
        assert!(replay.starts_with("HTTP/1.1 202"));
        assert!(first.contains("job_surface_test"));
        assert!(replay.contains("job_surface_test"));
        for sensitive in ["/private/path", "secret=value", "please capture"] {
            assert!(!first.contains(sensitive));
            assert!(!replay.contains(sensitive));
        }

        let conflict = capture_wire(
            port,
            "local_surface_key",
            "https://different.synthetic.test/article",
        );
        assert!(conflict.starts_with("HTTP/1.1 409"));
        assert!(!conflict.contains("different.synthetic"));

        let status = request(
            port,
            &format!("GET /memex/capture/status/job_surface_test HTTP/1.1\r\nHost: localhost:{port}\r\nOrigin: http://localhost:{port}\r\nConnection: close\r\n\r\n"),
        );
        assert!(status.starts_with("HTTP/1.1 200"));
        assert!(status.contains("\"state\":\"stored\""));
        assert!(status.contains("\"item_id\":\"item_surface_result\""));
        assert!(!status.contains("created_at"));
        fake_agent.join().unwrap();
    }

    #[test]
    fn direct_capture_status_exposes_bounded_preview_approval_action() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("agent.sock");
        let library = temp.path().join("library");
        fs::create_dir(&library).unwrap();
        let fake_agent = spawn_fake_preview_agent(&socket);
        let server = SurfaceServer::new_for_direct_test(
            SurfaceConfig {
                library,
                max_sensitivity: Sensitivity::Internal,
                port: 0,
            },
            socket,
        )
        .unwrap();
        let port = server.port();
        thread::spawn(move || server.serve().unwrap());

        let status = request(
            port,
            &format!("GET /memex/capture/status/job_preview_test HTTP/1.1\r\nHost: localhost:{port}\r\nOrigin: http://localhost:{port}\r\nConnection: close\r\n\r\n"),
        );
        assert!(status.starts_with("HTTP/1.1 200"));
        assert!(status.contains("\"state\":\"needs_user\""));
        assert!(status.contains("\"reason_code\":\"needs_preview\""));
        assert!(status.contains("\"approvable\":true"));

        let approval = request(
            port,
            &format!("POST /memex/capture/approve/job_preview_test HTTP/1.1\r\nHost: localhost:{port}\r\nOrigin: http://localhost:{port}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
        );
        assert!(approval.starts_with("HTTP/1.1 202"));
        assert!(approval.contains("\"state\":\"storing\""));
        assert!(approval.contains("\"approvable\":false"));

        let failed = request(
            port,
            &format!("GET /memex/capture/status/job_failed_test HTTP/1.1\r\nHost: localhost:{port}\r\nOrigin: http://localhost:{port}\r\nConnection: close\r\n\r\n"),
        );
        assert!(failed.starts_with("HTTP/1.1 200"));
        assert!(failed.contains("\"state\":\"failed\""));
        assert!(failed.contains("\"reason_code\":\"address_not_public\""));
        assert!(failed.contains("\"retryable\":true"));

        let retry = request(
            port,
            &format!("POST /memex/capture/retry/job_failed_test HTTP/1.1\r\nHost: localhost:{port}\r\nOrigin: http://localhost:{port}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
        );
        assert!(retry.starts_with("HTTP/1.1 202"));
        assert!(retry.contains("\"state\":\"queued\""));
        assert!(retry.contains("\"retryable\":false"));
        fake_agent.join().unwrap();
    }

    #[test]
    fn direct_capture_fails_closed_without_daemon_and_on_wrong_origin() {
        let temp = tempfile::tempdir().unwrap();
        let library = temp.path().join("library");
        fs::create_dir(&library).unwrap();
        let server = SurfaceServer::new_for_direct_test(
            SurfaceConfig {
                library,
                max_sensitivity: Sensitivity::Internal,
                port: 0,
            },
            temp.path().join("missing.sock"),
        )
        .unwrap();
        let port = server.port();
        thread::spawn(move || server.serve().unwrap());
        let unavailable = capture_wire(port, "local_surface_key", "https://synthetic.test/article");
        assert!(unavailable.starts_with("HTTP/1.1 503"));
        assert!(!unavailable.contains("synthetic.test"));

        let body = r#"{"schema_version":1,"command":"capture_public_url","text":"https://synthetic.test/article"}"#;
        let wrong_origin = request(
            port,
            &format!("POST /memex/capture HTTP/1.1\r\nHost: localhost:{port}\r\nOrigin: http://evil.invalid\r\nContent-Type: application/json\r\nIdempotency-Key: local_surface_key\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()),
        );
        assert!(wrong_origin.starts_with("HTTP/1.1 404"));

        let missing_origin = request(
            port,
            &format!("POST /memex/capture HTTP/1.1\r\nHost: localhost:{port}\r\nContent-Type: application/json\r\nIdempotency-Key: local_surface_key\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()),
        );
        assert!(missing_origin.starts_with("HTTP/1.1 404"));

        let duplicate_host = request(
            port,
            &format!("POST /memex/capture HTTP/1.1\r\nHost: localhost:{port}\r\nHost: localhost:{port}\r\nOrigin: http://localhost:{port}\r\nContent-Type: application/json\r\nIdempotency-Key: local_surface_key\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()),
        );
        assert!(duplicate_host.starts_with("HTTP/1.1 404"));
    }

    #[test]
    fn direct_capture_rejects_chunked_and_oversized_bodies_before_uds() {
        let temp = tempfile::tempdir().unwrap();
        let library = temp.path().join("library");
        fs::create_dir(&library).unwrap();
        let socket = temp.path().join("agent.sock");
        let _unused_agent = UnixListener::bind(&socket).unwrap();
        let server = SurfaceServer::new_for_direct_test(
            SurfaceConfig {
                library,
                max_sensitivity: Sensitivity::Internal,
                port: 0,
            },
            socket,
        )
        .unwrap();
        let port = server.port();
        thread::spawn(move || server.serve().unwrap());

        let chunked = request(
            port,
            &format!("POST /memex/capture HTTP/1.1\r\nHost: localhost:{port}\r\nOrigin: http://localhost:{port}\r\nContent-Type: application/json\r\nIdempotency-Key: local_surface_key\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n2\r\n{{}}\r\n0\r\n\r\n"),
        );
        assert!(chunked.starts_with("HTTP/1.1 400"));

        let duplicate_transfer_encoding = request(
            port,
            &format!("POST /memex/capture HTTP/1.1\r\nHost: localhost:{port}\r\nOrigin: http://localhost:{port}\r\nContent-Type: application/json\r\nIdempotency-Key: local_surface_key\r\nContent-Length: 2\r\nTransfer-Encoding: chunked\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n2\r\n{{}}\r\n0\r\n\r\n"),
        );
        assert!(duplicate_transfer_encoding.starts_with("HTTP/1.1 400"));

        let duplicate_content_length = request(
            port,
            &format!("POST /memex/capture HTTP/1.1\r\nHost: localhost:{port}\r\nOrigin: http://localhost:{port}\r\nContent-Type: application/json\r\nIdempotency-Key: local_surface_key\r\nContent-Length: 2\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"),
        );
        assert!(duplicate_content_length.starts_with("HTTP/1.1 400"));

        let chunked_action = request(
            port,
            &format!("POST /memex/capture/retry/job_surface_test HTTP/1.1\r\nHost: localhost:{port}\r\nOrigin: http://localhost:{port}\r\nContent-Length: 0\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n0\r\n\r\n"),
        );
        assert!(chunked_action.starts_with("HTTP/1.1 400"));

        let oversized = request(
            port,
            &format!("POST /memex/capture HTTP/1.1\r\nHost: localhost:{port}\r\nOrigin: http://localhost:{port}\r\nContent-Type: application/json\r\nIdempotency-Key: local_surface_key\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", MAX_CAPTURE_BODY + 1),
        );
        assert!(oversized.starts_with("HTTP/1.1 400"));
    }

    #[test]
    fn private_lan_constructor_rejects_non_private_binds() {
        let temp = tempfile::tempdir().unwrap();
        let config = SurfaceConfig {
            library: temp.path().join("library"),
            max_sensitivity: Sensitivity::Internal,
            port: 43191,
        };
        for address in [
            Ipv4Addr::LOCALHOST,
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::new(8, 8, 8, 8),
        ] {
            assert!(SurfaceServer::new_for_private_lan(
                config.clone(),
                address,
                "owner-mac.local",
                temp.path().join("agent.sock"),
            )
            .is_err());
        }
    }

    #[test]
    fn memex_alias_refreshes_after_a_live_store_commit() {
        let temp = tempfile::tempdir().unwrap();
        let library = temp.path().join("library");
        fs::create_dir(&library).unwrap();
        let server = SurfaceServer::new(SurfaceConfig {
            library: library.clone(),
            max_sensitivity: Sensitivity::Public,
            port: 0,
        })
        .unwrap();
        let port = server.port();
        thread::spawn(move || server.serve().unwrap());
        let before = request(
            port,
            &format!("GET /memex HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"),
        );
        assert!(before.starts_with("HTTP/1.1 200"));
        assert!(!before.contains("LIVE_REFRESH_MARKER"));

        ingest_one(&library, Sensitivity::Public, "LIVE_REFRESH_MARKER");
        let after = request(
            port,
            &format!("GET /memex HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"),
        );
        assert!(after.starts_with("HTTP/1.1 200"));
        assert!(after.contains("LIVE_REFRESH_MARKER"));
    }

    #[test]
    fn mobile_reader_preserves_generated_text_casing() {
        assert!(CSS.contains(".reader header > p"));
        assert!(!CSS.contains(".reader header p,"));
    }

    #[test]
    fn mobile_emitted_reader_links_follow_optional_language_route() {
        let temp = tempfile::tempdir().unwrap();
        let library = temp.path().join("library");
        let item_id = ingest_fixture(
            &library,
            Sensitivity::Public,
            "READER_LINK",
            mobile_metadata("READER_LINK", &["route-contract"]),
            "# Synthetic full text\n\nFULLTEXT_ONLY".into(),
        );
        let server = SurfaceServer::new(SurfaceConfig {
            library,
            max_sensitivity: Sensitivity::Public,
            port: 0,
        })
        .unwrap();

        let emitted_url = format!("/library/items/{item_id}/read?theme=dark&amp;font=serif");
        let library_page = server
            .library_page(Some("theme=dark&font=serif"), None)
            .unwrap();
        assert!(library_page.contains(&format!("data-read-url=\"{emitted_url}\"")));
        assert!(!library_page.contains("FULLTEXT_ONLY"));

        let detail_page = server
            .library_page(
                Some(&format!("detail={item_id}&theme=dark&font=serif")),
                None,
            )
            .unwrap();
        assert!(detail_page.contains(&format!("class=\"read\" href=\"{emitted_url}\"")));
        assert!(!detail_page.contains("FULLTEXT_ONLY"));

        let port = server.port();
        thread::spawn(move || server.serve().unwrap());
        let reader_path = format!("/library/items/{item_id}/read?theme=dark&font=serif");
        let reader = api_request(port, &reader_path);
        assert!(reader.starts_with("HTTP/1.1 200"));
        assert!(reader.contains("Content-Type: text/html; charset=utf-8"));
        assert!(reader.contains("<article class=\"reader\">"));
        assert!(reader.contains("data-theme=\"dark\" data-font=\"serif\""));
        assert!(reader.contains("FULLTEXT_ONLY"));
    }

    #[test]
    fn mobile_query_validation_and_budget_accounting_are_bounded() {
        assert!(parse_query(Some(""), &["limit"]).is_err());
        assert!(parse_query(Some("limit=1&limit=2"), &["limit"]).is_err());
        assert!(parse_query(Some("tag=%FF"), &["tag"]).is_err());
        assert!(parse_query(Some(&format!("cursor={}", "x".repeat(513))), &["cursor"]).is_err());
        assert!(parse_limit(Some(&"0".into())).is_err());
        assert!(parse_limit(Some(&"01".into())).is_err());
        assert!(parse_limit(Some(&"101".into())).is_err());
        let row = LibraryItem {
            item_id: "item_x".into(),
            revision_id: "revision_x".into(),
            item_type: "webpage".into(),
            title: "title".into(),
            creators: vec!["creator".into()],
            published_date: None,
            captured_at: None,
            captured_at_epoch: None,
            site_name: None,
            manual_tags: vec!["tag".into()],
            sensitivity: Sensitivity::Public,
        };
        let row_cost = index_cost(&row);
        assert!(row_cost >= std::mem::size_of::<LibraryItem>() + 38);
        assert_eq!(retained_index_cost(&[row], 1, &[], 0).unwrap(), row_cost);
        assert_eq!(INDEX_BUDGET, 64 * 1024 * 1024);
        let mut retained = INDEX_BUDGET;
        reserve_index_budget(&mut retained, 0).unwrap();
        assert!(reserve_index_budget(&mut retained, 1).is_err());
    }
    #[test]
    fn mobile_summary_and_markdown_do_not_expose_private_content() {
        let row = LibraryItem {
            item_id: "item_x".into(),
            revision_id: "revision_x".into(),
            item_type: "webpage".into(),
            title: "title".into(),
            creators: vec![],
            published_date: None,
            captured_at: None,
            captured_at_epoch: None,
            site_name: None,
            manual_tags: vec![],
            sensitivity: Sensitivity::Public,
        };
        let value = serde_json::to_string(&row).unwrap();
        for forbidden in [
            "markdown",
            "body",
            "source_url",
            "provenance",
            "attachments",
            "abstract",
        ] {
            assert!(!value.contains(forbidden));
        }
        let rendered = render_markdown(
            "<b>raw</b> [safe](https://example.com) [upper](HTTPS://example.com) [bad](javascript:alert(1)) [data](data:text/html,x) [network](//example.com) ![alt](https://example.com/x.png)",
            Some("item_x"),
        );
        assert!(!rendered.contains("<b>"));
        assert!(!rendered.contains("javascript:"));
        assert!(!rendered.contains("<img"));
        assert!(rendered.contains("href=\"https://example.com\""));
        assert!(rendered.contains("href=\"HTTPS://example.com\""));
        assert!(rendered.contains("bad"));
        assert!(!rendered.contains("href=\"data:"));
        assert!(!rendered.contains("href=\"//"));
        assert!(rendered.contains("alt"));
        assert_eq!(
            rendered.matches("<a ").count(),
            rendered.matches("</a>").count()
        );
    }

    #[test]
    fn mobile_reader_serves_only_verified_local_images() {
        let temp = tempfile::tempdir().unwrap();
        let library = temp.path().join("library");
        let (item_id, relative) = ingest_image_fixture(&library);
        let filename = relative.strip_prefix("attachments/").unwrap();
        let route = format!("/library/items/{item_id}/attachments/{filename}");
        let server = SurfaceServer::new(SurfaceConfig {
            library,
            max_sensitivity: Sensitivity::Public,
            port: 0,
        })
        .unwrap();
        let reader = server.reader_page(&item_id, None).unwrap();
        assert!(reader.contains(&format!("<img src=\"{route}\" alt=\"Diagram\"")));
        assert!(reader.contains("Before"));
        assert!(reader.contains("After"));
        let unsafe_image = render_markdown(
            "![unsafe](attachments/../outside.png) ![remote](https://example.invalid/x.png)",
            Some(&item_id),
        );
        assert!(!unsafe_image.contains("<img"));
        assert!(unsafe_image.contains("unsafe") && unsafe_image.contains("remote"));

        let port = server.port();
        thread::spawn(move || server.serve().unwrap());
        let response = request_bytes(
            port,
            &format!("GET {route} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"),
        );
        let split = response
            .windows(4)
            .position(|part| part == b"\r\n\r\n")
            .unwrap()
            + 4;
        let headers = std::str::from_utf8(&response[..split]).unwrap();
        assert!(headers.starts_with("HTTP/1.1 200"));
        assert!(headers.contains("Content-Type: image/png"));
        assert!(headers.contains("Content-Length: 8"));
        assert!(headers.contains("X-Content-Type-Options: nosniff"));
        assert_eq!(&response[split..], b"\x89PNG\r\n\x1a\n");

        let reader_response = api_request(port, &format!("/library/items/{item_id}/read"));
        assert!(reader_response.contains("img-src 'self'"));
        assert!(api_request(port, &format!("{route}?unexpected=1")).starts_with("HTTP/1.1 404"));
        assert!(api_request(
            port,
            &format!(
                "/library/items/{item_id}/attachments/sha256-{}.png",
                "0".repeat(64)
            )
        )
        .starts_with("HTTP/1.1 404"));
    }
    #[test]
    fn mobile_cursor_pages_15002_compact_rows_without_omissions() {
        let mut rows = (0..15_002)
            .map(|index| LibraryItem {
                item_id: format!("item_{index:05}"),
                revision_id: format!("revision_{index:05}"),
                item_type: "webpage".into(),
                title: format!("Synthetic item {index}"),
                creators: vec!["Synthetic Author".into()],
                published_date: Some("2026-08-10".into()),
                captured_at: Some(format!("2026-08-10T00:{:02}:00Z", index % 60)),
                captured_at_epoch: Some(index as i64),
                site_name: Some("Synthetic Library".into()),
                manual_tags: vec![format!("tag-{}", index % 20)],
                sensitivity: Sensitivity::Public,
            })
            .collect::<Vec<_>>();
        rows.sort_by(|left, right| {
            right
                .captured_at_epoch
                .cmp(&left.captured_at_epoch)
                .then_with(|| right.item_id.cmp(&left.item_id))
        });
        let mut retained = 0usize;
        for row in &rows {
            reserve_index_budget(&mut retained, index_cost(row)).unwrap();
        }
        assert!(retained < INDEX_BUDGET);
        assert_eq!(
            retained_index_cost(&rows, rows.capacity(), &[], 0).unwrap(),
            retained
        );

        let mut cursor = None;
        let mut found = Vec::new();
        loop {
            let start = cursor_start(&rows, cursor.as_ref(), |row| &row.item_id).unwrap();
            let page = rows.iter().skip(start).take(100).collect::<Vec<_>>();
            if page.is_empty() {
                break;
            }
            found.extend(page.iter().map(|row| row.item_id.clone()));
            cursor = page.last().map(|row| row.item_id.clone());
        }
        assert_eq!(found.len(), 15_002);
        assert_eq!(
            found
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            15_002
        );
        assert_eq!(found.first(), Some(&"item_15001".to_owned()));
        assert_eq!(found.last(), Some(&"item_00000".to_owned()));
    }

    #[test]
    fn mobile_reader_keeps_bilingual_derivatives_revision_bound_and_unambiguous() {
        let temp = tempfile::tempdir().unwrap();
        let library = temp.path().join("library");
        let item_id = ingest_fixture(
            &library,
            Sensitivity::Public,
            "BILINGUAL",
            mobile_metadata("BILINGUAL", &["language"]),
            "# Original heading\n\nOriginal paragraph.\n\nSecond paragraph.".into(),
        );
        generated_one(
            &library,
            &item_id,
            GeneratedTextPurpose::TitleTranslation,
            "en",
            "zh-CN",
            "双语演习",
        );
        generated_one(
            &library,
            &item_id,
            GeneratedTextPurpose::Summary,
            "en",
            "en",
            "A source-bound English summary.",
        );
        generated_one(
            &library,
            &item_id,
            GeneratedTextPurpose::Summary,
            "en",
            "zh-CN",
            "一段受原文约束的中文摘要。",
        );
        let translation_id = generated_one(
            &library,
            &item_id,
            GeneratedTextPurpose::DocumentTranslation,
            "en",
            "zh-CN",
            "# 译文标题\n\n第一段译文。\n\n第二段译文。",
        );
        let server = SurfaceServer::new(SurfaceConfig {
            library: library.clone(),
            max_sensitivity: Sensitivity::Public,
            port: 0,
        })
        .unwrap();

        let source = server.reader_page(&item_id, None).unwrap();
        assert!(source.contains("Original paragraph."));
        assert!(source.contains("A source-bound English summary."));
        assert!(source.contains("gpt-5.6-luna"));
        assert!(source.contains(">中文</a>"));
        assert!(source.contains(">中/英</a>"));

        let chinese = server
            .reader_page(&item_id, Some("language=zh&theme=dark"))
            .unwrap();
        assert!(chinese.contains("双语演习"));
        assert!(chinese.contains("一段受原文约束的中文摘要"));
        assert!(chinese.contains("第一段译文"));
        assert!(!chinese.contains("Original paragraph."));
        assert!(chinese.contains("data-theme=\"dark\""));

        let interleaved = server
            .reader_page(&item_id, Some("language=en-zh&font=serif"))
            .unwrap();
        assert!(interleaved.contains("class=\"parallel-block\""));
        assert!(
            interleaved.find("Original paragraph.").unwrap()
                < interleaved.find("第一段译文").unwrap()
        );
        assert!(interleaved.contains("language=en-zh"));
        assert!(interleaved.contains("data-font=\"serif\""));

        let analysis = serde_json::to_string(&server.analysis(&translation_id).unwrap()).unwrap();
        assert!(analysis.contains("document_translation"));
        assert!(analysis.contains("memex.faithful-translation"));
        assert!(!analysis.contains("prompt text"));
        let metadata_sheet = server
            .library_page(Some(&format!("detail={item_id}")), None)
            .unwrap();
        assert!(metadata_sheet.contains("translated-metadata-title"));
        assert!(metadata_sheet.contains("双语演习"));
        assert!(metadata_sheet.contains("A source-bound English summary."));
        assert!(metadata_sheet.contains("一段受原文约束的中文摘要"));

        generated_one(
            &library,
            &item_id,
            GeneratedTextPurpose::TitleTranslation,
            "en",
            "zh-CN",
            "有歧义的替代标题",
        );
        assert!(matches!(
            server.reader_page(&item_id, Some("language=zh")),
            Err(RouteError::NotFound)
        ));
        let fail_closed = server.reader_page(&item_id, None).unwrap();
        assert!(!fail_closed.contains(">中文</a>"));
        assert!(fail_closed.contains("Original paragraph."));
    }

    #[test]
    fn preview_reader_and_status_apis_follow_live_generated_analysis_states() {
        let temp = tempfile::tempdir().unwrap();
        let library = temp.path().join("library");
        fs::create_dir(&library).unwrap();
        let server = SurfaceServer::new(SurfaceConfig {
            library: library.clone(),
            max_sensitivity: Sensitivity::Public,
            port: 0,
        })
        .unwrap();
        let source_body = "# Original report\n\nThis is the synthetic source article, and the report was written for the test. The article has more detail about the result, which was checked with a bounded fixture and not with private content. This report has the facts from the source, and it was prepared for deterministic verification.";
        let item_id = ingest_fixture(
            &library,
            Sensitivity::Public,
            "TRANSLATION_STATE",
            mobile_metadata("TRANSLATION_STATE", &["translation"]),
            source_body.into(),
        );
        let revision = memex_store::read_item(&library, &item_id).unwrap().1;
        let outcome =
            memex_analysis::enqueue_item_translation(&library, &item_id, &revision.revision_id)
                .unwrap();
        assert!(matches!(
            outcome,
            memex_analysis::TranslationEnqueueOutcome::Enqueued { .. }
        ));
        assert!(matches!(
            memex_analysis::enqueue_item_summary(&library, &item_id, &revision.revision_id)
                .unwrap(),
            memex_analysis::SummaryEnqueueOutcome::Enqueued { .. }
        ));

        let queued_sheet = server
            .library_page(Some(&format!("detail={item_id}")), None)
            .unwrap();
        assert!(queued_sheet.contains("data-translation-state=\"queued\""));
        assert!(queued_sheet.contains("等待翻译"));
        assert!(queued_sheet.contains("data-summary-state=\"queued\""));
        assert!(queued_sheet.contains("等待摘要"));
        assert!(!queued_sheet.contains("正在翻译"));
        assert!(queued_sheet.contains("阅读全文"));
        assert!(!queued_sheet.contains("可验证的中文标题"));
        let queued_reader = server.reader_page(&item_id, None).unwrap();
        assert!(queued_reader.contains("等待翻译"));
        assert!(queued_reader.contains("等待摘要"));
        assert!(queued_reader.contains("Original report"));
        assert!(!queued_reader.contains(">中文</a>"));
        let queued_api = server
            .api_v2(&format!("/api/v2/items/{item_id}/translation"), None)
            .unwrap();
        let queued_api = serde_json::to_string(&queued_api).unwrap();
        assert!(queued_api.contains("\"state\":\"queued\""));
        assert!(!queued_api.contains("Original report"));
        let queued_summary_api = server
            .api_v2(&format!("/api/v2/items/{item_id}/summary"), None)
            .unwrap();
        let queued_summary_api = serde_json::to_string(&queued_summary_api).unwrap();
        assert!(queued_summary_api.contains("\"state\":\"queued\""));
        assert!(!queued_summary_api.contains("Original report"));

        let mut queue = TranslationQueue::open(&library).unwrap();
        let mut timeout = SurfaceTranslationModel {
            error: Some(TranslationModelError::Timeout),
        };
        assert!(queue.process_next(&mut timeout).unwrap());
        let failed_sheet = server
            .library_page(Some(&format!("detail={item_id}")), None)
            .unwrap();
        assert!(failed_sheet.contains("data-translation-state=\"failed\""));
        assert!(failed_sheet.contains("翻译失败"));
        assert!(failed_sheet.contains("可安全重试"));
        assert!(server
            .reader_page(&item_id, None)
            .unwrap()
            .contains("Original report"));

        let mut summary_queue = SummaryQueue::open(&library).unwrap();
        let mut summary_timeout = SurfaceSummaryModel {
            error: Some(SummaryModelError::Timeout),
        };
        assert!(summary_queue.process_next(&mut summary_timeout).unwrap());
        let failed_summary_sheet = server
            .library_page(Some(&format!("detail={item_id}")), None)
            .unwrap();
        assert!(failed_summary_sheet.contains("data-summary-state=\"failed\""));
        assert!(failed_summary_sheet.contains("摘要失败"));
        assert!(!failed_summary_sheet.contains("经过校验的摘要"));

        let failed_summary =
            memex_analysis::summary_status(&library, &item_id, &revision.revision_id)
                .unwrap()
                .unwrap();
        summary_queue.retry(&failed_summary.job_id).unwrap();
        assert!(summary_queue
            .process_next(&mut SurfaceSummaryModel { error: None })
            .unwrap());

        let failed = memex_analysis::translation_status(&library, &item_id, &revision.revision_id)
            .unwrap()
            .unwrap();
        queue.retry(&failed.job_id).unwrap();
        let mut success = SurfaceTranslationModel { error: None };
        assert!(queue.process_next(&mut success).unwrap());

        let completed_sheet = server
            .library_page(Some(&format!("detail={item_id}")), None)
            .unwrap();
        assert!(completed_sheet.contains("data-translation-state=\"succeeded\""));
        assert!(completed_sheet.contains("翻译完成"));
        assert!(completed_sheet.contains("可验证的中文标题"));
        assert!(completed_sheet.contains("data-summary-state=\"succeeded\""));
        assert!(completed_sheet.contains("摘要完成"));
        assert!(completed_sheet.contains("经过校验的摘要"));
        let source_reader = server.reader_page(&item_id, None).unwrap();
        assert!(source_reader.contains("Original report"));
        assert!(source_reader.contains(">中文</a>"));
        let chinese_reader = server.reader_page(&item_id, Some("language=zh")).unwrap();
        assert!(chinese_reader.contains("这是通过测试模型生成"));
        assert!(!chinese_reader.contains("Original report"));
        let succeeded_api = server
            .api_v2(&format!("/api/v2/items/{item_id}/translation"), None)
            .unwrap();
        assert_eq!(succeeded_api["data"]["translation"]["state"], "succeeded");
        let succeeded_summary_api = server
            .api_v2(&format!("/api/v2/items/{item_id}/summary"), None)
            .unwrap();
        assert_eq!(
            succeeded_summary_api["data"]["summary"]["state"],
            "succeeded"
        );

        drop(queue);
        drop(summary_queue);
        fs::remove_file(library.join("analysis/translation-jobs.sqlite3")).unwrap();
        let orphaned_reader = server.reader_page(&item_id, None).unwrap();
        assert!(orphaned_reader.contains("Original report"));
        assert!(!orphaned_reader.contains(">中文</a>"));
        assert!(matches!(
            server.reader_page(&item_id, Some("language=zh")),
            Err(RouteError::NotFound)
        ));
    }

    #[test]
    fn generated_text_rejects_unsupported_competing_source_languages() {
        for purpose in [
            GeneratedTextPurpose::TitleTranslation,
            GeneratedTextPurpose::Summary,
            GeneratedTextPurpose::DocumentTranslation,
        ] {
            let temp = tempfile::tempdir().unwrap();
            let library = temp.path().join("library");
            let item_id = ingest_fixture(
                &library,
                Sensitivity::Public,
                &format!("UNSUPPORTED_{purpose:?}"),
                ItemMetadata::default(),
                "Source body.".into(),
            );
            generated_one(&library, &item_id, purpose, "en", "zh-CN", "受支持的结果");
            generated_one(
                &library,
                &item_id,
                purpose,
                "fr",
                "zh-CN",
                "不应被静默忽略的冲突结果",
            );
            let server = SurfaceServer::new(SurfaceConfig {
                library,
                max_sensitivity: Sensitivity::Public,
                port: 0,
            })
            .unwrap();
            let revision = memex_store::read_item(&server.config.library, &item_id)
                .unwrap()
                .1;
            let generated = server.generated_text_for_item(&item_id, &revision).unwrap();
            assert!(generated.source_language.is_none());
            assert!(generated.title_translations.is_empty());
            assert!(generated.summaries.is_empty());
            assert!(generated.document_translations.is_empty());
        }
    }

    #[test]
    fn generated_text_rejects_competing_source_language_variants() {
        let temp = tempfile::tempdir().unwrap();
        let library = temp.path().join("library");
        let item_id = ingest_fixture(
            &library,
            Sensitivity::Public,
            "LANGUAGE_VARIANTS",
            ItemMetadata::default(),
            "Source body.".into(),
        );
        generated_one(
            &library,
            &item_id,
            GeneratedTextPurpose::TitleTranslation,
            "en-US",
            "zh-CN",
            "美国英语来源",
        );
        generated_one(
            &library,
            &item_id,
            GeneratedTextPurpose::Summary,
            "en-GB",
            "zh-CN",
            "英国英语来源",
        );
        let server = SurfaceServer::new(SurfaceConfig {
            library,
            max_sensitivity: Sensitivity::Public,
            port: 0,
        })
        .unwrap();
        let revision = memex_store::read_item(&server.config.library, &item_id)
            .unwrap()
            .1;
        let generated = server.generated_text_for_item(&item_id, &revision).unwrap();
        assert!(generated.source_language.is_none());
        assert!(generated.title_translations.is_empty());
        assert!(generated.summaries.is_empty());
        assert!(generated.document_translations.is_empty());
    }

    #[test]
    fn generated_text_rejects_revision_digest_and_evidence_drift() {
        let temp = tempfile::tempdir().unwrap();
        let library = temp.path().join("library");
        let item_id = ingest_fixture(
            &library,
            Sensitivity::Public,
            "REVISION_BINDING",
            ItemMetadata::default(),
            "Source body.".into(),
        );
        generated_one(
            &library,
            &item_id,
            GeneratedTextPurpose::Summary,
            "en",
            "en",
            "Bound summary.",
        );
        let server = SurfaceServer::new(SurfaceConfig {
            library,
            max_sensitivity: Sensitivity::Public,
            port: 0,
        })
        .unwrap();
        let revision = memex_store::read_item(&server.config.library, &item_id)
            .unwrap()
            .1;
        assert_eq!(
            server
                .generated_text_for_item(&item_id, &revision)
                .unwrap()
                .source_language
                .as_deref(),
            Some("en")
        );

        let mut content_drift = revision.clone();
        content_drift.content_markdown.push_str(" changed");
        assert!(server
            .generated_text_for_item(&item_id, &content_drift)
            .unwrap()
            .source_language
            .is_none());

        let mut evidence_drift = revision;
        evidence_drift.evidence_ids[0] = "evidence_changed".into();
        assert!(server
            .generated_text_for_item(&item_id, &evidence_drift)
            .unwrap()
            .source_language
            .is_none());
    }

    #[test]
    fn mobile_api_human_sheet_reader_and_assets_keep_content_split() {
        let temp = tempfile::tempdir().unwrap();
        let library = temp.path().join("library");
        let first = ingest_fixture(
            &library,
            Sensitivity::Public,
            "MOBILE_A",
            mobile_metadata("MOBILE_A", &["alpha", "shared"]),
            "# 第一章\n\nBODY_ONLY_A mobile-library-v0.css\n\n<b>RAW_HTML</b>\n\n[bad](javascript:alert(1)) ![ALT_ONLY](https://example.invalid/image.png)".into(),
        );
        let second = ingest_fixture(
            &library,
            Sensitivity::Public,
            "MOBILE_B",
            mobile_metadata("MOBILE_B", &["beta", "shared"]),
            "BODY_ONLY_B".into(),
        );
        let third = ingest_fixture(
            &library,
            Sensitivity::Public,
            "MOBILE_C",
            mobile_metadata("MOBILE_C", &["shared"]),
            "BODY_ONLY_C".into(),
        );
        let hidden = ingest_fixture(
            &library,
            Sensitivity::Restricted,
            "MOBILE_PRIVATE",
            mobile_metadata("MOBILE_PRIVATE", &["private"]),
            "PRIVATE_BODY_SENTINEL".into(),
        );
        let server = SurfaceServer::new(SurfaceConfig {
            library: library.clone(),
            max_sensitivity: Sensitivity::Public,
            port: 0,
        })
        .unwrap();

        let first_page = server.api_v2("/api/v2/items", Some("limit=1")).unwrap();
        assert_eq!(first_page["data"]["items"].as_array().unwrap().len(), 1);
        assert_eq!(first_page["data"]["visible_count"], 3);
        let cursor = first_page["data"]["next_cursor"].as_str().unwrap();
        let second_page = server
            .api_v2("/api/v2/items", Some(&format!("limit=1&cursor={cursor}")))
            .unwrap();
        assert_eq!(second_page["data"]["items"].as_array().unwrap().len(), 1);
        assert!(server
            .api_v2("/api/v2/items", Some("limit=1&limit=2"))
            .is_err());
        assert!(matches!(
            server.api_v2("/api/v2/items", Some("tag=missing")),
            Err(RouteError::NotFound)
        ));
        assert!(server
            .api_v2("/api/v2/items", Some("cursor=item_missing"))
            .is_err());
        let shared = server
            .api_v2("/api/v2/items", Some("tag=shared&limit=100"))
            .unwrap();
        assert_eq!(shared["data"]["visible_count"], 3);
        let tags = server.api_v2("/api/v2/tags", Some("limit=100")).unwrap();
        let tag_names = tags["data"]["tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value["tag"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(tag_names, vec!["alpha", "beta", "shared"]);

        let summary = serde_json::to_string(&shared).unwrap();
        for forbidden in [
            "BODY_ONLY",
            "PRIVATE_BODY_SENTINEL",
            "source_url",
            "canonical_url",
            "abstract_text",
            "provenance",
            "attachments",
        ] {
            assert!(!summary.contains(forbidden), "summary leaked {forbidden}");
        }
        let metadata = server
            .api_v2(&format!("/api/v2/items/{first}/metadata"), None)
            .unwrap();
        let metadata = serde_json::to_string(&metadata).unwrap();
        assert!(metadata.contains("ABSTRACT_ONLY"));
        assert!(metadata.contains("captured_at"));
        assert!(!metadata.contains("BODY_ONLY_A"));
        assert!(!metadata.contains("evidence_ids"));
        assert!(!metadata.contains("attachments"));
        let content = server
            .api_v2(&format!("/api/v2/items/{first}/content"), None)
            .unwrap();
        let content = serde_json::to_string(&content).unwrap();
        assert!(content.contains("BODY_ONLY_A"));
        assert!(!content.contains("ABSTRACT_ONLY"));
        assert!(!content.contains("metadata"));

        let sheet = server
            .library_page(Some(&format!("detail={first}&theme=dark&font=serif")), None)
            .unwrap();
        assert!(sheet.contains("class=\"inspector sheet\""));
        assert!(sheet.contains("class=\"theme-switch\""));
        assert!(sheet.contains("data-theme=\"dark\""));
        assert!(sheet.contains("data-value=\"light\""));
        assert!(sheet.contains("data-value=\"dark\""));
        assert!(sheet.contains("aria-current=\"true\""));
        assert!(sheet.contains("采集时间"));
        assert!(sheet.contains("ABSTRACT_ONLY"));
        assert!(sheet.contains("阅读全文"));
        assert!(!sheet.contains("BODY_ONLY_A"));
        assert!(!sheet.contains("PRIVATE_BODY_SENTINEL"));
        let reader = server
            .reader_page(&first, Some("theme=dark&font=serif"))
            .unwrap();
        assert!(reader.contains("BODY_ONLY_A"));
        assert!(reader.contains("<h1>第一章</h1>"));
        assert!(reader.contains("日间"));
        assert!(reader.contains("夜间"));
        assert!(!reader.contains("纸张"));
        assert!(reader.contains("data-theme=\"dark\""));
        assert!(matches!(
            server.library_page(Some("theme=sepia"), None),
            Err(RouteError::NotFound)
        ));
        assert!(matches!(
            server.reader_page(&first, Some("theme=sepia")),
            Err(RouteError::NotFound)
        ));
        assert!(reader.contains("ALT_ONLY"));
        assert!(!reader.contains("<b>"));
        assert!(!reader.contains("javascript:"));
        assert!(!reader.contains("<img"));

        let hidden_error = server
            .api_v2(&format!("/api/v2/items/{hidden}/metadata"), None)
            .unwrap_err();
        let missing_error = server
            .api_v2("/api/v2/items/item_missing/metadata", None)
            .unwrap_err();
        assert!(matches!(
            (hidden_error, missing_error),
            (RouteError::NotFound, RouteError::NotFound)
        ));
        let mobile = load_mobile_index(&server.config).unwrap();
        assert_eq!(mobile.items.len(), 3);
        assert!(mobile.items.iter().any(|row| row.item_id == second));
        assert!(mobile.items.iter().any(|row| row.item_id == third));
        assert!(!mobile.items.iter().any(|row| row.item_id == hidden));

        let port = server.port();
        thread::spawn(move || server.serve().unwrap());
        let css = api_request(port, "/assets/mobile-library-v0.css");
        assert!(css.starts_with("HTTP/1.1 200"));
        assert!(css.contains("Content-Type: text/css; charset=utf-8"));
        assert!(css.contains("Cache-Control: no-store"));
        assert!(css.contains("X-Content-Type-Options: nosniff"));
        assert!(api_request(port, "/assets/mobile-library-v0.css?x=1").starts_with("HTTP/1.1 404"));
        let library_page = api_request(port, "/library");
        assert!(library_page.contains("script-src 'self'"));
        assert!(library_page.contains("connect-src 'self'"));
        assert!(library_page.contains("form-action 'none'"));
        assert!(!library_page.contains("unsafe-inline"));
        let legacy_item = api_request(port, &format!("/items/{first}"));
        assert!(legacy_item.contains("style-src 'unsafe-inline'"));
        assert!(!legacy_item.contains("script-src 'self'"));
        let hidden = api_request(port, &format!("/api/v2/items/{hidden}/metadata"));
        let missing = api_request(port, "/api/v2/items/item_missing/metadata");
        assert_eq!(hidden.lines().next(), missing.lines().next());
        assert_eq!(
            hidden.split("\r\n\r\n").nth(1),
            missing.split("\r\n\r\n").nth(1)
        );
    }
    #[test]
    fn binds_ipv4_loopback_with_ephemeral_port() {
        let temp = tempfile::tempdir().unwrap();
        let server = SurfaceServer::new(SurfaceConfig {
            library: temp.path().to_path_buf(),
            max_sensitivity: Sensitivity::Public,
            port: 0,
        })
        .unwrap();
        assert!(matches!(
            server.api_v2("/api/v2/items", Some("tag=missing")),
            Err(RouteError::NotFound)
        ));
        assert!(server.port() > 0);
        assert!(server
            .startup_receipt()
            .url
            .starts_with("http://127.0.0.1:"));
    }

    #[test]
    fn authenticated_orb_portal_origin_is_explicit_and_exact() {
        let public_url = "https://t-preview-p123.onamp.dev/";
        for invalid in [
            "http://t-preview-p123.onamp.dev/",
            "https://t-preview-p123.onamp.dev/path",
            "https://t-preview-p123.onamp.dev/?query=1",
            "https://t-preview-p123.onamp.dev:444/",
            "https://user@t-preview-p123.onamp.dev/",
            "https://example.invalid/",
        ] {
            assert!(parse_orb_portal_origin(invalid).is_err(), "{invalid}");
        }
        let library = tempfile::tempdir().unwrap().keep();
        let server = SurfaceServer::new_for_orb_portal(
            SurfaceConfig {
                library,
                max_sensitivity: Sensitivity::Public,
                port: 0,
            },
            public_url,
        )
        .unwrap();
        assert_eq!(server.startup_receipt().url, public_url);
        let port = server.port();
        thread::spawn(move || server.serve().unwrap());

        let portal = request(
            port,
            "GET /api/v1/health HTTP/1.1\r\nHost: t-preview-p123.onamp.dev\r\nConnection: close\r\n\r\n",
        );
        assert!(portal.starts_with("HTTP/1.1 200"));
        let portal_origin = request(
            port,
            "GET /api/v1/health HTTP/1.1\r\nHost: t-preview-p123.onamp.dev\r\nOrigin: https://t-preview-p123.onamp.dev\r\nConnection: close\r\n\r\n",
        );
        assert!(portal_origin.starts_with("HTTP/1.1 200"));
        let wrong_origin = request(
            port,
            "GET /api/v1/health HTTP/1.1\r\nHost: t-preview-p123.onamp.dev\r\nOrigin: https://other.onamp.dev\r\nConnection: close\r\n\r\n",
        );
        assert!(wrong_origin.starts_with("HTTP/1.1 404"));
    }

    #[test]
    fn host_origin_method_head_and_cors_contract() {
        let port = live_server();
        let ok = request(port, &format!("GET /api/v1/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"));
        assert!(ok.starts_with("HTTP/1.1 200"));
        assert!(!ok.contains("Access-Control-Allow-Origin"));
        let bad_host = request(
            port,
            "GET /api/v1/health HTTP/1.1\r\nHost: evil.invalid\r\nConnection: close\r\n\r\n",
        );
        assert!(bad_host.starts_with("HTTP/1.1 404"));
        let bad_origin = request(port, &format!("GET /api/v1/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: https://evil.invalid\r\nConnection: close\r\n\r\n"));
        assert!(bad_origin.starts_with("HTTP/1.1 404"));
        let post = request(port, &format!("POST /api/v1/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: http://127.0.0.1:{port}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"));
        assert!(post.starts_with("HTTP/1.1 405"));
        assert!(post.contains("Allow: GET, HEAD"));
        let head = request(port, &format!("HEAD /api/v1/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"));
        assert!(head.starts_with("HTTP/1.1 200"));
        assert!(!head.contains("\r\n\r\n{\""));
        let get_length = ok.split("\r\n\r\n").nth(1).unwrap().len();
        assert!(head.contains(&format!("Content-Length: {get_length}")));
        assert_eq!(head.split("\r\n\r\n").nth(1).unwrap(), "");
        let duplicate_host = request(port, &format!("GET /api/v1/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n"));
        assert!(duplicate_host.starts_with("HTTP/1.1 404"));
        let duplicate_origin = request(port, &format!("GET /api/v1/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: http://127.0.0.1:{port}\r\nOrigin: http://127.0.0.1:{port}\r\nConnection: close\r\n\r\n"));
        assert!(duplicate_origin.starts_with("HTTP/1.1 404"));
        let bad_host_post = request(
            port,
            "POST /api/v1/health HTTP/1.1\r\nHost: evil.invalid\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        assert!(bad_host_post.starts_with("HTTP/1.1 404"));
    }

    #[test]
    fn strict_routes_and_queries_are_rejected() {
        let port = live_server();
        for path in [
            "/api/v1/items/x/extra",
            "/api/v1/items/x%2fextra",
            "/api/v1/items/%zz",
            "/api/v1/items?x=1",
            "/api/v1/items?",
            "/api/v1/recall?q=x&q=y",
            "/api/v1/recall?q=x&limit=no",
            "/api/v1/recall?q=x&unknown=1",
            "/api/v1/recall",
            "/api/v1/recall?q=",
            "/api/v1/recall?q=%00",
            "/api/v1/recall?q=%FF",
            "/api/v1/recall?q=%C3%28",
            "/api/v1",
            "/api/v1/",
            "/api/v1x/health",
            "/health",
            "/catalog",
        ] {
            assert!(
                api_request(port, path).starts_with("HTTP/1.1 404"),
                "{path}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_optional_parents_and_empty_indexes_are_internal_failures() {
        use std::os::unix::fs::symlink;

        let server_for = |library: PathBuf| {
            SurfaceServer::new(SurfaceConfig {
                library,
                max_sensitivity: Sensitivity::Public,
                port: 0,
            })
            .unwrap()
        };

        let item_root = tempfile::tempdir().unwrap();
        symlink(
            item_root.path().join("missing"),
            item_root.path().join("items"),
        )
        .unwrap();
        let item_server = server_for(item_root.path().to_path_buf());
        assert!(matches!(
            item_server.route("/api/v1/items/item_missing", None),
            Err(RouteError::Internal)
        ));
        assert!(matches!(
            item_server.route("/library", None),
            Err(RouteError::Internal)
        ));
        assert!(matches!(
            item_server.route("/api/v2/items", None),
            Err(RouteError::Internal)
        ));

        let registry_root = tempfile::tempdir().unwrap();
        symlink(
            registry_root.path().join("missing"),
            registry_root.path().join("registry"),
        )
        .unwrap();
        let source_id = format!("legacy_source_{}", "a".repeat(64));
        assert!(matches!(
            server_for(registry_root.path().to_path_buf())
                .route(&format!("/api/v1/sources/{source_id}"), None),
            Err(RouteError::Internal)
        ));

        let analysis_root = tempfile::tempdir().unwrap();
        fs::create_dir(analysis_root.path().join("analysis")).unwrap();
        fs::create_dir(analysis_root.path().join("analysis/records")).unwrap();
        symlink(
            analysis_root.path().join("missing"),
            analysis_root.path().join("analysis/index.json"),
        )
        .unwrap();
        assert!(matches!(
            server_for(analysis_root.path().to_path_buf()).route("/api/v1/analyses", None),
            Err(RouteError::Internal)
        ));

        let publication_root = tempfile::tempdir().unwrap();
        fs::create_dir(publication_root.path().join("publications")).unwrap();
        fs::create_dir(publication_root.path().join("publications/records")).unwrap();
        symlink(
            publication_root.path().join("missing"),
            publication_root.path().join("publications/index.json"),
        )
        .unwrap();
        assert!(matches!(
            server_for(publication_root.path().to_path_buf()).route("/api/v1/publications", None),
            Err(RouteError::Internal)
        ));
    }

    #[test]
    fn html_escapes_title_body_query_and_route_display() {
        let value = json!({"title":"<script>bad</script>","body_markdown":"<img onerror=bad>","query":"\"'><script>bad</script>"});
        let page = html_page("/items/<script>bad</script>", &value);
        assert!(!page.contains("<script>bad</script>"));
        assert!(!page.contains("<img onerror=bad>"));
        assert!(page.contains("&lt;script&gt;bad&lt;/script&gt;"));
        assert!(page.contains("&quot;&#x27;&gt;&lt;script&gt;bad&lt;/script&gt;"));
    }

    #[test]
    fn human_pages_include_get_recall_form_and_restrictive_csp() {
        let page = html_page("/items", &json!({}));
        assert!(page.contains("<form action=\"/recall\" method=\"get\">"));
        assert!(page.contains("name=\"q\" required"));
        assert!(page.contains("name=\"limit\" min=\"1\" max=\"100\" value=\"20\""));
        let port = live_server();
        let response = api_request(port, "/");
        assert!(response.contains("form-action 'self'"));
        assert!(response.contains("base-uri 'none'"));
        assert!(response.contains("frame-ancestors 'none'"));
    }

    #[test]
    fn internal_source_is_hidden_at_public_and_matches_missing_detail() {
        let temp = tempfile::tempdir().unwrap();
        let package = temp.path().join("package");
        let library = temp.path().join("library");
        fs::create_dir(&package).unwrap();
        fs::write(package.join("source.json"), r#"{"source_id":"fixture.source","name":"INTERNAL_ONLY_MARKER","url":"https://example.invalid/source","category":"technology","topic_tags":[],"region":"global","language":"en","access_type":"public","capture_mode":"browser","evidence_role":["primary"],"status":{"state":"legacy-unknown"}}"#).unwrap();
        let prepared =
            memex_registry::preflight(&package, &[std::path::PathBuf::from("source.json")])
                .unwrap()
                .pop()
                .unwrap();
        let id = prepared.record.source_id.clone();
        memex_registry::apply(&library, prepared).unwrap();
        let public = SurfaceServer::new(SurfaceConfig {
            library: library.clone(),
            max_sensitivity: Sensitivity::Public,
            port: 0,
        })
        .unwrap();
        let (_, list) = public.route("/api/v1/sources", None).unwrap();
        assert!(!list.contains(&id) && !list.contains("INTERNAL_ONLY_MARKER"));
        let hidden = public
            .route(&format!("/api/v1/sources/{id}"), None)
            .unwrap_err();
        let missing = public
            .route("/api/v1/sources/no_such_source", None)
            .unwrap_err();
        assert!(matches!(
            (hidden, missing),
            (RouteError::NotFound, RouteError::NotFound)
        ));
        let internal = SurfaceServer::new(SurfaceConfig {
            library,
            max_sensitivity: Sensitivity::Internal,
            port: 0,
        })
        .unwrap();
        let detail = internal
            .route(&format!("/api/v1/sources/{id}"), None)
            .unwrap()
            .1;
        assert!(detail.contains(&id));
        for forbidden in [
            "external_source_id",
            "provenance",
            "adapter_id",
            "input_sha256",
            "preferred_extractor",
        ] {
            assert!(!detail.contains(forbidden), "leaked {forbidden}");
        }
    }

    #[test]
    fn public_ceiling_mixed_sensitivity_fixture_is_complete_and_non_enumerating() {
        let temp = tempfile::tempdir().unwrap();
        let package = temp.path().join("package");
        let library = temp.path().join("library");
        fs::create_dir(&package).unwrap();
        fs::write(package.join("internal-source.json"), r#"{"source_id":"fixture.internal-source","name":"INTERNAL_SOURCE_MARKER","url":"https://example.invalid/source","category":"technology","topic_tags":[],"region":"global","language":"en","access_type":"public","capture_mode":"browser","evidence_role":["primary"],"status":{"state":"legacy-unknown"}}"#).unwrap();
        let internal_source = memex_registry::preflight(
            &package,
            &[std::path::PathBuf::from("internal-source.json")],
        )
        .unwrap()
        .pop()
        .unwrap();
        let source_id = internal_source.record.source_id.clone();
        memex_registry::apply(&library, internal_source).unwrap();

        let public_item = ingest_one(&library, Sensitivity::Public, "PUBLIC_ITEM_MARKER");
        let restricted_item =
            ingest_one(&library, Sensitivity::Restricted, "RESTRICTED_ITEM_MARKER");
        let public_analysis = analysis_one(
            &library,
            Sensitivity::Public,
            public_item.clone(),
            "PUBLIC_RECALL_MARKER",
        );
        let restricted_analysis = analysis_one(
            &library,
            Sensitivity::Restricted,
            restricted_item.clone(),
            "RESTRICTED_RECALL_MARKER",
        );
        let public_publication = publication_one(
            &library,
            Sensitivity::Public,
            public_analysis.clone(),
            "PUBLIC_PUBLICATION_MARKER",
        );
        let restricted_publication = publication_one(
            &library,
            Sensitivity::Restricted,
            restricted_analysis.clone(),
            "RESTRICTED_PUBLICATION_MARKER",
        );

        let (_, restricted_revision) = memex_store::read_item(&library, &restricted_item).unwrap();
        fs::write(
            library
                .join("items")
                .join(&restricted_item)
                .join("revisions")
                .join(format!("{}.json", restricted_revision.revision_id)),
            [0xff],
        )
        .unwrap();
        fs::write(
            library
                .join("analysis/records")
                .join(format!("{restricted_analysis}.json")),
            [0xff],
        )
        .unwrap();
        fs::write(
            library
                .join("publications/records")
                .join(format!("{restricted_publication}.json")),
            [0xff],
        )
        .unwrap();

        let public = SurfaceServer::new(SurfaceConfig {
            library: library.clone(),
            max_sensitivity: Sensitivity::Public,
            port: 0,
        })
        .unwrap();
        let port = public.port();
        thread::spawn(move || public.serve().unwrap());
        let catalog = api_request(port, "/api/v1/catalog");
        assert!(
            catalog.contains("\"source_count\":0")
                && catalog.contains("\"item_count\":1")
                && catalog.contains("\"analysis_count\":1")
                && catalog.contains("\"publication_count\":1")
        );
        let sources = api_request(port, "/api/v1/sources");
        assert!(!sources.contains(&source_id) && !sources.contains("INTERNAL_SOURCE_MARKER"));
        for (list, visible, hidden) in [
            ("/api/v1/items", &public_item, &restricted_item),
            ("/api/v1/analyses", &public_analysis, &restricted_analysis),
            (
                "/api/v1/publications",
                &public_publication,
                &restricted_publication,
            ),
        ] {
            let body = api_request(port, list);
            let root_marker = temp.path().to_string_lossy();
            for forbidden in [
                "provenance",
                "backend_ref",
                "evidence_ids",
                "config_digest",
                root_marker.as_ref(),
            ] {
                assert!(
                    !body.contains(forbidden),
                    "leaked forbidden field {forbidden}"
                );
            }
            assert!(
                body.contains(visible),
                "{list} must contain visible fixture"
            );
            assert!(
                !body.contains(hidden),
                "{list} must not contain hidden fixture"
            );
        }
        for (route, visible) in [
            (format!("/api/v1/items/{public_item}"), public_item.as_str()),
            (
                format!("/api/v1/analyses/{public_analysis}"),
                public_analysis.as_str(),
            ),
            (
                format!("/api/v1/publications/{public_publication}"),
                public_publication.as_str(),
            ),
        ] {
            let body = api_request(port, &route);
            assert!(body.starts_with("HTTP/1.1 200") && body.contains(visible));
        }
        let recall = api_request(port, "/api/v1/recall?q=PUBLIC_RECALL_MARKER&limit=10");
        assert!(recall.contains("PUBLIC_RECALL_MARKER"));
        assert!(!recall.contains("RESTRICTED_RECALL_MARKER"));
        for route in [
            format!("/api/v1/sources/{source_id}"),
            format!("/api/v1/items/{restricted_item}"),
            format!("/api/v1/analyses/{restricted_analysis}"),
            format!("/api/v1/publications/{restricted_publication}"),
        ] {
            let hidden = api_request(port, &route);
            let missing = api_request(port, &format!("{route}.nonexistent"));
            assert_eq!(hidden.lines().next(), missing.lines().next(), "{route}");
            assert_eq!(
                hidden.split("\r\n\r\n").nth(1),
                missing.split("\r\n\r\n").nth(1),
                "hidden and missing bodies must be identical: {route}"
            );
            for header in [
                "Content-Type: application/json",
                "Cache-Control: no-store",
                "X-Content-Type-Options: nosniff",
            ] {
                assert!(
                    hidden.contains(header) && missing.contains(header),
                    "{route}"
                );
            }
        }
        let internal = SurfaceServer::new(SurfaceConfig {
            library: library.clone(),
            max_sensitivity: Sensitivity::Internal,
            port: 0,
        })
        .unwrap();
        assert!(internal
            .route(&format!("/api/v1/sources/{source_id}"), None)
            .unwrap()
            .1
            .contains(&source_id));

        fs::write(
            library
                .join("analysis/records")
                .join(format!("{public_analysis}.json")),
            [0xff],
        )
        .unwrap();
        let corrupt = api_request(port, &format!("/api/v1/analyses/{public_analysis}"));
        assert!(corrupt.starts_with("HTTP/1.1 500"));

        let (_, public_revision) = memex_store::read_item(&library, &public_item).unwrap();
        fs::write(
            library
                .join("items")
                .join(&public_item)
                .join("revisions")
                .join(format!("{}.json", public_revision.revision_id)),
            [0xff],
        )
        .unwrap();
        let corrupt = api_request(port, &format!("/api/v1/items/{public_item}"));
        assert!(corrupt.starts_with("HTTP/1.1 500"));

        fs::write(
            library
                .join("publications/records")
                .join(format!("{public_publication}.json")),
            [0xff],
        )
        .unwrap();
        let corrupt = api_request(port, &format!("/api/v1/publications/{public_publication}"));
        assert!(corrupt.starts_with("HTTP/1.1 500"));

        let corrupt_release = format!("release_{}", "a".repeat(64));
        fs::create_dir_all(library.join("publications/releases")).unwrap();
        fs::write(
            library
                .join("publications/releases")
                .join(format!("{corrupt_release}.json")),
            [0xff],
        )
        .unwrap();
        let corrupt = api_request(port, &format!("/api/v1/public-releases/{corrupt_release}"));
        assert!(corrupt.starts_with("HTTP/1.1 500"));
        let missing_release = format!("release_{}", "b".repeat(64));
        let missing = api_request(port, &format!("/api/v1/public-releases/{missing_release}"));
        assert!(missing.starts_with("HTTP/1.1 404"));
    }
}
