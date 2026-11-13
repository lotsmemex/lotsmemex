//! Non-secret identity references, never persisted permission decisions.
use super::*;
use memex_core::Sensitivity;

// Isolated implementation/probes only; no production listener or credential path.
#[cfg(test)]
mod control;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "origin", rename_all = "snake_case", deny_unknown_fields)]
pub enum Authority {
    LocalOwner,
    Remote {
        principal: String,
        session_id: String,
        grant_epoch: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Preview,
    Admission,
    Dispatch,
    Commit,
    Read,
    Stop,
    Reconcile,
}

pub struct Check<'a> {
    pub authority: &'a Authority,
    pub library: &'a Path,
    pub item_id: &'a str,
    pub phase: Phase,
    pub required_scope: &'static str,
    pub ceiling: Sensitivity,
}

impl Authority {
    pub fn principal(&self) -> &str {
        match self {
            Self::LocalOwner => "local_owner",
            Self::Remote { principal, .. } => principal,
        }
    }
    pub fn check(
        &self,
        library: &Path,
        item_id: &str,
        phase: Phase,
        ceiling: Sensitivity,
        verifier: &impl Fn(Check<'_>) -> Result<Sensitivity>,
    ) -> Result<Sensitivity> {
        if let Self::Remote {
            principal,
            session_id,
            grant_epoch,
        } = self
        {
            anyhow::ensure!(
                principal.len() == 64
                    && principal.bytes().all(|c| c.is_ascii_hexdigit())
                    && !session_id.is_empty()
                    && *grant_epoch > 0,
                "permission_denied"
            );
        }
        let required_scope = if matches!(phase, Phase::Read | Phase::Reconcile) {
            "read"
        } else {
            "edit"
        };
        Ok(verifier(Check {
            authority: self,
            library,
            item_id,
            phase,
            required_scope,
            ceiling,
        })?
        .min(ceiling))
    }
}

/// Not wired to the production remote gateway. No UDS-based owner fallback.
pub fn local_only(check: Check<'_>) -> Result<Sensitivity> {
    anyhow::ensure!(
        matches!(check.authority, Authority::LocalOwner),
        "permission_denied"
    );
    Ok(check.ceiling)
}
