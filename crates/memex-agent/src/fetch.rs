use anyhow::{bail, Result};
use flate2::read::{GzDecoder, ZlibDecoder};
use serde::Deserialize;
use std::{
    collections::BTreeSet,
    io::Read,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Duration,
};
use url::Url;

pub const MAX_BODY: usize = 8 * 1024 * 1024;
/// One image a source handler selected (article GIFs are often several MiB).
pub const MAX_EXACT_IMAGE: usize = 16 * 1024 * 1024;
// Reqwest/hyper rejects more than 100 HTTP/1 response headers before returning
// a response; the stricter application bound is checked again below.
pub const MAX_HEADERS: usize = 96;
pub const MAX_HEADER_BYTES: usize = 64 * 1024;
pub const MAX_RESOLVED_ADDRESSES: usize = 64;
const MAX_DECOMPRESSION_QUEUE: usize = 8;
pub trait Resolver: Send + Sync {
    fn resolve(&self, host: &str, timeout: Duration) -> Result<Vec<IpAddr>>;
}
pub trait HttpTransport: Send + Sync {
    fn get(&self, url: &Url, pinned: &[SocketAddr], timeout: Duration) -> Result<RawResponse>;
}
pub struct RawResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}
#[derive(Debug)]
pub struct Fetched {
    pub status: u16,
    pub final_url: Url,
    pub media_type: String,
    pub body: Vec<u8>,
    pub redirects: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchDisposition {
    InteractiveLogin,
    Unsupported,
    Retryable,
    FatalWorker,
}
#[derive(Debug)]
pub struct FetchFailure {
    code: &'static str,
    disposition: FetchDisposition,
    retry_after: Option<(String, i64)>,
}
impl FetchFailure {
    pub fn disposition(&self) -> FetchDisposition {
        self.disposition
    }

    pub fn reason_code(&self) -> &'static str {
        self.code
    }
    pub fn retry_after(&self) -> Option<(&str, i64)> {
        self.retry_after
            .as_ref()
            .map(|(origin, at)| (origin.as_str(), *at))
    }
}
impl std::fmt::Display for FetchFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.code)
    }
}
impl std::error::Error for FetchFailure {}
impl From<anyhow::Error> for FetchFailure {
    fn from(error: anyhow::Error) -> Self {
        if let Some(deferred) = error.downcast_ref::<OriginDeferred>() {
            return Self {
                code: "polite_wait",
                disposition: FetchDisposition::Retryable,
                retry_after: Some((deferred.origin.clone(), deferred.not_before)),
            };
        }
        if let Some(limited) = error.downcast_ref::<RateLimited>() {
            return Self {
                code: "rate_limited",
                disposition: FetchDisposition::Retryable,
                retry_after: Some((limited.origin.clone(), limited.not_before)),
            };
        }
        let code = error.to_string();
        match code.as_str() {
            "interactive_login_required" => Self {
                code: "interactive_login_required",
                disposition: FetchDisposition::InteractiveLogin,
                retry_after: None,
            },
            "dns_timeout"
            | "dns_busy"
            | "dns_unavailable"
            | "decompression_timeout"
            | "decompression_busy"
            | "decompression_unavailable" => Self {
                code: "fetch_worker_failed",
                disposition: FetchDisposition::FatalWorker,
                retry_after: None,
            },
            "transport_error" | "fetch_deadline" | "dns_error" | "http_status_retryable" => Self {
                code: "fetch_retry",
                disposition: FetchDisposition::Retryable,
                retry_after: None,
            },
            "chrome_unavailable" | "page_changed" | "image_too_large" => Self {
                code: match code.as_str() {
                    "page_changed" => "page_changed",
                    "image_too_large" => "image_too_large",
                    _ => "chrome_unavailable",
                },
                disposition: FetchDisposition::Unsupported,
                retry_after: None,
            },
            "address_not_public" => Self {
                code: "address_not_public",
                disposition: FetchDisposition::Unsupported,
                retry_after: None,
            },
            "url_malformed"
            | "host_missing"
            | "url_policy_rejected"
            | "url_port_forbidden"
            | "redirect_limit"
            | "redirect_location_missing"
            | "redirect_location_invalid"
            | "redirect_downgrade" => Self {
                code: "network_policy_rejected",
                disposition: FetchDisposition::Unsupported,
                retry_after: None,
            },
            _ => Self {
                code: "unsupported_content",
                disposition: FetchDisposition::Unsupported,
                retry_after: None,
            },
        }
    }
}

#[derive(Debug)]
pub(crate) struct RateLimited {
    pub(crate) origin: String,
    pub(crate) not_before: i64,
}
impl std::fmt::Display for RateLimited {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("rate_limited")
    }
}
impl std::error::Error for RateLimited {}

#[derive(Debug)]
pub(crate) struct OriginDeferred {
    pub origin: String,
    pub not_before: i64,
}
impl std::fmt::Display for OriginDeferred {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("polite_wait")
    }
}
impl std::error::Error for OriginDeferred {}

fn retry_after_at(value: &str, now: i64) -> Option<i64> {
    let value = value.trim();
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        // Keep unrepresentably long, valid delays in the future rather than retry early.
        return Some(now.saturating_add(value.parse::<i64>().unwrap_or(i64::MAX)));
    }
    chrono::DateTime::parse_from_rfc2822(value)
        .ok()
        .map(|date| date.timestamp().max(now))
}

pub fn is_global(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => global4(v),
        IpAddr::V6(v) => global6(v),
    }
}
fn global4(v: Ipv4Addr) -> bool {
    let o = v.octets();
    !(v.is_private()
        || v.is_loopback()
        || v.is_link_local()
        || v.is_multicast()
        || v.is_unspecified()
        || o[0] == 0
        || o[0] >= 240
        || (o[0] == 100 && (64..=127).contains(&o[1]))
        || (o[0] == 192 && o[1] == 0 && o[2] == 2)
        || (o[0] == 198 && matches!(o[1], 18 | 19))
        || (o[0] == 198 && o[1] == 51 && o[2] == 100)
        || (o[0] == 203 && o[1] == 0 && o[2] == 113)
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)
        || (o[0] == 192 && o[1] == 31 && o[2] == 196)
        || (o[0] == 192 && o[1] == 52 && o[2] == 193)
        || (o[0] == 192 && o[1] == 88 && o[2] == 99)
        || (o[0] == 192 && o[1] == 175 && o[2] == 48))
}
#[allow(clippy::nonminimal_bool)] // Keep each reserved IPv6 range auditable against the policy table.
fn global6(v: Ipv6Addr) -> bool {
    let s = v.segments();
    if (s[0] & 0xe000) != 0x2000
        || v.is_unspecified()
        || v.is_loopback()
        || v.is_multicast()
        || (s[0] & 0xfe00) == 0xfc00
        || (s[0] & 0xffc0) == 0xfe80
        || (s[0] & 0xffc0) == 0xfec0
        || (s[0] == 0x2001 && s[1] == 0xdb8)
        || (s[0] == 0x0100 && s[1..4] == [0, 0, 0])
        || (s[0] == 0x2001 && s[1] <= 0x01ff)
        || (s[0] == 0x2620 && s[1] == 0x004f && s[2] == 0x8000)
        || (s[0] == 0x3fff && (s[1] & 0xf000) == 0)
        || s[0] == 0x5f00
    {
        return false;
    }
    if v.to_ipv4_mapped().is_some() || v.to_ipv4().is_some() {
        return false;
    }
    if (s[0] == 0x64 && s[1] == 0xff9b && s[2..6] == [0, 0, 0, 0])
        || (s[0] == 0x64 && s[1] == 0xff9b && s[2] == 1)
        || s[0] == 0x2002
    {
        return false;
    }
    true
}

