//! The 「版本」 page's facts. One source of truth for the version: the workspace
//! `Cargo.toml` `[workspace.package] version` (every crate inherits it; Cargo
//! injects it as `CARGO_PKG_VERSION`). The build is identified by the running
//! binary itself: its SHA-256 (short id) and modification time (link time), and
//! the bundle id from the enclosing `Info.plist`. The agent's own version and
//! binary hash come from `agent_version` (2026-11-03 candidate): a hash still
//! being computed reads 「计算中…」, an agent without the command 「未知（代理未
//!报告）」 — never a made-up value. Schema version and features are shown as sent.

use memex_client::{Client, ClientError};
use sha2::{Digest, Sha256};
use std::path::Path;

/// `0.x.y`, shared by every crate of the desktop workspace.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Hex digits of the binary's SHA-256 shown as the build id.
pub const SHORT_ID_LEN: usize = 12;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildInfo {
    /// Full SHA-256 of the running binary (hex); `None` when it can't be read.
    pub sha256: Option<String>,
    /// Link time of the binary, local "YYYY-MM-DD HH:MM".
    pub built: Option<String>,
    /// `CFBundleIdentifier`; `None` when run outside an app bundle (`cargo run`).
    pub bundle_id: Option<String>,
}

impl BuildInfo {
    pub fn short_id(&self) -> Option<&str> {
        self.sha256.as_deref().map(|s| &s[..s.len().min(SHORT_ID_LEN)])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentInfo {
    /// The synthetic development fixture (no agent).
    Fixture { schema_version: Option<u32>, features: Vec<String> },
    /// A real agent; `schema_version` is `None` for one without `capabilities`.
    Agent { schema_version: Option<u32>, features: Vec<String> },
    /// The agent did not answer (a short reason).
    Unavailable(String),
}

/// The `agent_version` row values, as the phone shows them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentVersionInfo {
    Reported(memex_client::AgentVersion),
    /// No answer to `agent_version` (an older agent, or none reachable).
    NotReported,
}

impl AgentVersionInfo {
    /// (代理版本, 代理程序哈希).
    pub fn rows(&self) -> (String, String) {
        let unknown = || memex_client::tx("未知（代理未报告）").to_string();
        match self {
            AgentVersionInfo::Reported(v) => (
                Some(v.version.trim()).filter(|s| !s.is_empty()).map_or_else(unknown, str::to_string),
                v.binary_sha256.as_deref().map(str::trim).filter(|s| !s.is_empty()).map_or_else(|| memex_client::tx("计算中…").to_string(), str::to_string),
            ),
            AgentVersionInfo::NotReported => (unknown(), unknown()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionInfo {
    pub build: BuildInfo,
    pub agent: AgentInfo,
    pub agent_version: AgentVersionInfo,
}

/// Reads everything for the page. Blocking (hashes the binary); call off the UI thread.
pub fn read(client: &Client) -> VersionInfo {
    let exe = std::env::current_exe().ok();
    let build = BuildInfo {
        sha256: exe.as_deref().and_then(sha256_file),
        built: exe.as_deref().and_then(modified_local),
        bundle_id: exe.as_deref().and_then(bundle_id_of),
    };
    let agent = match client.capabilities() {
        Ok(c) if client.mode().is_fixture() => AgentInfo::Fixture { schema_version: c.schema_version, features: c.features },
        Ok(c) => AgentInfo::Agent { schema_version: c.schema_version, features: c.features },
        Err(e) => AgentInfo::Unavailable(agent_error(&e)),
    };
    let agent_version = match client.agent_version() {
        Ok(v) => AgentVersionInfo::Reported(v),
        Err(_) => AgentVersionInfo::NotReported,
    };
    VersionInfo { build, agent, agent_version }
}

fn agent_error(e: &ClientError) -> String {
    e.message()
}

fn sha256_file(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).ok()?;
    Some(format!("{:x}", hasher.finalize()))
}

fn modified_local(path: &Path) -> Option<String> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let epoch = modified.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
    Some(memex_client::biblio::local_datetime(epoch))
}

/// `…/X.app/Contents/MacOS/<binary>` → `…/X.app/Contents/Info.plist`'s bundle id.
fn bundle_id_of(exe: &Path) -> Option<String> {
    let contents = exe.parent()?.parent()?;
    if exe.parent()?.file_name()? != "MacOS" || contents.file_name()? != "Contents" {
        return None;
    }
    bundle_id(&std::fs::read_to_string(contents.join("Info.plist")).ok()?)
}

/// `CFBundleIdentifier` from an XML property list.
pub fn bundle_id(plist: &str) -> Option<String> {
    let after = &plist[plist.find("<key>CFBundleIdentifier</key>")? + "<key>CFBundleIdentifier</key>".len()..];
    let start = after.find("<string>")? + "<string>".len();
    let end = start + after[start..].find("</string>")?;
    // Only whitespace may sit between the key and its value.
    after[..start - "<string>".len()].trim().is_empty().then(|| after[start..end].trim().to_string()).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use memex_client::fixture::{FixtureBackend, FixtureControls};

    #[test]
    fn version_comes_from_the_workspace_and_the_build_id_is_a_sha_prefix() {
        assert_eq!(APP_VERSION.split('.').count(), 3);
        assert!(APP_VERSION.starts_with("0."));
        let b = BuildInfo { sha256: Some("1c71ebed7deda854aee3711b0239b3c7e6fce4327450a0a34c842975d139eacd".into()), built: None, bundle_id: None };
        assert_eq!(b.short_id(), Some("1c71ebed7ded"));
        assert_eq!(BuildInfo { sha256: None, built: None, bundle_id: None }.short_id(), None);
    }

    #[test]
    fn bundle_id_is_read_from_the_info_plist() {
        let plist = include_str!("../../../packaging/Info.plist");
        assert_eq!(bundle_id(plist).as_deref(), Some("com.lotsmemex.desktop.dev"));
        assert_eq!(bundle_id("<key>CFBundleIdentifier</key>\n  <string> a.b </string>").as_deref(), Some("a.b"));
        assert_eq!(bundle_id("<key>CFBundleIdentifier</key><true/><string>x</string>"), None);
        assert_eq!(bundle_id("<key>CFBundleName</key><string>x</string>"), None);
        assert_eq!(bundle_id_of(Path::new("/tmp/target/debug/memex-desktop")), None, "cargo run: no bundle");
    }

    #[test]
    fn the_running_binary_is_hashed_and_dated() {
        let exe = std::env::current_exe().unwrap();
        let sha = sha256_file(&exe).unwrap();
        assert_eq!(sha.len(), 64);
        assert!(modified_local(&exe).unwrap().len() == 16);
    }

    #[test]
    fn agent_facts_follow_capabilities_or_say_why_not() {
        let fixture = FixtureBackend::new(5);
        let info = read(&Client::new(fixture.clone()));
        let AgentInfo::Fixture { schema_version, features } = info.agent else { panic!("{:?}", info.agent) };
        assert_eq!(schema_version, Some(1));
        assert!(features.iter().any(|f| f == "item_chat_v1"));
        fixture.set_controls(FixtureControls { legacy_agent: true, ..Default::default() });
        let info = read(&Client::new(fixture.clone()));
        assert_eq!(info.agent, AgentInfo::Fixture { schema_version: None, features: vec![] }, "an agent without capabilities");
        fixture.set_controls(FixtureControls { offline: true, ..Default::default() });
        assert!(matches!(read(&Client::new(fixture)).agent, AgentInfo::Unavailable(_)));
    }

    #[test]
    fn agent_version_rows_never_invent_a_value() {
        memex_client::i18n::set_language(memex_client::Lang::Zh);
        let fixture = FixtureBackend::new(5);
        let rows = |c: FixtureControls| {
            fixture.set_controls(c);
            read(&Client::new(fixture.clone())).agent_version.rows()
        };
        assert_eq!(rows(FixtureControls::default()), ("0.0.0-synthetic".to_string(), "f1x7ure0c0de".to_string()));
        assert_eq!(rows(FixtureControls { agent_hashing: true, ..Default::default() }), ("0.0.0-synthetic".to_string(), "计算中…".to_string()), "a null hash is still being computed");
        let unknown = ("未知（代理未报告）".to_string(), "未知（代理未报告）".to_string());
        assert_eq!(rows(FixtureControls { no_agent_version: true, ..Default::default() }), unknown);
        assert_eq!(rows(FixtureControls { offline: true, ..Default::default() }), unknown);
        let v = memex_client::AgentVersion { version: "0.1.0".into(), binary_sha256: None, query_origin: false };
        assert_eq!(AgentVersionInfo::Reported(v).rows().1, "计算中…");
    }
}
