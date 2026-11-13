//! The place the owner explicitly chose in the sidebar (`ui_active_group_set`):
//! a library root or folder, or `null` for a virtual view (Inbox, 全部条目,
//! types, sources, the trash). The Chrome extension files a capture there when
//! 「收进 Inbox」 is not ticked. This is UI state only: it moves no items.
//!
//! Contract: `interfaces/memex.chrome_capture_v1.md` 「Active UI destination」
//! (B worktree, sha256 5538e0d9…e1c0). Everything tied to it is in this file:
//! the feature name, the command (`Command::UiActiveGroupSet` in `dto.rs`
//! mirrors it), the acknowledgement and its checks.
//!
//! Only explicit navigation is reported, never a restored, cached or refreshed
//! selection. The server keeps the last *accepted* explicit selection and an
//! exact replay of an older request never becomes latest again, so the client
//! does not order requests itself; it only never resends a superseded one.

use crate::{Client, ClientError, Command, Sensitivity};
use serde::Deserialize;

/// The UDS `capabilities.features` entry that gates `ui_active_group_set`.
/// (`url-jobs-active-group-v1` is the extension's HTTP intake gate, not
/// advertised over the UDS.)
pub const ACTIVE_GROUP_FEATURE: &str = "ui_active_group_v1";

/// Ceiling sent with every selection: the owner's own UI.
pub const ACTIVE_GROUP_CEILING: Sensitivity = Sensitivity::Restricted;

/// Acknowledgement schema the contract defines.
pub const ACTIVE_GROUP_SCHEMA: u32 = 1;

/// `ui_active_group` in the reply, `{schema_version:1, request_id, group_id,
/// updated_at}`: the acknowledgement of one request (a replay returns the
/// original one, `updated_at` included).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ActiveGroupAck {
    pub schema_version: u32,
    pub request_id: String,
    pub group_id: Option<String>,
    /// Server UTC time of the acknowledged write.
    pub updated_at: String,
}

impl Client {
    /// The agent advertises the feature. A failed capability probe counts as
    /// "no": nothing is sent then.
    pub fn active_group_supported(&self) -> bool {
        self.capabilities().is_ok_and(|c| c.features.iter().any(|f| f == ACTIVE_GROUP_FEATURE))
    }

    /// One `ui_active_group_set`. Never sent without the feature. The reply
    /// must acknowledge this request and this selection.
    pub fn ui_active_group_set(&self, request_id: &str, group_id: Option<&str>) -> Result<ActiveGroupAck, ClientError> {
        if !self.active_group_supported() {
            return Err(ClientError::Unsupported);
        }
        let command = Command::UiActiveGroupSet { request_id: request_id.into(), group_id: group_id.map(str::to_string), max_sensitivity: ACTIVE_GROUP_CEILING };
        let ack = self.backend.send(&command)?.ui_active_group.ok_or(ClientError::InvalidFrame)?;
        if ack.schema_version != ACTIVE_GROUP_SCHEMA || ack.request_id != request_id || ack.group_id.as_deref() != group_id {
            return Err(ClientError::InvalidFrame);
        }
        Ok(ack)
    }

