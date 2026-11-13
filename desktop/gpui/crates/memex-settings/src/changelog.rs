//! 「更新日志」: the releases, newest first. The single source is the phone's
//! `mobile/web/changelog.mjs` (a JSON object after `export default `), read here at
//! build time so the phone and the Mac always list the same releases. The newest
//! entry is the app version: the workspace `Cargo.toml` version must equal it.

use serde::Deserialize;

const SOURCE: &str = include_str!("../../../../../mobile/web/changelog.mjs");
/// The large black-rose portrait shown above the releases.
pub const PORTRAIT_PNG: &[u8] = include_bytes!("../../../../../mobile/web/assets/memex-portrait.png");

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub date: String,
    pub platforms: Vec<String>,
    pub items: Vec<String>,
}

#[derive(Deserialize)]
struct Log {
    releases: Vec<Release>,
}

/// Parses the shared module. Panics only on a malformed source file (a build-time
/// asset; the tests below catch it before any release).
pub fn releases() -> Vec<Release> {
    let start = SOURCE.find("\nexport default ").expect("changelog.mjs: a line starting with export default") + "\nexport default ".len();
    let json = SOURCE[start..].trim().trim_end_matches(';');
    serde_json::from_str::<Log>(json).expect("changelog.mjs: valid JSON").releases
}

/// 「Mac · 手机」 for a release's platforms.
pub fn platforms_label(r: &Release) -> String {
    r.platforms
        .iter()
        .map(|p| match p.as_str() {
            "mac" => "Mac".to_string(),
            "phone" => memex_client::tx("手机").to_string(),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_release_is_the_app_version_and_versions_count_up_by_one() {
        let all = releases();
        assert_eq!(all[0].version, crate::version::APP_VERSION, "Cargo workspace version must equal the newest changelog entry");
        // v0.1.x is the x-th release: newest first, consecutive, starting at 0.1.1.
        let n = all.len();
        for (i, r) in all.iter().enumerate() {
            assert_eq!(r.version, format!("0.1.{}", n - i), "release {i}");
            assert!(!r.items.is_empty() && !r.platforms.is_empty(), "release {} has items and platforms", r.version);
            assert!(r.platforms.iter().all(|p| p == "mac" || p == "phone"), "release {}", r.version);
        }
        // The phone's version line must match the newest entry too.
        let app = include_str!("../../../../../mobile/web/app.mjs");
        assert!(app.contains(&format!("const APP_VERSION = '{}';", all[0].version)));
        assert!(app.contains(&format!("const BUILD_DATE = '{}';", all[0].date)));
        assert_eq!(&PORTRAIT_PNG[1..4], b"PNG");
    }
}
