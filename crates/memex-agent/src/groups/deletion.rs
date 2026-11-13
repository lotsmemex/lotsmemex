//! Owner-UDS-only organization deletion. Remote routes are deliberately denied.
use crate::ipc::{Command, Response};
use anyhow::{bail, ensure, Result};
use memex_core::Sensitivity;
use memex_store::group_delete;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

const OWNER: &str = "owner_local";
const TTL: Duration = Duration::from_secs(120);
const CAPACITY: usize = 1024;
const PAGE: usize = 50;

#[derive(Clone)]
enum Purpose {
    Preview { group_id: String, impact: String },
    Cursor { view: String, offset: usize },
}

#[derive(Clone)]
struct Token {
    library: PathBuf,
    ceiling: Sensitivity,
    expires: Instant,
    purpose: Purpose,
}

fn tokens() -> &'static Mutex<BTreeMap<String, Token>> {
    static TOKENS: OnceLock<Mutex<BTreeMap<String, Token>>> = OnceLock::new();
    TOKENS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn insert(library: &Path, ceiling: Sensitivity, purpose: Purpose) -> Result<String> {
    let mut entries = tokens()
        .lock()
        .map_err(|_| anyhow::anyhow!("group_delete_unavailable"))?;
    let now = Instant::now();
    entries.retain(|_, value| value.expires > now);
    ensure!(entries.len() < CAPACITY, "group_delete_preview_limit");
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|_| anyhow::anyhow!("group_delete_unavailable"))?;
    let key = hex::encode(bytes);
    entries.insert(
        key.clone(),
        Token {
            library: library.to_owned(),
            ceiling,
            expires: now + TTL,
            purpose,
        },
    );
    Ok(key)
}

fn lookup(key: &str, library: &Path, ceiling: Sensitivity) -> Result<Token> {
    ensure!(
        key.len() == 64 && key.bytes().all(|c| c.is_ascii_hexdigit()),
        "group_delete_preview_not_found"
    );
    let entries = tokens()
        .lock()
        .map_err(|_| anyhow::anyhow!("group_delete_unavailable"))?;
    let token = entries
        .get(key)
        .ok_or_else(|| anyhow::anyhow!("group_delete_preview_not_found"))?;
    ensure!(
        token.library == library && token.ceiling == ceiling,
        "group_delete_preview_not_found"
    );
    ensure!(
        token.expires > Instant::now(),
        "group_delete_preview_expired"
    );
    Ok(token.clone())
}

fn digest(value: impl serde::Serialize) -> Result<String> {
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&value)?)
    ))
}