    /// Reports one explicit selection (blocking; call off the UI thread). An
    /// unknown outcome is retried once with the same `request_id`, unless a
    /// newer selection was made meanwhile (`superseded`); any other failure is
    /// final. The caller ignores the result: navigation never waits on it.
    pub fn report_active_group(&self, request_id: &str, group_id: Option<&str>, superseded: impl Fn() -> bool) -> Result<ActiveGroupAck, ClientError> {
        match self.ui_active_group_set(request_id, group_id) {
            Err(e) if e.is_transport() && !superseded() => self.ui_active_group_set(request_id, group_id),
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::fixture::{FixtureBackend, FixtureControls};
    use crate::{Client, ClientError, Command};

    const FOLDER: &str = "grp_c_sleep";

    #[test]
    fn wire_shape_sends_null_for_a_virtual_view() {
        let command = Command::UiActiveGroupSet { request_id: "r1".into(), group_id: None, max_sensitivity: super::ACTIVE_GROUP_CEILING };
        let json = serde_json::to_value(&command).unwrap();
        assert_eq!(json, serde_json::json!({"command": "ui_active_group_set", "request_id": "r1", "group_id": null, "max_sensitivity": "restricted"}));
        assert!(command.is_capability_gated());
        // The acknowledgement as the contract writes it; one without its schema or time is not one.
        let reply: crate::Response = serde_json::from_value(serde_json::json!({"ok": true, "ui_active_group": {"schema_version": 1, "request_id": "r1", "group_id": null, "updated_at": "2026-11-08T12:00:00Z"}})).unwrap();
        assert_eq!(reply.ui_active_group.unwrap().group_id, None);
        assert!(serde_json::from_value::<super::ActiveGroupAck>(serde_json::json!({"request_id": "r1", "group_id": null, "updated_at": "t"})).is_err());
        assert!(serde_json::from_value::<super::ActiveGroupAck>(serde_json::json!({"schema_version": 1, "request_id": "r1", "group_id": null})).is_err());
    }

    #[test]
    fn folder_and_null_selections_reach_the_agent() {
        let fixture = FixtureBackend::new(16);
        let client = Client::new(fixture.clone());
        assert_eq!(client.report_active_group("req-a", Some(FOLDER), || false).unwrap().group_id.as_deref(), Some(FOLDER));
        assert_eq!(fixture.active_group(), Some(FOLDER.into()));
        assert_eq!(client.report_active_group("req-b", None, || false).unwrap().group_id, None);
        assert_eq!(fixture.active_group(), None);
        assert_eq!(fixture.active_group_calls(), vec![("req-a".into(), Some(FOLDER.into())), ("req-b".into(), None)]);
        // An unknown folder is refused, not retried.
        assert_eq!(client.report_active_group("req-c", Some("grp_missing"), || false), Err(ClientError::Daemon("library_resource_not_found".into())));
        assert_eq!(fixture.active_group_calls().len(), 3);
    }

    #[test]
    fn without_the_feature_nothing_is_sent() {
        let fixture = FixtureBackend::new(16);
        fixture.set_controls(FixtureControls { no_active_group: true, ..Default::default() });
        let client = Client::new(fixture.clone());
        assert!(!client.active_group_supported());
        assert_eq!(client.report_active_group("req-a", Some(FOLDER), || false), Err(ClientError::Unsupported));
        assert!(fixture.active_group_calls().is_empty());
        // An agent that cannot even answer `capabilities` is treated the same.
        fixture.set_controls(FixtureControls { legacy_agent: true, ..Default::default() });
        client.forget_capabilities();
        assert_eq!(client.report_active_group("req-b", None, || false), Err(ClientError::Unsupported));
        assert!(fixture.active_group_calls().is_empty());
    }

    #[test]
    fn unknown_outcome_is_retried_once_with_the_same_request_id() {
        let fixture = FixtureBackend::new(16);
        let client = Client::new(fixture.clone());
        // The first reply is lost: one retry, same id; the replay is acknowledged.
        fixture.set_controls(FixtureControls { lose_active_group_replies: 1, ..Default::default() });
        assert!(client.report_active_group("req-a", Some(FOLDER), || false).is_ok());
        assert_eq!(fixture.active_group_calls(), vec![("req-a".into(), Some(FOLDER.into())); 2]);
        assert_eq!(fixture.active_group(), Some(FOLDER.into()));

        // Both replies lost: exactly one retry, then given up.
        fixture.set_controls(FixtureControls { lose_active_group_replies: 5, ..Default::default() });
        assert_eq!(client.report_active_group("req-b", None, || false), Err(ClientError::Timeout));
        assert_eq!(fixture.active_group_calls()[2..], [("req-b".into(), None), ("req-b".into(), None)]);

        // Superseded by a newer click: no retry.
        assert!(client.report_active_group("req-c", Some(FOLDER), || true).is_err());
        assert_eq!(fixture.active_group_calls().len(), 5);
    }

    #[test]
    fn an_old_replay_does_not_become_the_selection_again() {
        let fixture = FixtureBackend::new(16);
        let client = Client::new(fixture.clone());
        client.report_active_group("req-old", Some(FOLDER), || false).unwrap();
        client.report_active_group("req-new", None, || false).unwrap();
        // The original acknowledgement, while the newer selection stays.
        assert_eq!(client.report_active_group("req-old", Some(FOLDER), || false).unwrap().request_id, "req-old");
        assert_eq!(fixture.active_group(), None);
        assert_eq!(client.report_active_group("req-old", None, || false), Err(ClientError::Daemon("request_identity_conflict".into())));
    }
}
