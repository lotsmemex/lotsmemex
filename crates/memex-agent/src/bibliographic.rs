//! Mac-side orchestration for explicit single-item bibliographic lookups.
//!
//! The Store authorizes and freezes; this module only performs the one
//! approved HTTPS request in between, outside the Store writer lock.

use crate::{
    fetch::{
        fetch_metadata_lookup, HttpTransport, RateLimited, ReqwestTransport, Resolver,
        SystemResolver,
    },
    ipc::{Command, Response},
    queue::Queue,
};
use anyhow::{bail, Result};
use memex_core::{ItemIdentifier, Sensitivity};
use serde::{Deserialize, Serialize};
use std::{path::Path, time::Duration};
use url::Url;

/// Fits inside the IPC client's 5s read deadline; a slow provider fails as
/// `lookup_unavailable` rather than holding the single IPC loop.
pub const LOOKUP_FETCH_BUDGET: Duration = Duration::from_secs(4);
/// Minimum spacing between lookups to one provider origin.
pub const LOOKUP_MIN_SPACING_SECONDS: i64 = 1;
pub const BIBLIOGRAPHIC_METADATA_V2_FEATURE: &str = "bibliographic_metadata_v2";
pub const GROUPS_V1_FEATURE: &str = "groups_v1";
pub const TRANSLATION_PROVIDER_V1_FEATURE: &str = "translation_provider_v1";
pub const ITEM_CHAT_V1_FEATURE: &str = "item_chat_v1";
pub const AI_INSIGHTS_V1_FEATURE: &str = "ai_insights_v1";
/// `minutes_request`: AI minutes are generated only on this explicit request.
pub const MINUTES_REQUEST_V1_FEATURE: &str = "minutes_request_v1";
/// `dedup_plan`, `dedup_apply`, `trash_list`, `item_trash_restore` (no hard delete).
pub const DEDUP_TRASH_V1_FEATURE: &str = "dedup_trash_v1";

/// groups_v1 protocol: tree read plus library_query `group_id` /
/// `include_descendants` and `groups` facets, and single-op `group_edit` writes
/// (`write_ops`). No move/reparent or delete.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupsCapability {
    pub tree_page_limit: usize,
    pub query_group_filter: bool,
    #[serde(default)]
    pub query_unfiled: bool,
    #[serde(default)]
    pub node_delete: bool,
    pub query_group_facets: bool,
    pub writes: bool,
    pub write_ops: Vec<String>,
    pub moves: bool,
    #[serde(default)]
    pub trash: bool,
}

/// Injectable network for lookups; production uses the pinned resolver and
/// reqwest transport shared with capture.
pub struct LookupNetwork {
    resolver: Box<dyn Resolver>,
    transport: Box<dyn HttpTransport>,
}