pub fn fetch(
    url: &str,
    resolver: &dyn Resolver,
    transport: &dyn HttpTransport,
) -> std::result::Result<Fetched, FetchFailure> {
    fetch_inner(
        url,
        resolver,
        transport,
        Duration::from_secs(30),
        &mut |_, _| Ok(()),
        None,
    )
    .map_err(Into::into)
}

pub(crate) fn fetch_scheduled(
    url: &str,
    resolver: &dyn Resolver,
    transport: &dyn HttpTransport,
    before_hop: &mut dyn FnMut(&Url, u8) -> Result<()>,
) -> std::result::Result<Fetched, FetchFailure> {
    // Five redirects may each wait up to 25s. This is background capture time,
    // not an increase to the interactive RPC deadline.
    fetch_inner(
        url,
        resolver,
        transport,
        Duration::from_secs(180),
        before_hop,
        None,
    )
    .map_err(Into::into)
}

/// Exact resource policies share the public-DNS-pinned transport, not a generic image proxy.
#[derive(Clone, Debug, PartialEq, Eq)]
enum PodcastResource {
    Feed,
    Cover(String),
    /// One image a registered source handler selected; see `fetch_exact_image`.
    ExactImage(String),
}
impl PodcastResource {
    fn from_url(url: &Url) -> Option<Self> {
        if url.as_str() == memex_intake::url_jobs::podcast::FEED_URL {
            return Some(Self::Feed);
        }
        let video = url.path().strip_prefix("/vi/")?.split('/').next()?;
        memex_store::covers::youtube_cover_url(video, url.as_str())
            .then(|| Self::Cover(video.into()))
    }
    fn limit(&self) -> usize {
        match self { Self::Feed => 2 * 1024 * 1024, Self::Cover(_) => 5 * 1024 * 1024, Self::ExactImage(_) => MAX_EXACT_IMAGE }
    }
    fn content_type(&self, value: &str) -> Result<String> {
        let media = value.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
        let allowed = match self {
            Self::Feed => matches!(media.as_str(), "application/rss+xml" | "application/xml" | "text/xml"),
            Self::Cover(_) => matches!(media.as_str(), "image/jpeg" | "image/png" | "image/webp"),
            Self::ExactImage(_) => matches!(media.as_str(), "image/jpeg" | "image/png" | "image/webp" | "image/gif"),
        };
        anyhow::ensure!(allowed, "content_type_rejected");
        Ok(media)
    }
    /// Redirects cannot change the resource; an exact image keeps its exact URL.
    fn admits(&self, url: &Url) -> bool {
        match self {
            Self::ExactImage(exact) => url.as_str() == exact,
            _ => Self::from_url(url).as_ref() == Some(self),
        }
    }
}

pub fn fetch_podcast_resource(
    url: &str,
    resolver: &dyn Resolver,
    transport: &dyn HttpTransport,
    before_hop: &mut dyn FnMut(&Url, u8) -> Result<()>,
) -> std::result::Result<Fetched, FetchFailure> {
    let source = Url::parse(url).map_err(|_| FetchFailure::from(anyhow::anyhow!("url_malformed")))?;
    let resource = PodcastResource::from_url(&source)
        .ok_or_else(|| FetchFailure::from(anyhow::anyhow!("url_policy_rejected")))?;
    fetch_inner(url, resolver, transport, Duration::from_secs(180), before_hop, Some(&resource))
        .map_err(Into::into)
}

/// Only an image a source handler explicitly selected and admitted. Redirects
/// cannot change its identity, host, query or resource type. No cookies or Referer.
pub(crate) fn fetch_exact_image(
    url: &str,
    resolver: &dyn Resolver,
    transport: &dyn HttpTransport,
) -> std::result::Result<Fetched, FetchFailure> {
    Url::parse(url).map_err(|_| FetchFailure::from(anyhow::anyhow!("url_malformed")))?;
    fetch_inner(url, resolver, transport, Duration::from_secs(30), &mut |_, _| Ok(()),
        Some(&PodcastResource::ExactImage(url.into())))
        .map_err(|error| match error.to_string().as_str() {
            "compressed_body_too_large" | "decoded_body_too_large" => anyhow::anyhow!("image_too_large"),
            _ => error,
        })
        .map_err(Into::into)
}

fn fetch_inner(
    url: &str,
    resolver: &dyn Resolver,
    transport: &dyn HttpTransport,
    budget: Duration,
    before_hop: &mut dyn FnMut(&Url, u8) -> Result<()>,
    podcast: Option<&PodcastResource>,
) -> Result<Fetched> {
    let deadline = std::time::Instant::now() + budget;
    let source = Url::parse(url).map_err(|_| anyhow::anyhow!("url_malformed"))?;
    let api = lots_scout_extract::LinkresearcherArticleExtractor::api_url(url);
    let mut current = match &api {
        Some(api) => Url::parse(api)?,
        None => source.clone(),
    };
    for hops in 0..=5 {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            bail!("fetch_deadline")
        }
        validate_url(&current)?;
        if let Some(resource) = podcast {
            anyhow::ensure!(resource.admits(&current), "url_policy_rejected");
        }
        before_hop(&current, hops)?;
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            bail!("fetch_deadline")
        }
        let host = current
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("host_missing"))?;
        let ips = resolver.resolve(host, remaining.min(Duration::from_secs(5)))?;
        if ips.is_empty()
            || ips.len() > MAX_RESOLVED_ADDRESSES
            || ips.iter().any(|x| !is_global(*x))
        {
            bail!("address_not_public")
        };
        let port = current.port_or_known_default().unwrap();
        let pinned: Vec<_> = ips.into_iter().map(|x| SocketAddr::new(x, port)).collect();
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            bail!("fetch_deadline")
        }
        let raw = transport.get(&current, &pinned, remaining.min(Duration::from_secs(30)))?;
        validate_headers(&raw.headers)?;
        if matches!(raw.status, 401 | 403 | 407) {
            bail!("interactive_login_required")
        }
        if matches!(raw.status, 301 | 302 | 303 | 307 | 308) {
            // A source API is one exact public representation. Do not follow a
            // redirect to a login endpoint or fetch a different representation.
            if api.is_some() {
                bail!("source_api_redirect_rejected")
            }
            if hops == 5 {
                bail!("redirect_limit")
            };
            let location = singleton_header(&raw.headers, "location")?
                .ok_or_else(|| anyhow::anyhow!("redirect_location_missing"))?;
            let next = current
                .join(location)
                .map_err(|_| anyhow::anyhow!("redirect_location_invalid"))?;
            if current.scheme() == "https" && next.scheme() == "http" {
                bail!("redirect_downgrade")
            };
            current = next;
            continue;
        }
        if !(200..300).contains(&raw.status) {
            if matches!(raw.status, 429 | 503) {
                let now = chrono::Utc::now().timestamp();
                let not_before = singleton_header(&raw.headers, "retry-after")?
                    .and_then(|value| retry_after_at(value, now))
                    .unwrap_or(now);
                return Err(RateLimited {
                    origin: current.origin().ascii_serialization(),
                    not_before,
                }
                .into());
            }
            if raw.status == 408 || raw.status == 429 || raw.status >= 500 {
                bail!("http_status_retryable")
            }
            bail!("http_status_rejected")
        };
        let ct = singleton_header(&raw.headers, "content-type")?
            .ok_or_else(|| anyhow::anyhow!("content_type_rejected"))?;
        let media = match podcast {
            Some(resource) => resource.content_type(ct)?,
            None => validate_content_type(ct, api.is_some())?,
        };
        let body = decode_with_worker(&raw.headers, raw.body, deadline, decompressor(),
            podcast.map_or(MAX_BODY, PodcastResource::limit))?;
        if !matches!(podcast, Some(PodcastResource::Cover(_) | PodcastResource::ExactImage(_))) {
            std::str::from_utf8(&body).map_err(|_| anyhow::anyhow!("body_not_utf8"))?;
        }
        return Ok(Fetched {
            status: raw.status,
            // Public article identity is distinct from its acquisition URL,
            // which the capture blob descriptor records for API responses.
            final_url: if api.is_some() { source } else { current },
            media_type: media,
            body,
            redirects: hops,
        });
    }
    unreachable!()
}
/// Descriptive agent for the approved metadata providers (no cookies, no
/// credentials, no item data beyond the identifier in the request path).
pub const METADATA_LOOKUP_USER_AGENT: &str =
    "LotsMemeX/0.1 (bibliographic metadata lookup; +https://github.com/lotsmemex/lotsmemex)";