pub(crate) fn dispatch(library: &Path, command: Command) -> Result<Response> {
    let library = library.canonicalize()?;
    let mut response = Response::empty_ok();
    match command {
        Command::GroupDeletePreview {
            group_id,
            max_sensitivity,
        } => {
            let impact = group_delete::preview(&library, &group_id, max_sensitivity)?;
            let token = insert(
                &library,
                max_sensitivity,
                Purpose::Preview {
                    group_id: group_id.clone(),
                    impact: impact.impact_sha256,
                },
            )?;
            response.group_delete_preview = Some(json!({
                "group_id": group_id, "name": impact.name, "confirmation_token": token,
                "expires_at": (chrono::Utc::now() + chrono::Duration::seconds(TTL.as_secs() as i64)).to_rfc3339(),
                "affected_node_count": impact.affected_node_count,
                "affected_membership_count": impact.affected_membership_count,
                "distinct_item_count": impact.distinct_item_count,
                "includes_descendants": true, "items_retained": true, "recoverable": true,
            }));
        }
        Command::GroupDelete {
            group_id,
            confirmation_token,
            request_id,
            max_sensitivity,
        } => {
            ensure!(
                confirmation_token.len() <= 256,
                "invalid_group_delete_request"
            );
            let hash = digest(("group_delete", &group_id, &confirmation_token))?;
            let receipt = if let Some(receipt) =
                group_delete::replay(&library, OWNER, &request_id, &hash, max_sensitivity)?
            {
                receipt
            } else {
                let token = lookup(&confirmation_token, &library, max_sensitivity)?;
                let Purpose::Preview {
                    group_id: expected,
                    impact,
                } = token.purpose
                else {
                    bail!("group_delete_preview_not_found")
                };
                ensure!(expected == group_id, "group_delete_preview_not_found");
                group_delete::delete(
                    &library,
                    OWNER,
                    &request_id,
                    &hash,
                    &group_id,
                    &impact,
                    max_sensitivity,
                    token.expires,
                )?
            };
            tokens()
                .lock()
                .map_err(|_| anyhow::anyhow!("group_delete_unavailable"))?
                .remove(&confirmation_token);
            response.group_delete = Some(serde_json::to_value(receipt)?);
        }
        Command::GroupDeleteRestore {
            deletion_id,
            request_id,
            max_sensitivity,
        } => {
            let hash = digest(("group_delete_restore", &deletion_id))?;
            response.group_delete_restore = Some(serde_json::to_value(group_delete::restore(
                &library,
                OWNER,
                &request_id,
                &hash,
                &deletion_id,
                max_sensitivity,
            )?)?);
        }
        Command::GroupDeleteList {
            cursor,
            max_sensitivity,
        } => {
            // Store authorizes the entire retained object before yielding each entry.
            let entries = group_delete::list(&library, max_sensitivity)?;
            let view = digest((&library, OWNER, max_sensitivity, &entries))?;
            let offset = match cursor {
                None => 0,
                Some(cursor) => {
                    let token = lookup(&cursor, &library, max_sensitivity)
                        .map_err(|_| anyhow::anyhow!("stale_group_delete_view"))?;
                    let Purpose::Cursor {
                        view: expected,
                        offset,
                    } = token.purpose
                    else {
                        bail!("stale_group_delete_view")
                    };
                    ensure!(
                        view == expected && offset <= entries.len(),
                        "stale_group_delete_view"
                    );
                    offset
                }
            };
            let end = (offset + PAGE).min(entries.len());
            response.group_deletions = Some(serde_json::to_value(&entries[offset..end])?);
            if end < entries.len() {
                response.next_cursor = Some(insert(
                    &library,
                    max_sensitivity,
                    Purpose::Cursor { view, offset: end },
                )?);
            }
        }
        _ => bail!("invalid_group_delete_request"),
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use memex_store::{edit_groups, GroupEditOp};

    fn call(root: &Path, value: serde_json::Value) -> Result<Response> {
        dispatch(root, serde_json::from_value(value)?)
    }

    fn root_group(root: &Path, name: &str, ceiling: Sensitivity) -> memex_store::GroupEditReceipt {
        edit_groups(
            root,
            name,
            GroupEditOp::CreateRoot {
                name: name.into(),
                sensitivity: ceiling,
            },
            Sensitivity::Restricted,
        )
        .unwrap()
    }

    fn preview(root: &Path, id: &str) -> String {
        call(
            root,
            json!({"command":"group_delete_preview", "group_id":id, "max_sensitivity":"public"}),
        )
        .unwrap()
        .group_delete_preview
        .unwrap()["confirmation_token"]
            .as_str()
            .unwrap()
            .into()
    }

    #[test]
    fn confirmation_is_bound_expiring_and_replay_survives_cache_loss_and_restore() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let group = root_group(root, "Main", Sensitivity::Public);
        let path = root.join("catalog/groups/state.json");
        let original = std::fs::read(&path).unwrap();
        let token = preview(root, &group.group_id);
        call(
            root,
            json!({"command":"group_delete_list", "cursor":null, "max_sensitivity":"public"}),
        )
        .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let request = json!({"command":"group_delete", "group_id":group.group_id,
            "confirmation_token":token, "request_id":"delete", "max_sensitivity":"public"});
        let mut bad = request.clone();
        bad["confirmation_token"] = "0".repeat(64).into();
        assert!(call(root, bad).is_err());
        let other = tempfile::tempdir().unwrap();
        root_group(other.path(), "Other", Sensitivity::Public);
        assert!(call(other.path(), request.clone()).is_err());
        let mut narrowed = request.clone();
        narrowed["max_sensitivity"] = "internal".into();
        assert!(call(root, narrowed).is_err());
        tokens().lock().unwrap().get_mut(&token).unwrap().expires =
            Instant::now() - Duration::from_secs(1);
        assert_eq!(
            call(root, request).unwrap_err().to_string(),
            "group_delete_preview_expired"
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let stale = preview(root, &group.group_id);
        let renamed = edit_groups(
            root,
            "rename",
            GroupEditOp::Rename {
                group_id: group.group_id.clone(),
                expected_version: group.version,
                name: "Renamed".into(),
            },
            Sensitivity::Public,
        )
        .unwrap();
        let v1 = std::fs::read(&path).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&v1).unwrap()["state_version"],
            1
        );
        assert!(call(
            root,
            json!({"command":"group_delete", "group_id":group.group_id,
            "confirmation_token":stale, "request_id":"stale", "max_sensitivity":"public"})
        )
        .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), v1);
        let token = preview(root, &group.group_id);
        let request = json!({"command":"group_delete", "group_id":group.group_id,
            "confirmation_token":token, "request_id":"success", "max_sensitivity":"public"});
        let receipt = call(root, request.clone()).unwrap().group_delete.unwrap();
        assert_eq!(receipt["outcome"], "deleted");
        assert!(tokens().lock().unwrap().get(&token).is_none());
        assert_eq!(
            call(root, request.clone()).unwrap().group_delete.unwrap(),
            receipt
        );
        let restored = call(
            root,
            json!({"command":"group_delete_restore", "deletion_id":receipt["deletion_id"],
            "request_id":"restore", "max_sensitivity":"public"}),
        )
        .unwrap();
        assert_eq!(
            restored.group_delete_restore.unwrap()["outcome"],
            "restored"
        );
        assert_eq!(call(root, request).unwrap().group_delete.unwrap(), receipt);
        assert!(group_delete::list(root, Sensitivity::Public)
            .unwrap()
            .is_empty());
        assert_eq!(
            memex_store::read_group_tree(root, Sensitivity::Public, 0, 50, None)
                .unwrap()
                .nodes[0]
                .name,
            "Renamed"
        );
        assert_ne!(
            memex_store::read_group_tree(root, Sensitivity::Public, 0, 50, None)
                .unwrap()
                .nodes[0]
                .version,
            renamed.version
        );
        let wire = json!({"command":"group_delete_preview", "group_id":group.group_id,
            "max_sensitivity":"public", "principal":"local_owner"});
        assert!(serde_json::from_value::<Command>(wire).is_err());
    }

    #[test]
    fn deletion_pages_bind_authorized_view_and_ceiling_before_slicing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for n in 0..51 {
            let group = root_group(root, &format!("root-{n:02}"), Sensitivity::Public);
            let impact = group_delete::preview(root, &group.group_id, Sensitivity::Public).unwrap();
            group_delete::delete(
                root,
                OWNER,
                &format!("delete-{n}"),
                &digest(n).unwrap(),
                &group.group_id,
                &impact.impact_sha256,
                Sensitivity::Public,
                Instant::now() + TTL,
            )
            .unwrap();
        }
        let request =
            json!({"command":"group_delete_list", "cursor":null, "max_sensitivity":"public"});
        let first = call(root, request.clone()).unwrap();
        assert_eq!(
            first
                .group_deletions
                .as_ref()
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            50
        );
        let cursor = first.next_cursor.unwrap();
        let next =
            json!({"command":"group_delete_list", "cursor":cursor, "max_sensitivity":"public"});
        let hidden = root_group(root, "hidden", Sensitivity::Private);
        let impact = group_delete::preview(root, &hidden.group_id, Sensitivity::Private).unwrap();
        group_delete::delete(
            root,
            OWNER,
            "delete-hidden",
            &digest("hidden").unwrap(),
            &hidden.group_id,
            &impact.impact_sha256,
            Sensitivity::Private,
            Instant::now() + TTL,
        )
        .unwrap();
        let second = call(root, next.clone()).unwrap();
        assert_eq!(second.group_deletions.unwrap().as_array().unwrap().len(), 1);
        assert!(second.next_cursor.is_none());
        let mut elevated = next.clone();
        elevated["max_sensitivity"] = "private".into();
        assert!(call(root, elevated).is_err());
        let id = &first.group_deletions.unwrap()[0]["deletion_id"];
        call(
            root,
            json!({"command":"group_delete_restore", "deletion_id":id,
            "request_id":"restore", "max_sensitivity":"public"}),
        )
        .unwrap();
        assert_eq!(
            call(root, next).unwrap_err().to_string(),
            "stale_group_delete_view"
        );
    }
}