impl LookupNetwork {
    pub fn new(resolver: impl Resolver + 'static, transport: impl HttpTransport + 'static) -> Self {
        Self {
            resolver: Box::new(resolver),
            transport: Box::new(transport),
        }
    }
    pub fn production(contact: Option<String>) -> Self {
        Self::new(
            SystemResolver::default(),
            ReqwestTransport::with_metadata_contact(contact),
        )
    }
    #[cfg(test)]
    pub(crate) fn resolver_for_test(&self) -> &dyn Resolver {
        self.resolver.as_ref()
    }
    #[cfg(test)]
    pub(crate) fn transport_for_test(&self) -> &dyn HttpTransport {
        self.transport.as_ref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LookupSourceCapability {
    pub provider: memex_store::LookupProvider,
    pub identifier_kind: memex_core::IdentifierKind,
    pub route: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BibliographicCapability {
    pub metadata_schema_version: u32,
    pub normalizer_version: String,
    pub lookup_adapter_version: String,
    pub lookup_sources: Vec<LookupSourceCapability>,
    pub lookup_preview_ttl_seconds: i64,
    pub lookup_batch: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCapabilities {
    pub schema_version: u32,
    pub features: Vec<String>,
    pub bibliographic_metadata_v2: BibliographicCapability,
    pub groups_v1: GroupsCapability,
    /// Filled per request from model settings + credential status (no network).
    pub translation_provider_v1: crate::provider_generation::TranslationProviderCapability,
    /// Per-kind availability from saved routes (summary: highlights/quotes; chat: fact_check).
    #[serde(default)]
    pub ai_insights_v1: Option<crate::insights::InsightsCapability>,
}

pub fn capabilities() -> AgentCapabilities {
    AgentCapabilities {
        schema_version: 1,
        features: vec![
            BIBLIOGRAPHIC_METADATA_V2_FEATURE.into(),
            GROUPS_V1_FEATURE.into(),
            TRANSLATION_PROVIDER_V1_FEATURE.into(),
            ITEM_CHAT_V1_FEATURE.into(),
            AI_INSIGHTS_V1_FEATURE.into(),
            DEDUP_TRASH_V1_FEATURE.into(),
            MINUTES_REQUEST_V1_FEATURE.into(),
            "markdown_edit_v1".into(),
            "reading_annotations_v1".into(),
        ],
        bibliographic_metadata_v2: BibliographicCapability {
            metadata_schema_version: memex_store::METADATA_SCHEMA_VERSION,
            normalizer_version: memex_core::IDENTIFIER_NORMALIZER_VERSION.into(),
            lookup_adapter_version: memex_store::LOOKUP_ADAPTER_VERSION.into(),
            lookup_sources: memex_store::LookupProvider::ALL
                .into_iter()
                .map(|provider| LookupSourceCapability {
                    provider,
                    identifier_kind: provider.identifier_kind(),
                    route: provider.route_template().into(),
                })
                .collect(),
            lookup_preview_ttl_seconds: memex_store::LOOKUP_PREVIEW_TTL_SECONDS,
            lookup_batch: false,
        },
        groups_v1: GroupsCapability {
            tree_page_limit: memex_store::MAX_GROUP_TREE_PAGE,
            query_group_filter: true,
            query_unfiled: true,
            node_delete: false,
            query_group_facets: true,
            writes: true,
            write_ops: crate::groups::WRITE_OPS
                .iter()
                .map(|op| (*op).into())
                .collect(),
            moves: false,
            trash: true,
        },
        translation_provider_v1:
            crate::provider_generation::TranslationProviderCapability::unavailable(),
        ai_insights_v1: None,
    }
}

fn lookup_error_code(error: &anyhow::Error) -> &'static str {
    match error.to_string().as_str() {
        "lookup_policy_rejected"
        | "url_policy_rejected"
        | "url_port_forbidden"
        | "address_not_public"
        | "host_missing" => "lookup_policy_rejected",
        "lookup_redirect_rejected" => "lookup_redirect_rejected",
        "lookup_not_found" => "lookup_not_found",
        "lookup_response_too_large"
        | "compressed_body_too_large"
        | "decoded_body_too_large"
        | "compression_ratio_exceeded" => "lookup_response_too_large",
        "content_type_rejected"
        | "charset_rejected"
        | "body_not_utf8"
        | "content_length_mismatch"
        | "content_encoding_rejected"
        | "duplicate_response_header"
        | "headers_too_large"
        | "decompression_failed" => "invalid_lookup_response",
        _ => "lookup_unavailable",
    }
}

/// One explicitly requested lookup of one identifier of one item. There is
/// no batch or background entry point.
pub(crate) fn lookup_preview(
    queue: &mut Queue,
    library: &Path,
    network: &LookupNetwork,
    item_id: &str,
    expected_revision_id: &str,
    identifier: &ItemIdentifier,
    max_sensitivity: Sensitivity,
) -> Result<memex_store::LookupPreviewPage> {
    // Authorization, CAS and identifier validity are decided before any I/O.
    let target = memex_store::prepare_bibliographic_lookup(
        library,
        item_id,
        expected_revision_id,
        identifier,
        max_sensitivity,
    )?;
    let url =
        Url::parse(&target.request_url).map_err(|_| anyhow::anyhow!("invalid_lookup_request"))?;
    let origin = url.origin().ascii_serialization();
    let now = chrono::Utc::now().timestamp();
    if queue
        .reserve_lookup_origin(&origin, now, LOOKUP_MIN_SPACING_SECONDS)?
        .is_some()
    {
        bail!("lookup_rate_limited")
    }
    let fetched = match fetch_metadata_lookup(
        &url,
        network.resolver.as_ref(),
        network.transport.as_ref(),
        LOOKUP_FETCH_BUDGET,
    ) {
        Ok(fetched) => fetched,
        Err(error) => {
            if let Some(limited) = error.downcast_ref::<RateLimited>() {
                queue.defer_capture_origin(&limited.origin, limited.not_before)?;
                bail!("lookup_rate_limited")
            }
            bail!(lookup_error_code(&error))
        }
    };
    let preview = memex_store::create_lookup_preview(
        library,
        &target,
        &fetched.body,
        &chrono::Utc::now().to_rfc3339(),
        max_sensitivity,
    )?;
    // Bounded first page; the frozen preview is read further by preview_id.
    memex_store::page_lookup_preview(&preview, false, 0, memex_store::MAX_LOOKUP_CANDIDATE_PAGE)
}

/// IPC handler for the bibliographic_metadata_v2 commands. Returns the
/// command unchanged when it belongs to another module.
pub(crate) fn dispatch(
    q: &mut Queue,
    library: &Path,
    lookup: &LookupNetwork,
    c: Command,
) -> Result<std::result::Result<Response, Command>> {
    let mut response = Response::empty_ok();
    match c {
        Command::BibliographicMetadataRead {
            item_id,
            max_sensitivity,
        } => {
            response.bibliographic_metadata = Some(
                memex_store::read_item_metadata_v2(library, &item_id, max_sensitivity)?
                    .ok_or_else(|| anyhow::anyhow!("library_resource_not_found"))?,
            );
        }
        Command::BibliographicMetadataEdit {
            max_sensitivity,
            edit,
        } => {
            // A metadata edit is not a generation authorization: no translation
            // or summary is enqueued here (the bounded reconciler owns that).
            let item_id = edit.item_id.clone();
            response.item_update = Some(memex_store::revise_item_metadata_v2(
                library,
                &item_id,
                edit,
                max_sensitivity,
            )?);
        }
        Command::BibliographicDuplicateCheck {
            identifiers,
            exclude_item_id,
            max_sensitivity,
        } => {
            response.identifier_duplicates = Some(memex_store::find_identifier_duplicates(
                library,
                &identifiers,
                max_sensitivity,
                exclude_item_id.as_deref(),
            )?);
        }
        Command::BibliographicLookupPreview {
            item_id,
            expected_revision_id,
            identifier,
            max_sensitivity,
        } => {
            response.lookup_preview = Some(lookup_preview(
                q,
                library,
                lookup,
                &item_id,
                &expected_revision_id,
                &identifier,
                max_sensitivity,
            )?);
        }
        Command::BibliographicLookupPreviewRead {
            preview_id,
            max_sensitivity,
            candidate_offset,
            candidate_limit,
        } => {
            response.lookup_preview = Some(memex_store::read_lookup_preview(
                library,
                &preview_id,
                max_sensitivity,
                candidate_offset,
                candidate_limit,
            )?);
        }
        Command::BibliographicLookupValueRead {
            preview_id,
            candidate_id,
            side,
            offset,
            max_sensitivity,
        } => {
            response.lookup_value = Some(memex_store::read_lookup_value(
                library,
                &preview_id,
                &candidate_id,
                side,
                offset,
                max_sensitivity,
            )?);
        }
        other => return Ok(Err(other)),
    }
    Ok(Ok(response))
}