/// The contact joins Crossref's polite pool via the User-Agent only (never a
/// query string); Open Library and absent/invalid contacts get the plain agent.
pub fn metadata_user_agent(url: &Url, contact: Option<&str>) -> String {
    match contact.filter(|contact| crate::config::valid_lookup_contact(contact)) {
        Some(contact)
            if memex_store::lookup_route(url) == Some(memex_store::LookupProvider::Crossref) =>
        {
            format!("{} mailto:{contact}", METADATA_LOOKUP_USER_AGENT)
        }
        _ => METADATA_LOOKUP_USER_AGENT.to_owned(),
    }
}

/// Fetches one approved metadata route (exact host/path shapes owned by
/// `memex_store::lookup_route`) over pinned public addresses. Only Open
/// Library's single same-host edition redirect is followed; JSON only.
pub(crate) fn fetch_metadata_lookup(
    url: &Url,
    resolver: &dyn Resolver,
    transport: &dyn HttpTransport,
    budget: Duration,
) -> Result<Fetched> {
    let deadline = std::time::Instant::now() + budget;
    let mut current = url.clone();
    for hops in 0..=1u8 {
        if memex_store::lookup_route(&current).is_none() {
            bail!("lookup_policy_rejected")
        }
        validate_url(&current)?;
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            bail!("fetch_deadline")
        }
        let host = current
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("host_missing"))?;
        let ips = resolver.resolve(host, remaining.min(Duration::from_secs(5)))?;
        if ips.is_empty()
            || ips.len() > MAX_RESOLVED_ADDRESSES
            || ips.iter().any(|x| !is_global(*x))
        {
            bail!("address_not_public")
        }
        let port = current.port_or_known_default().unwrap();
        let pinned: Vec<_> = ips.into_iter().map(|x| SocketAddr::new(x, port)).collect();
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            bail!("fetch_deadline")
        }
        let raw = transport.get(&current, &pinned, remaining.min(Duration::from_secs(30)))?;
        validate_headers(&raw.headers)?;
        if matches!(raw.status, 301 | 302 | 303 | 307 | 308) {
            let next = singleton_header(&raw.headers, "location")?
                .and_then(|location| current.join(location).ok())
                .ok_or_else(|| anyhow::anyhow!("lookup_redirect_rejected"))?;
            if hops == 1 || !memex_store::lookup_redirect_allowed(&current, &next) {
                bail!("lookup_redirect_rejected")
            }
            current = next;
            continue;
        }
        if raw.status == 404 {
            bail!("lookup_not_found")
        }
        if matches!(raw.status, 429 | 503) {
            let now = chrono::Utc::now().timestamp();
            let not_before = singleton_header(&raw.headers, "retry-after")?
                .and_then(|value| retry_after_at(value, now))
                .unwrap_or(now);
            return Err(RateLimited {
                origin: current.origin().ascii_serialization(),
                not_before,
            }
            .into());
        }
        if !(200..300).contains(&raw.status) {
            bail!("lookup_unavailable")
        }
        let ct = singleton_header(&raw.headers, "content-type")?
            .ok_or_else(|| anyhow::anyhow!("content_type_rejected"))?;
        let media_type = validate_content_type(ct, true)?;
        let body = decode(&raw.headers, raw.body, deadline)?;
        if body.len() > memex_store::MAX_LOOKUP_RESPONSE_BYTES {
            bail!("lookup_response_too_large")
        }
        std::str::from_utf8(&body).map_err(|_| anyhow::anyhow!("body_not_utf8"))?;
        return Ok(Fetched {
            status: raw.status,
            final_url: current,
            media_type,
            body,
            redirects: hops,
        });
    }
    bail!("lookup_redirect_rejected")
}

fn validate_url(u: &Url) -> Result<()> {
    if !matches!(u.scheme(), "http" | "https")
        || !u.username().is_empty()
        || u.password().is_some()
        || u.fragment().is_some()
    {
        bail!("url_policy_rejected")
    };
    let d = if u.scheme() == "http" { 80 } else { 443 };
    if u.port().is_some_and(|p| p != d) {
        bail!("url_port_forbidden")
    }
    Ok(())
}
fn singleton_header<'a>(h: &'a [(String, String)], n: &str) -> Result<Option<&'a str>> {
    let mut values = h
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case(n))
        .map(|(_, value)| value.as_str());
    let value = values.next();
    if values.next().is_some() {
        bail!("duplicate_response_header")
    }
    Ok(value)
}
fn validate_headers(h: &[(String, String)]) -> Result<()> {
    if h.len() > MAX_HEADERS
        || h.iter().map(|(k, v)| k.len() + v.len() + 4).sum::<usize>() > MAX_HEADER_BYTES
    {
        bail!("headers_too_large")
    }
    Ok(())
}
fn validate_content_type(v: &str, source_api: bool) -> Result<String> {
    let mut p = v.split(';');
    let media = p.next().unwrap_or("").trim().to_ascii_lowercase();
    if if source_api {
        media != "application/json"
    } else {
        !matches!(media.as_str(), "text/html" | "application/xhtml+xml")
    } {
        bail!("content_type_rejected")
    };
    for x in p {
        let x = x.trim().to_ascii_lowercase();
        if x.starts_with("charset=") && !matches!(x.as_str(), "charset=utf-8" | "charset=\"utf-8\"")
        {
            bail!("charset_rejected")
        }
    }
    Ok(media)
}
fn decode(
    h: &[(String, String)],
    compressed: Vec<u8>,
    deadline: std::time::Instant,
) -> Result<Vec<u8>> {
    decode_with_worker(h, compressed, deadline, decompressor(), MAX_BODY)
}
fn decode_with_worker(
    h: &[(String, String)],
    compressed: Vec<u8>,
    deadline: std::time::Instant,
    worker: &std::sync::mpsc::SyncSender<DecodeRequest>,
    max_body: usize,
) -> Result<Vec<u8>> {
    if compressed.len() > max_body {
        bail!("compressed_body_too_large")
    };
    if let Some(length) = singleton_header(h, "content-length")? {
        if length.is_empty()
            || (length.len() > 1 && length.starts_with('0'))
            || !length.bytes().all(|byte| byte.is_ascii_digit())
            || length.parse::<usize>().ok() != Some(compressed.len())
        {
            bail!("content_length_mismatch")
        }
    }
    let encoding = singleton_header(h, "content-encoding")?
        .unwrap_or("identity")
        .trim()
        .to_ascii_lowercase();
    if !matches!(
        encoding.as_str(),
        "" | "identity" | "gzip" | "deflate" | "br"
    ) {
        bail!("content_encoding_rejected")
    }
    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
    if remaining.is_zero() {
        bail!("fetch_deadline")
    }
    let (reply, result) = std::sync::mpsc::sync_channel(1);
    worker
        .try_send(DecodeRequest {
            encoding,
            compressed,
            max_body,
            reply,
        })
        .map_err(|_| anyhow::anyhow!("decompression_busy"))?;
    match result.recv_timeout(remaining) {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(code)) => Err(anyhow::anyhow!(code)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            Err(anyhow::anyhow!("decompression_timeout"))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err(anyhow::anyhow!("decompression_unavailable"))
        }
    }
}

type DecodeResult = std::result::Result<Vec<u8>, &'static str>;
struct DecodeRequest {
    encoding: String,
    compressed: Vec<u8>,
    max_body: usize,
    reply: std::sync::mpsc::SyncSender<DecodeResult>,
}
fn decompressor() -> &'static std::sync::mpsc::SyncSender<DecodeRequest> {
    static WORKER: std::sync::OnceLock<std::sync::mpsc::SyncSender<DecodeRequest>> =
        std::sync::OnceLock::new();
    WORKER.get_or_init(|| {
        let (sender, receiver) =
            std::sync::mpsc::sync_channel::<DecodeRequest>(MAX_DECOMPRESSION_QUEUE);
        std::thread::spawn(move || {
            while let Ok(request) = receiver.recv() {
                let result = decode_bounded(&request.encoding, &request.compressed, request.max_body);
                let _ = request.reply.send(result);
            }
        });
        sender
    })
}
fn decode_bounded(encoding: &str, compressed: &[u8], max_body: usize) -> DecodeResult {
    let reader: Box<dyn Read> = match encoding {
        "" | "identity" => Box::new(compressed),
        "gzip" => Box::new(GzDecoder::new(compressed)),
        "deflate" => Box::new(ZlibDecoder::new(compressed)),
        "br" => Box::new(brotli::Decompressor::new(compressed, 4096)),
        _ => return Err("content_encoding_rejected"),
    };
    let mut reader = reader.take((max_body + 1) as u64);
    let mut out = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|_| "decompression_failed")?;
        if read == 0 {
            break;
        }
        if out.len().saturating_add(read) > max_body {
            return Err("decoded_body_too_large");
        }
        out.extend_from_slice(&buffer[..read]);
    }
    if !compressed.is_empty() && out.len() > compressed.len().saturating_mul(100) {
        return Err("compression_ratio_exceeded");
    }
    Ok(out)
}

type ResolveReply = std::sync::mpsc::SyncSender<std::io::Result<Vec<IpAddr>>>;

const TRUSTED_DOH_HOST: &str = "cloudflare-dns.com";
const TRUSTED_DOH_PATH: &str = "/dns-query";
const MAX_DOH_BODY: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DnsRecordType {
    A,
    Aaaa,
}
impl DnsRecordType {
    fn number(self) -> u16 {
        match self {
            Self::A => 1,
            Self::Aaaa => 28,
        }
    }
}

#[derive(Deserialize)]
struct DohReply {
    #[serde(rename = "Status")]
    status: u32,
    #[serde(rename = "TC", default)]
    truncated: bool,
    #[serde(rename = "Answer", default)]
    answer: Vec<DohAnswer>,
}

#[derive(Deserialize)]
struct DohAnswer {
    #[serde(rename = "type")]
    record_type: u16,
    data: String,
}

fn documented_benchmark_synthetic(ip: IpAddr) -> bool {
    matches!(ip, IpAddr::V4(value) if {
        let octets = value.octets();
        octets[0] == 198 && matches!(octets[1], 18 | 19)
    })
}

fn validated_resolved_addresses(addresses: Vec<IpAddr>) -> Result<Vec<IpAddr>> {
    let addresses = addresses.into_iter().collect::<BTreeSet<_>>();
    if addresses.is_empty()
        || addresses.len() > MAX_RESOLVED_ADDRESSES
        || addresses.iter().any(|address| !is_global(*address))
    {
        bail!("address_not_public")
    }
    Ok(addresses.into_iter().collect())
}

fn resolve_synthetic_fallback(
    native: Vec<IpAddr>,
    mut lookup: impl FnMut(DnsRecordType) -> Result<Vec<IpAddr>>,
) -> Result<Vec<IpAddr>> {
    if native.is_empty()
        || native.len() > MAX_RESOLVED_ADDRESSES
        || !native
            .iter()
            .all(|address| documented_benchmark_synthetic(*address))
    {
        return Ok(native);
    }
    let mut addresses = lookup(DnsRecordType::A)?;
    addresses.extend(lookup(DnsRecordType::Aaaa)?);
    validated_resolved_addresses(addresses)
}

fn trusted_doh_bootstrap() -> [SocketAddr; 2] {
    // Public resolver anycast addresses are fixed bootstrap trust anchors. The
    // HTTPS URL still supplies the hostname for certificate validation and SNI.
    [
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)), 443),
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(1, 0, 0, 1)), 443),
    ]
}

fn trusted_doh_lookup(
    host: &str,
    record_type: DnsRecordType,
    timeout: Duration,
) -> Result<Vec<IpAddr>> {
    let mut endpoint = Url::parse(&format!("https://{TRUSTED_DOH_HOST}{TRUSTED_DOH_PATH}"))?;
    endpoint
        .query_pairs_mut()
        .append_pair("name", host)
        .append_pair("type", &record_type.number().to_string());
    let bootstrap = trusted_doh_bootstrap();
    if bootstrap.iter().any(|address| !is_global(address.ip())) {
        bail!("dns_error")
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| anyhow::anyhow!("dns_error"))?;
    let body = runtime.block_on(async move {
        tokio::time::timeout(timeout, async move {
            let client = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .http1_only()
                .pool_max_idle_per_host(0)
                .timeout(timeout)
                .connect_timeout(Duration::from_secs(5).min(timeout))
                .resolve_to_addrs(TRUSTED_DOH_HOST, &bootstrap)
                .build()
                .map_err(|_| anyhow::anyhow!("dns_error"))?;
            let mut response = client
                .get(endpoint)
                .header(reqwest::header::ACCEPT, "application/dns-json")
                .send()
                .await
                .map_err(|_| anyhow::anyhow!("dns_error"))?;
            if !response.status().is_success() {
                bail!("dns_error")
            }
            let media_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.split(';').next())
                .map(str::trim);
            if media_type != Some("application/dns-json") {
                bail!("dns_error")
            }
            let mut body = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| anyhow::anyhow!("dns_error"))?
            {
                if body.len().saturating_add(chunk.len()) > MAX_DOH_BODY {
                    bail!("dns_error")
                }
                body.extend_from_slice(&chunk);
            }
            Ok::<_, anyhow::Error>(body)
        })
        .await
        .map_err(|_| anyhow::anyhow!("dns_error"))?
    })?;
    let reply: DohReply =
        serde_json::from_slice(&body).map_err(|_| anyhow::anyhow!("dns_error"))?;
    if reply.status != 0 || reply.truncated {
        bail!("dns_error")
    }
    reply
        .answer
        .into_iter()
        .filter(|answer| answer.record_type == record_type.number())
        .map(|answer| {
            let address = answer
                .data
                .parse::<IpAddr>()
                .map_err(|_| anyhow::anyhow!("dns_error"))?;
            if matches!(
                (record_type, address),
                (DnsRecordType::A, IpAddr::V4(_)) | (DnsRecordType::Aaaa, IpAddr::V6(_))
            ) {
                Ok(address)
            } else {
                bail!("dns_error")
            }
        })
        .collect()
}

/// A single bounded system-resolver worker avoids accumulating unbounded
/// `getaddrinfo` threads. A timeout poisons the worker and is a fatal coordinator
/// error: the capture worker loop restarts, reusing (never abandoning) this worker.
pub struct SystemResolver {
    requests: std::sync::mpsc::SyncSender<(String, ResolveReply)>,
}
impl Default for SystemResolver {
    fn default() -> Self {
        let (requests, receiver) = std::sync::mpsc::sync_channel::<(String, ResolveReply)>(1);
        std::thread::spawn(move || {
            use std::net::ToSocketAddrs;
            while let Ok((host, reply)) = receiver.recv() {
                let result = (host.as_str(), 0).to_socket_addrs().map(|addresses| {
                    addresses
                        .take(MAX_RESOLVED_ADDRESSES + 1)
                        .map(|address| address.ip())
                        .collect()
                });
                let _ = reply.send(result);
            }
        });
        Self { requests }
    }
}
impl Resolver for SystemResolver {
    fn resolve(&self, host: &str, timeout: Duration) -> Result<Vec<IpAddr>> {
        let deadline = std::time::Instant::now() + timeout;
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        self.requests
            .try_send((host.to_owned(), tx))
            .map_err(|_| anyhow::anyhow!("dns_busy"))?;
        match rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())) {
            Ok(result) => {
                let native = result.map_err(|_| anyhow::anyhow!("dns_error"))?;
                resolve_synthetic_fallback(native, |record_type| {
                    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                    if remaining.is_zero() {
                        bail!("dns_timeout")
                    }
                    trusted_doh_lookup(host, record_type, remaining)
                })
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => bail!("dns_timeout"),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => bail!("dns_unavailable"),
        }
    }
}
pub struct ReqwestTransport {
    timeout: Duration,
    metadata_contact: Option<String>,
    exact_images: Vec<std::sync::Arc<dyn crate::source::SourceHandler>>,
}
impl ReqwestTransport {
    /// Lookup transport carrying an owner-configured Crossref contact. The
    /// caller validates it (`config::valid_lookup_contact`).
    pub fn with_metadata_contact(contact: Option<String>) -> Self {
        Self {
            metadata_contact: contact,
            ..Self::default()
        }
    }
    /// Requests an image response for URLs these handlers' `exact_image_url` admits.
    pub fn with_source_images(handlers: &[std::sync::Arc<dyn crate::source::SourceHandler>]) -> Self {
        Self {
            exact_images: handlers.to_vec(),
            ..Self::default()
        }
    }
}
fn reqwest_error(error: reqwest::Error) -> anyhow::Error {
    if error.is_timeout() {
        anyhow::anyhow!("fetch_deadline")
    } else {
        anyhow::anyhow!("transport_error")
    }
}
impl Default for ReqwestTransport {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            metadata_contact: None,
            exact_images: Vec::new(),
        }
    }
}
impl HttpTransport for ReqwestTransport {
    fn get(&self, url: &Url, pinned: &[SocketAddr], timeout: Duration) -> Result<RawResponse> {
        let host = url.host_str().unwrap().to_owned();
        let url = url.clone();
        let pinned = pinned.to_vec();
        let timeout = self.timeout.min(timeout);
        let metadata_lookup = memex_store::lookup_route(&url).is_some();
        let user_agent = metadata_user_agent(&url, self.metadata_contact.as_deref());
        let json_api = metadata_lookup
            || lots_scout_extract::LinkresearcherArticleExtractor::is_api_url(url.as_str());
        let podcast = PodcastResource::from_url(&url).or_else(|| {
            self.exact_images
                .iter()
                .any(|handler| handler.exact_image_url(url.as_str()))
                .then(|| PodcastResource::ExactImage(url.to_string()))
        });
        let max_body = podcast.as_ref().map_or(MAX_BODY, PodcastResource::limit);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        runtime
            .block_on(async move {
                tokio::time::timeout(timeout, async move {
                    let client = reqwest::Client::builder()
                        .no_proxy()
                        .redirect(reqwest::redirect::Policy::none())
                        .no_gzip()
                        .no_brotli()
                        .no_zstd()
                        .no_deflate()
                        .http1_only()
                        .pool_max_idle_per_host(0)
                        .timeout(timeout)
                        .connect_timeout(Duration::from_secs(10).min(timeout))
                        .resolve_to_addrs(&host, &pinned)
                        .build()
                        .map_err(reqwest_error)?;
                    let mut request = client
                        .get(url.clone())
                        .header(
                            reqwest::header::ACCEPT,
                            match podcast.as_ref() {
                                Some(PodcastResource::Feed) => "application/rss+xml, application/xml, text/xml",
                                Some(PodcastResource::Cover(_)) => "image/jpeg, image/png, image/webp",
                                Some(PodcastResource::ExactImage(_)) => "image/jpeg, image/png, image/webp, image/gif",
                                None if json_api => "application/json",
                                None => "text/html, application/xhtml+xml",
                            },
                        )
                        .header(reqwest::header::ACCEPT_ENCODING, "gzip, br, deflate");
                    if metadata_lookup {
                        request = request.header(reqwest::header::USER_AGENT, user_agent);
                    }
                    let mut response = request.send().await.map_err(reqwest_error)?;
                    let status = response.status().as_u16();
                    let headers = response
                        .headers()
                        .iter()
                        .map(|(key, value)| {
                            Ok((
                                key.to_string(),
                                value
                                    .to_str()
                                    .map_err(|_| anyhow::anyhow!("response_header_invalid"))?
                                    .to_string(),
                            ))
                        })
                        .collect::<Result<Vec<_>>>()?;
                    validate_headers(&headers)?;
                    let mut body = Vec::new();
                    if (200..300).contains(&status) {
                        let content_type = singleton_header(&headers, "content-type")?
                            .ok_or_else(|| anyhow::anyhow!("content_type_rejected"))?;
                        match podcast.as_ref() {
                            Some(resource) => resource.content_type(content_type)?,
                            None => validate_content_type(content_type, json_api)?,
                        };
                        let encoding = singleton_header(&headers, "content-encoding")?
                            .unwrap_or("identity")
                            .trim()
                            .to_ascii_lowercase();
                        if !matches!(
                            encoding.as_str(),
                            "" | "identity" | "gzip" | "deflate" | "br"
                        ) {
                            bail!("content_encoding_rejected")
                        }
                        if let Some(length) = singleton_header(&headers, "content-length")? {
                            if length.is_empty()
                                || (length.len() > 1 && length.starts_with('0'))
                                || !length.bytes().all(|byte| byte.is_ascii_digit())
                                || length
                                    .parse::<usize>()
                                    .ok()
                                    .is_none_or(|value| value > max_body)
                            {
                                bail!("compressed_body_too_large")
                            }
                        }
                        while let Some(chunk) = response.chunk().await.map_err(reqwest_error)? {
                            if body.len().saturating_add(chunk.len()) > max_body {
                                bail!("compressed_body_too_large")
                            }
                            body.extend_from_slice(&chunk);
                        }
                    }
                    Ok::<_, anyhow::Error>(RawResponse {
                        status,
                        headers,
                        body,
                    })
                })
                .await
            })
            .map_err(|_| anyhow::anyhow!("fetch_deadline"))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{write::GzEncoder, Compression};
    use std::{collections::BTreeMap, io::Write as _, net::TcpListener};
    struct R(BTreeMap<String, Vec<IpAddr>>);
    impl Resolver for R {
        fn resolve(&self, h: &str, _: Duration) -> Result<Vec<IpAddr>> {
            Ok(self.0[h].clone())
        }
    }
    struct T(std::sync::Mutex<Vec<RawResponse>>);
    impl HttpTransport for T {
        fn get(&self, _: &Url, _: &[SocketAddr], _: Duration) -> Result<RawResponse> {
            Ok(self.0.lock().unwrap().remove(0))
        }
    }
    fn res() -> R {
        R(BTreeMap::from([
            ("a.test".into(), vec!["93.184.216.34".parse().unwrap()]),
            ("b.test".into(), vec!["93.184.216.35".parse().unwrap()]),
        ]))
    }
    struct RecordingResolver {
        addresses: BTreeMap<String, Vec<IpAddr>>,
        calls: std::sync::Mutex<Vec<String>>,
    }
    impl Resolver for RecordingResolver {
        fn resolve(&self, host: &str, _: Duration) -> Result<Vec<IpAddr>> {
            self.calls.lock().unwrap().push(host.to_owned());
            Ok(self.addresses[host].clone())
        }
    }
    struct RecordingTransport {
        responses: std::sync::Mutex<Vec<RawResponse>>,
        calls: std::sync::Mutex<Vec<(String, Vec<SocketAddr>)>>,
    }
    impl HttpTransport for RecordingTransport {
        fn get(&self, url: &Url, pinned: &[SocketAddr], _: Duration) -> Result<RawResponse> {
            self.calls
                .lock()
                .unwrap()
                .push((url.host_str().unwrap().to_owned(), pinned.to_vec()));
            Ok(self.responses.lock().unwrap().remove(0))
        }
    }
    #[test]
    fn exact_image_policy_rejects_mixed_dns_redirects_mime_and_oversize() {
        const IMAGE: &str = "https://images.example/synthetic/0";
        let resolver = RecordingResolver { addresses:BTreeMap::from([("images.example".into(), vec!["93.184.216.34".parse().unwrap()])]), calls:Default::default() };
        let png = include_bytes!("../examples/fixtures/reading-image.png");
        let response = |media: &str, body:Vec<u8>| RawResponse { status:200, headers:vec![("content-type".into(),media.into())], body };
        let transport = |r| RecordingTransport { responses:std::sync::Mutex::new(vec![r]), calls:Default::default() };
        assert_eq!(fetch_exact_image(IMAGE, &resolver, &transport(response("image/png",png.to_vec()))).unwrap().body, png);
        let mixed = RecordingResolver { addresses:BTreeMap::from([("images.example".into(), vec!["93.184.216.34".parse().unwrap(), "127.0.0.1".parse().unwrap()])]), calls:Default::default() };
        let unused = transport(response("image/png",png.to_vec()));
        assert_eq!(fetch_exact_image(IMAGE, &mixed, &unused).unwrap_err().reason_code(), "address_not_public");
        assert!(unused.calls.lock().unwrap().is_empty());
        for location in ["https://images.example/different/0", "https://127.0.0.1/image.png", "https://other.invalid/image.png"] {
            let t = transport(RawResponse { status:302, headers:vec![("location".into(),location.into())], body:vec![] });
            assert!(fetch_exact_image(IMAGE, &resolver, &t).is_err());
            assert_eq!(t.calls.lock().unwrap().len(), 1);
        }
        assert!(fetch_exact_image(IMAGE, &resolver, &transport(response("text/html",png.to_vec()))).is_err());
        let gif = |size: usize| { let mut body = b"GIF89a".to_vec(); body.resize(size, 0); body };
        assert_eq!(fetch_exact_image(IMAGE, &resolver, &transport(response("image/gif", gif(MAX_EXACT_IMAGE)))).unwrap().body.len(), MAX_EXACT_IMAGE);
        assert_eq!(fetch_exact_image(IMAGE, &resolver, &transport(response("image/gif", gif(MAX_EXACT_IMAGE + 1)))).unwrap_err().reason_code(), "image_too_large");
    }

    #[test]
    fn podcast_redirects_keep_video_identity_and_recheck_dns_before_every_hop() {
        const IMAGE: &str = "https://i.ytimg.com/vi/abcdefghijk/hqdefault.jpg";
        let resolver = RecordingResolver { addresses: BTreeMap::from([
            ("i.ytimg.com".into(), vec!["93.184.216.34".parse().unwrap()]),
        ]), calls: Default::default() };
        let redirect = |location: &str| RawResponse { status: 302,
            headers: vec![("location".into(), location.into())], body: vec![] };
        let transport = RecordingTransport { responses: std::sync::Mutex::new(vec![
            redirect("/vi/abcdefghijk/mqdefault.jpg"),
            RawResponse { status: 200, headers: vec![("content-type".into(), "image/jpeg".into())],
                body: vec![0xff, 0xd8, 0xff] },
        ]), calls: Default::default() };
        let mut hops = Vec::new();
        let result = fetch_podcast_resource(IMAGE, &resolver, &transport,
            &mut |_, hop| { hops.push(hop); Ok(()) }).unwrap();
        assert_eq!(result.body, [0xff, 0xd8, 0xff]); // Binary need not be UTF-8; Store later decodes it.
        assert_eq!(result.redirects, 1);
        assert_eq!(hops, [0, 1]);
        assert_eq!(resolver.calls.lock().unwrap().len(), 2);
        assert!(transport.calls.lock().unwrap().iter().all(|(_, ips)|
            ips == &["93.184.216.34:443".parse().unwrap()]));
        for location in ["https://evil.invalid/vi/abcdefghijk/hqdefault.jpg",
            "http://i.ytimg.com/vi/abcdefghijk/hqdefault.jpg", "/vi/other_video/hqdefault.jpg",
            "/vi/abcdefghijk/hqdefault.jpg?redirect=1", "https://ampcode.com/podcast.rss"] {
            let transport = RecordingTransport { responses: std::sync::Mutex::new(vec![redirect(location)]),
                calls: Default::default() };
            assert!(fetch_podcast_resource(IMAGE, &resolver, &transport, &mut |_, _| Ok(())).is_err());
            assert_eq!(transport.calls.lock().unwrap().len(), 1);
        }
        struct Rebinding(std::sync::atomic::AtomicUsize);
        impl Resolver for Rebinding {
            fn resolve(&self, _: &str, _: Duration) -> Result<Vec<IpAddr>> {
                Ok(vec![if self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    "93.184.216.34" } else { "127.0.0.1" }.parse().unwrap()])
            }
        }
        let transport = RecordingTransport { responses: std::sync::Mutex::new(vec![redirect(IMAGE)]),
            calls: Default::default() };
        let rebound = Rebinding(Default::default());
        assert_eq!(fetch_podcast_resource(IMAGE, &rebound, &transport, &mut |_, _| Ok(())).unwrap_err()
            .reason_code(), "address_not_public");
        assert_eq!(transport.calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn podcast_feed_and_raster_have_distinct_mime_and_compressed_and_decoded_limits() {
        let resolver = R(BTreeMap::from([
            ("ampcode.com".into(), vec!["93.184.216.34".parse().unwrap()]),
            ("i.ytimg.com".into(), vec!["93.184.216.34".parse().unwrap()]),
        ]));
        for (url, media, limit) in [
            (memex_intake::url_jobs::podcast::FEED_URL, "application/rss+xml", 2 * 1024 * 1024),
            ("https://i.ytimg.com/vi/abcdefghijk/hqdefault.jpg", "image/webp", 5 * 1024 * 1024),
        ] {
            let response = |body, ct: &str, gzip| {
                let mut headers = vec![("content-type".into(), ct.into())];
                if gzip { headers.push(("content-encoding".into(), "gzip".into())); }
                T(std::sync::Mutex::new(vec![RawResponse { status: 200, headers, body }]))
            };
            assert_eq!(fetch_podcast_resource(url, &resolver, &response(vec![b'x'; limit], media, false),
                &mut |_, _| Ok(())).unwrap().body.len(), limit);
            assert!(fetch_podcast_resource(url, &resolver, &response(vec![b'x'; limit + 1], media, false),
                &mut |_, _| Ok(())).is_err());
            // Moderately compressible ASCII: rejection cannot be explained by
            // the compressed-size or 100x ratio gates instead of decoded size.
            let mut state = 17_u32;
            let data: Vec<u8> = (0..=limit).map(|_| {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                32 + ((state >> 24) & 63) as u8
            }).collect();
            let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
            encoder.write_all(&data).unwrap();
            let compressed = encoder.finish().unwrap();
            assert!(compressed.len() < limit && data.len() < compressed.len() * 100);
            assert!(fetch_podcast_resource(url, &resolver, &response(compressed, media, true),
                &mut |_, _| Ok(())).is_err());
            assert!(fetch_podcast_resource(url, &resolver, &response(b"login".to_vec(), "text/html", false),
                &mut |_, _| Ok(())).is_err());
            // The ordinary article fetch must not acquire arbitrary raster/XML support.
            assert!(fetch(url, &resolver, &response(b"not-html".to_vec(), media, false)).is_err());
        }
        let empty = T(std::sync::Mutex::new(vec![]));
        for bad in ["https://ampcode.com/podcast.rss?key=x", "https://evil.invalid/podcast.rss",
            "https://user@ampcode.com/podcast.rss", "https://i.ytimg.com/vi/abcdefghijk/other.png"] {
            assert!(fetch_podcast_resource(bad, &resolver, &empty, &mut |_, _| Ok(())).is_err());
        }
    }

    #[test]
    fn ip_matrix() {
        for x in [
            "127.0.0.1",
            "10.0.0.1",
            "100.64.0.1",
            "192.31.196.1",
            "192.52.193.1",
            "192.88.99.1",
            "192.175.48.1",
            "192.0.2.1",
            "198.18.0.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "fec0::1",
            "100::1",
            "2001:1::1",
            "2001:db8::1",
            "2620:4f:8000::1",
            "3fff::1",
            "5f00::1",
            "::ffff:8.8.8.8",
            "64:ff9b::808:808",
            "64:ff9b:1::808:808",
            "2002:0808:0808::1",
            "4000::1",
            "8000::1",
            "e000::1",
        ] {
            assert!(!is_global(x.parse().unwrap()), "{x}")
        }
        assert!(is_global("93.184.216.34".parse().unwrap()));
        assert!(is_global("2606:4700:4700::1111".parse().unwrap()))
    }
    #[test]
    fn any_non_public_dns_answer_rejects_the_whole_hop() {
        let resolver = R(BTreeMap::from([(
            "mixed.test".into(),
            vec![
                "93.184.216.34".parse().unwrap(),
                "127.0.0.1".parse().unwrap(),
            ],
        )]));
        let transport = T(std::sync::Mutex::new(vec![]));
        assert_eq!(
            fetch("https://mixed.test/", &resolver, &transport)
                .unwrap_err()
                .to_string(),
            "address_not_public"
        );
        let resolver = R(BTreeMap::from([(
            "many.test".into(),
            vec!["93.184.216.34".parse().unwrap(); MAX_RESOLVED_ADDRESSES + 1],
        )]));
        let error = fetch("https://many.test/", &resolver, &transport).unwrap_err();
        assert_eq!(error.disposition(), FetchDisposition::Unsupported);
    }

    #[test]
    fn benchmark_synthetic_native_answers_use_complete_validated_doh_answers() {
        let native = vec!["198.18.0.1".parse().unwrap()];
        let mut queries = Vec::new();
        let resolved = resolve_synthetic_fallback(native, |record_type| {
            queries.push(record_type);
            Ok(match record_type {
                DnsRecordType::A => vec!["93.184.216.34".parse().unwrap()],
                DnsRecordType::Aaaa => vec!["2606:4700:4700::1111".parse().unwrap()],
            })
        })
        .unwrap();
        assert_eq!(queries, [DnsRecordType::A, DnsRecordType::Aaaa]);
        assert_eq!(resolved.len(), 2);

        let ordinary = vec!["93.184.216.34".parse().unwrap()];
        assert_eq!(
            resolve_synthetic_fallback(ordinary.clone(), |_| panic!("unexpected DoH lookup"))
                .unwrap(),
            ordinary
        );
        let overflow = vec!["198.18.0.1".parse().unwrap(); MAX_RESOLVED_ADDRESSES + 1];
        assert_eq!(
            resolve_synthetic_fallback(overflow.clone(), |_| panic!("unexpected DoH lookup"))
                .unwrap(),
            overflow
        );
    }

    #[test]
    fn synthetic_fallback_rejects_mixed_empty_and_oversized_doh_answers() {
        let native = || vec!["198.19.0.1".parse().unwrap()];
        let mixed = resolve_synthetic_fallback(native(), |record_type| {
            Ok(match record_type {
                DnsRecordType::A => vec![
                    "93.184.216.34".parse().unwrap(),
                    "127.0.0.1".parse().unwrap(),
                ],
                DnsRecordType::Aaaa => vec![],
            })
        })
        .unwrap_err();
        assert_eq!(mixed.to_string(), "address_not_public");

        let empty = resolve_synthetic_fallback(native(), |_| Ok(vec![])).unwrap_err();
        assert_eq!(empty.to_string(), "address_not_public");

        let oversized = resolve_synthetic_fallback(native(), |record_type| {
            Ok(match record_type {
                DnsRecordType::A => {
                    vec!["93.184.216.34".parse().unwrap(); MAX_RESOLVED_ADDRESSES + 1]
                }
                DnsRecordType::Aaaa => vec![],
            })
        });
        // Duplicates are harmless and collapse before enforcing the unique-address bound.
        assert_eq!(oversized.unwrap().len(), 1);
        let oversized = resolve_synthetic_fallback(native(), |record_type| {
            Ok(match record_type {
                DnsRecordType::A => (1..=MAX_RESOLVED_ADDRESSES + 1)
                    .map(|suffix| IpAddr::V4(Ipv4Addr::new(93, 184, 216, suffix as u8)))
                    .collect(),
                DnsRecordType::Aaaa => vec![],
            })
        })
        .unwrap_err();
        assert_eq!(oversized.to_string(), "address_not_public");
    }

    #[test]
    fn fetch_failure_preserves_bounded_network_policy_reasons() {
        for (input, expected) in [
            ("address_not_public", "address_not_public"),
            ("redirect_downgrade", "network_policy_rejected"),
            ("content_type_rejected", "unsupported_content"),
        ] {
            let failure = FetchFailure::from(anyhow::anyhow!(input));
            assert_eq!(failure.reason_code(), expected);
            assert_eq!(failure.disposition(), FetchDisposition::Unsupported);
        }
    }
    #[test]
    fn redirects_and_policy() {
        let t = T(std::sync::Mutex::new(vec![
            RawResponse {
                status: 302,
                headers: vec![("location".into(), "https://b.test/x".into())],
                body: vec![],
            },
            RawResponse {
                status: 200,
                headers: vec![("content-type".into(), "text/html; charset=utf-8".into())],
                body: b"<article>x</article>".to_vec(),
            },
        ]));
        assert_eq!(fetch("https://a.test/", &res(), &t).unwrap().redirects, 1);
        let t = T(std::sync::Mutex::new(vec![RawResponse {
            status: 302,
            headers: vec![("location".into(), "http://b.test".into())],
            body: vec![],
        }]));
        assert_eq!(
            fetch("https://a.test/", &res(), &t)
                .unwrap_err()
                .to_string(),
            "network_policy_rejected"
        );
    }

    #[test]
    fn every_redirect_is_resolved_validated_and_pinned_again() {
        let first: IpAddr = "93.184.216.34".parse().unwrap();
        let second: IpAddr = "93.184.216.35".parse().unwrap();
        let resolver = RecordingResolver {
            addresses: BTreeMap::from([
                ("a.test".into(), vec![first]),
                ("b.test".into(), vec![second]),
            ]),
            calls: Default::default(),
        };
        let transport = RecordingTransport {
            responses: std::sync::Mutex::new(vec![
                RawResponse {
                    status: 302,
                    headers: vec![("location".into(), "https://b.test/article".into())],
                    body: vec![],
                },
                RawResponse {
                    status: 200,
                    headers: vec![("content-type".into(), "text/html".into())],
                    body: b"<article>safe</article>".to_vec(),
                },
            ]),
            calls: Default::default(),
        };
        assert_eq!(
            fetch("https://a.test/start", &resolver, &transport)
                .unwrap()
                .redirects,
            1
        );
        assert_eq!(
            resolver.calls.lock().unwrap().as_slice(),
            ["a.test", "b.test"]
        );
        assert_eq!(
            transport.calls.lock().unwrap().as_slice(),
            [
                ("a.test".into(), vec![SocketAddr::new(first, 443)]),
                ("b.test".into(), vec![SocketAddr::new(second, 443)]),
            ]
        );
    }
    #[test]
    fn body_policy() {
        let t = T(std::sync::Mutex::new(vec![RawResponse {
            status: 200,
            headers: vec![("content-type".into(), "text/plain".into())],
            body: vec![],
        }]));
        assert_eq!(
            fetch("https://a.test/", &res(), &t)
                .unwrap_err()
                .to_string(),
            "unsupported_content"
        );
    }

    #[test]
    fn compression_size_ratio_and_deadline_are_bounded() {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(&vec![b'x'; 32 * 1024]).unwrap();
        let compressed = encoder.finish().unwrap();
        let headers = vec![
            ("content-type".into(), "text/html".into()),
            ("content-encoding".into(), "gzip".into()),
            ("content-length".into(), compressed.len().to_string()),
        ];
        assert_eq!(
            decode(
                &headers,
                compressed,
                std::time::Instant::now() + Duration::from_secs(1)
            )
            .unwrap_err()
            .to_string(),
            "compression_ratio_exceeded"
        );
        assert_eq!(
            decode(&[], vec![], std::time::Instant::now())
                .unwrap_err()
                .to_string(),
            "fetch_deadline"
        );
        assert_eq!(
            decode(
                &[("content-length".into(), (MAX_BODY + 1).to_string())],
                vec![0; MAX_BODY + 1],
                std::time::Instant::now() + Duration::from_secs(1)
            )
            .unwrap_err()
            .to_string(),
            "compressed_body_too_large"
        );

        let (worker, requests) = std::sync::mpsc::sync_channel::<DecodeRequest>(1);
        let slow = std::thread::spawn(move || {
            let request = requests.recv().unwrap();
            std::thread::sleep(Duration::from_millis(100));
            let _ = request.reply.send(Ok(request.compressed));
        });
        let started = std::time::Instant::now();
        assert_eq!(
            decode_with_worker(
                &[],
                b"bounded".to_vec(),
                started + Duration::from_millis(20),
                &worker,
                MAX_BODY,
            )
            .unwrap_err()
            .to_string(),
            "decompression_timeout"
        );
        slow.join().unwrap();
        let failure = FetchFailure::from(anyhow::anyhow!("decompression_timeout"));
        assert_eq!(failure.disposition(), FetchDisposition::FatalWorker);
    }

    #[test]
    fn reqwest_transport_pins_address_omits_credentials_and_bounds_headers() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = [0u8; 4096];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
            assert!(request.starts_with("get /article http/1.1\r\n"));
            assert!(request.contains("host: synthetic.test:"));
            for forbidden in ["authorization:", "cookie:", "referer:", "proxy-"] {
                assert!(!request.contains(forbidden), "{forbidden}");
            }
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: 20\r\nConnection: close\r\n\r\n<article>x</article>",
                )
                .unwrap();
        });
        let url = Url::parse(&format!("http://synthetic.test:{}/article", address.port())).unwrap();
        let response = ReqwestTransport::default()
            .get(&url, &[address], Duration::from_secs(2))
            .unwrap();
        assert_eq!(response.body, b"<article>x</article>");
        server.join().unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            let large = "x".repeat(MAX_HEADER_BYTES + 1);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nX-Large: {large}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
        });
        let url = Url::parse(&format!("http://synthetic.test:{}/", address.port())).unwrap();
        assert!(ReqwestTransport::default()
            .get(&url, &[address], Duration::from_secs(2))
            .is_err());
        server.join().unwrap();
    }

    #[test]
    fn reqwest_transport_uses_an_absolute_timeout_and_rejects_large_length() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            std::thread::sleep(Duration::from_millis(150));
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 0\r\n\r\n",
            );
        });
        let url = Url::parse(&format!("http://synthetic.test:{}/", address.port())).unwrap();
        let error = ReqwestTransport::default()
            .get(&url, &[address], Duration::from_millis(30))
            .err()
            .unwrap();
        assert_eq!(error.to_string(), "fetch_deadline");
        server.join().unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                MAX_BODY + 1
            )
            .unwrap();
        });
        let url = Url::parse(&format!("http://synthetic.test:{}/", address.port())).unwrap();
        let error = ReqwestTransport::default()
            .get(&url, &[address], Duration::from_secs(2))
            .err()
            .unwrap();
        assert_eq!(error.to_string(), "compressed_body_too_large");
        server.join().unwrap();
    }
}
