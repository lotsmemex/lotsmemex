//! Shell-owned desktop preferences (view mode, appearance, interface language,
//! panel sizes, and the agent socket to use) and where they live.
//!
//! Location, first match wins:
//! 1. `MEMEX_DESKTOP_STATE` (development; lenient: a missing file means defaults).
//! 2. The locator `~/Library/Application Support/<data dir>DesktopDev/data-location.json`,
//!    `{"schema_version":1,"settings_path":"/abs/Desktop/settings.json"}`. Once a
//!    locator exists everything must be valid: a broken locator, a missing or
//!    unreadable settings file, or a missing `agent_socket` is an explicit error.
//! 3. Neither: development defaults in `<data dir>DesktopDev/layout.json`.
//!
//! Agent socket: `MEMEX_AGENT_SOCKET`, else the settings file's `agent_socket`, else
//! (only when nothing is configured) the labelled synthetic fixture. A configured
//! socket that is unreachable shows offline; nothing falls back to the fixture or to
//! another daemon.

use memex_client::{tx, txf, Lang, LanguagePref};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewMode {
    #[default]
    Reading,
    Classic,
}

/// Reader mode: 原文 / 对照 / 译文.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReaderMode {
    Original,
    #[default]
    Bilingual,
    Translation,
}

impl ReaderMode {
    pub fn to_reader(self) -> memex_reader::model::Mode {
        use memex_reader::model::Mode;
        match self {
            ReaderMode::Original => Mode::Original,
            ReaderMode::Bilingual => Mode::Bilingual,
            ReaderMode::Translation => Mode::Translation,
        }
    }

    pub fn from_reader(mode: memex_reader::model::Mode) -> Self {
        use memex_reader::model::Mode;
        match mode {
            Mode::Original => ReaderMode::Original,
            Mode::Bilingual => ReaderMode::Bilingual,
            Mode::Translation => ReaderMode::Translation,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    pub fn label(self) -> &'static str {
        tx(match self {
            Appearance::System => "跟随系统",
            Appearance::Light => "浅色",
            Appearance::Dark => "深色",
        })
    }
    /// Effective darkness given the system appearance.
    pub fn is_dark(self, system_dark: bool) -> bool {
        match self {
            Appearance::System => system_dark,
            Appearance::Light => false,
            Appearance::Dark => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<u32>,
    /// Absolute path of the owner-only agent socket. Written by whoever sets up the
    /// data root; the app only reads it and keeps it on save.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_socket: Option<PathBuf>,
    pub view: ViewMode,
    pub appearance: Appearance,
    /// Interface language: `system | zh | en`; missing or unknown means `system`.
    pub language: LanguagePref,
    pub sidebar_visible: bool,
    pub sidebar_width: f32,
    pub right_width: f32,
    /// Fraction of the right panel used by the item list (rest: metadata).
    pub right_split: f32,
    /// Fraction of the center used by the reader when a conversation exists.
    pub center_split: f32,
    /// The whole AI conversation (transcript, suggestions and composer) is folded
    /// away; only the reader's stats line and a centered AI button remain. Folded
    /// by default (more room to read); a new key so the old unfolded default of the
    /// transcript-only fold does not carry over.
    #[serde(rename = "ai_folded")]
    pub conversation_collapsed: bool,
    pub inspector_width: f32,
    /// Toolbar view switch: folder lists include subfolder items (groups_v1 g2).
    pub include_subfolders: bool,
    /// Reader mode: `original | bilingual | translation`.
    pub reader_mode: ReaderMode,
    /// Book 对照 as side-by-side columns instead of paragraph pairs (default).
    pub book_parallel: bool,
    /// Reading typeface: `sans` (screen, default) | `serif` (print-like).
    pub reading_font: memex_client::ReadingFont,
    pub reading_size: memex_client::ReadingSize,
    /// Typeface of notes (注释): `kai` (default) | `song` | `body`.
    pub note_font: memex_client::NoteFont,
    /// Speakers' names: `original` (default) | `translated` | `both`; display only.
    pub speaker_names: memex_client::SpeakerNames,
    /// Phone-compatible counting rules and speeds; local appearance only.
    pub reading_stats: memex_client::reading_stats::StatsPrefs,
    /// 阅读计时 ⏱: on by default, so the reader times itself while it is in front;
    /// off only when switched off by hand. A new key: the old `pass_timing` (off by
    /// default) does not carry over and is kept untouched on save.
    #[serde(rename = "pass_timing_auto")]
    pub pass_timing: bool,
    /// 古籍 display per item id (简繁, 竖排, typeface); only non-default styles are kept.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub item_styles: std::collections::BTreeMap<String, memex_client::ItemStyle>,
    /// Fields written by a newer app or by setup are kept on save.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            schema_version: None,
            agent_socket: None,
            view: ViewMode::Reading,
            appearance: Appearance::System,
            language: LanguagePref::System,
            sidebar_visible: true,
            sidebar_width: 210.,
            right_width: 340.,
            right_split: 0.5,
            center_split: 0.58,
            conversation_collapsed: true,
            inspector_width: 360.,
            include_subfolders: false,
            reader_mode: ReaderMode::Bilingual,
            book_parallel: false,
            reading_font: memex_client::ReadingFont::Sans,
            reading_size: Default::default(),
            note_font: Default::default(),
            speaker_names: Default::default(),
            reading_stats: Default::default(),
            pass_timing: true,
            item_styles: Default::default(),
            extra: Default::default(),
        }
    }
}

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Locator {
    schema_version: u32,
    settings_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Connection {
    Agent(PathBuf),
    Fixture,
}

/// The resolved startup: preferences, where to save them (None: never save), and
/// what to connect to or why the configuration is unusable.
#[derive(Debug)]
pub struct Startup {
    pub prefs: Prefs,
    pub prefs_path: Option<PathBuf>,
    pub connection: Result<Connection, String>,
}

/// The desktop's own folder beside the data directory, e.g. LotsMemeXDesktopDev.
fn dev_dir(home: &Path) -> PathBuf {
    home.join("Library/Application Support").join(format!("{}DesktopDev", memex_client::DATA_DIR_NAME))
}

pub fn locator_path(home: &Path) -> PathBuf {
    dev_dir(home).join("data-location.json")
}

impl Startup {
    pub fn from_environment() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        Self::resolve(std::env::var_os("MEMEX_AGENT_SOCKET").map(PathBuf::from), std::env::var_os("MEMEX_DESKTOP_STATE").map(PathBuf::from), &home)
    }

    pub fn resolve(env_socket: Option<PathBuf>, env_state: Option<PathBuf>, home: &Path) -> Self {
        let located = match env_state {
            Some(path) => Ok((Prefs::load_lenient(&path), path, false)),
            None => match std::fs::read(locator_path(home)) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    let path = dev_dir(home).join("layout.json");
                    Ok((Prefs::load_lenient(&path), path, false))
                }
                Err(e) => Err(txf("无法读取数据位置指针：{0}", &[&e])),
                Ok(bytes) => read_located(&bytes).map(|(p, path)| (p, path, true)),
            },
        };
        let (prefs, path, required) = match located {
            Ok(v) => v,
            Err(message) => return Self { prefs: Prefs::default(), prefs_path: None, connection: Err(message) },
        };
        let connection = match (env_socket, &prefs.agent_socket) {
            (Some(socket), _) => Ok(Connection::Agent(socket)),
            (None, Some(socket)) if socket.is_absolute() => Ok(Connection::Agent(socket.clone())),
            (None, Some(_)) => Err(tx("设置中的 agent_socket 必须是绝对路径").into()),
            (None, None) if required => Err(tx("设置文件未指定 agent_socket").into()),
            (None, None) => Ok(Connection::Fixture),
        };
        Self { prefs, prefs_path: Some(path), connection }
    }
}

fn read_located(locator: &[u8]) -> Result<(Prefs, PathBuf), String> {
    let l: Locator = serde_json::from_slice(locator).map_err(|_| tx("数据位置指针无法解析").to_string())?;
    if l.schema_version != SCHEMA_VERSION {
        return Err(txf("数据位置指针版本 {0} 不受支持", &[&l.schema_version]));
    }
    if !l.settings_path.is_absolute() {
        return Err(tx("数据位置指针中的 settings_path 必须是绝对路径").into());
    }
    let shown = l.settings_path.display();
    let bytes = std::fs::read(&l.settings_path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => txf("找不到桌面设置文件 {0}", &[&shown]),
        _ => txf("无法读取桌面设置文件 {0}：{1}", &[&shown, &e]),
    })?;
    let prefs: Prefs = serde_json::from_slice(&bytes).map_err(|_| txf("桌面设置文件 {0} 无法解析", &[&shown]))?;
    if prefs.schema_version != Some(SCHEMA_VERSION) {
        return Err(txf("桌面设置文件 {0} 缺少或不支持 schema_version", &[&shown]));
    }
    Ok((prefs.clamped(), l.settings_path))
}

/// The macOS preferred language (`zh*` → Chinese, anything else → English), from
/// CFLocaleCopyPreferredLanguages; Chinese if it cannot be read.
pub fn system_language() -> Lang {
    #[cfg(target_os = "macos")]
    {
        use core_foundation::{array::CFArray, base::TCFType as _, string::CFString};
        // SAFETY: a Create-rule CFArray of CFStrings, owned by the wrapper.
        let first = unsafe {
            let raw = core_foundation_sys::locale::CFLocaleCopyPreferredLanguages();
            if raw.is_null() {
                None
            } else {
                let langs: CFArray<CFString> = CFArray::wrap_under_create_rule(raw);
                langs.get(0).map(|s| s.to_string())
            }
        };
        if let Some(tag) = first {
            return memex_client::i18n::lang_from_tag(&tag);
        }
    }
    Lang::Zh
}

impl Prefs {
    /// Development files: anything unreadable means defaults.
    pub fn load_lenient(path: &Path) -> Self {
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice::<Prefs>(&b).ok()).unwrap_or_default().clamped()
    }

    /// Atomic owner-only write; unknown fields and `agent_socket` are kept.
    pub fn save(&self, path: &Path) {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let Ok(bytes) = serde_json::to_vec_pretty(self) else { return };
        let tmp = path.with_extension("json.tmp");
        let written = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp).and_then(|mut f| f.write_all(&bytes).and_then(|_| f.sync_all()));
        if written.is_ok() {
            let _ = std::fs::rename(&tmp, path);
        } else {
            let _ = std::fs::remove_file(&tmp);
        }
    }

    pub fn clamped(mut self) -> Self {
        let d = Prefs::default();
        let fix = |v: f32, lo: f32, hi: f32, def: f32| if v.is_finite() { v.clamp(lo, hi) } else { def };
        self.sidebar_width = fix(self.sidebar_width, 160., 360., d.sidebar_width);
        self.right_width = fix(self.right_width, 260., 560., d.right_width);
        self.inspector_width = fix(self.inspector_width, 280., 560., d.inspector_width);
        self.right_split = fix(self.right_split, 0.2, 0.8, d.right_split);
        // Wide: the panes' pixel minimums limit the split; this only rejects nonsense.
        self.center_split = fix(self.center_split, 0.05, 0.95, d.center_split);
        self.reading_stats = self.reading_stats.normalized();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reading_stats_defaults_round_trip_and_keep_unrelated_fields() {
        use memex_client::reading_stats::{CountMethod, StatsPrefs};
        let mut p: Prefs = serde_json::from_str(r#"{"future":42,"agent_socket":"/tmp/synthetic.sock"}"#).unwrap();
        assert_eq!(p.reading_stats, StatsPrefs::default());
        assert_eq!(p.reading_size, memex_client::ReadingSize::Medium);
        p.reading_size = memex_client::ReadingSize::ExtraLarge;
        p.reading_stats = StatsPrefs { markup: true, method: CountMethod::Chars, zh: 600, en: 300 };
        let path = temp("reading-stats").join("prefs.json");
        p.save(&path);
        let back = Prefs::load_lenient(&path);
        assert_eq!(back, p);
        assert_eq!(back.extra["future"], 42);
        p.reading_stats.zh = 0;
        p.reading_stats.en = 999;
        assert_eq!(p.clamped().reading_stats, StatsPrefs { markup: true, method: CountMethod::Chars, ..StatsPrefs::default() });
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn conversation_pane_fold_and_share_are_remembered() {
        // Older files have neither field: AI folded, default share. The old
        // transcript-only key does not unfold it.
        let p: Prefs = serde_json::from_str(r#"{"view":"reading","conversation_collapsed":false}"#).unwrap();
        assert!(p.conversation_collapsed);
        assert_eq!(p.center_split, 0.58);
        let saved = Prefs { conversation_collapsed: false, center_split: 0.3, ..Prefs::default() };
        let back: Prefs = serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
        assert!(!back.conversation_collapsed);
        assert_eq!(back.clamped().center_split, 0.3);
        let tiny: Prefs = serde_json::from_str(r#"{"center_split":0.01}"#).unwrap();
        assert_eq!(tiny.clamped().center_split, 0.05);
    }

    #[test]
    fn corrupt_or_extreme_values_are_clamped_and_unknown_fields_ignored() {
        let p: Prefs = serde_json::from_str(r#"{"view":"classic","right_split":9,"sidebar_width":-5,"future":1}"#).unwrap();
        let p = p.clamped();
        assert_eq!(p.view, ViewMode::Classic);
        assert_eq!(p.right_split, 0.8);
        assert_eq!(p.sidebar_width, 160.);
        assert_eq!(p.appearance, Appearance::System);
        assert_eq!(p.language, LanguagePref::System);
        // An unknown language value never breaks reading the file.
        let p: Prefs = serde_json::from_str(r#"{"view":"classic","language":"fr"}"#).unwrap();
        assert_eq!((p.view, p.language), (ViewMode::Classic, LanguagePref::System));
    }

    #[test]
    fn reading_timing_is_on_unless_switched_off_by_hand() {
        // Older files with the old opt-in key stay on; the old key is kept as is.
        let p: Prefs = serde_json::from_str(r#"{"pass_timing":false}"#).unwrap();
        assert!(p.pass_timing);
        assert_eq!(p.extra["pass_timing"], false);
        let off = Prefs { pass_timing: false, ..Prefs::default() };
        let json = serde_json::to_value(&off).unwrap();
        assert_eq!(json["pass_timing_auto"], false);
        assert!(!serde_json::from_value::<Prefs>(json).unwrap().pass_timing);
    }

    #[test]
    fn appearance_follows_system_only_when_asked() {
        assert!(Appearance::System.is_dark(true));
        assert!(!Appearance::Light.is_dark(true));
        assert!(Appearance::Dark.is_dark(false));
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("memex-prefs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_locator(home: &Path, body: &str) {
        let p = locator_path(home);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn nothing_configured_uses_fixture_and_dev_defaults() {
        let home = temp("none");
        let s = Startup::resolve(None, None, &home);
        assert_eq!(s.connection, Ok(Connection::Fixture));
        assert_eq!(s.prefs_path, Some(dev_dir(&home).join("layout.json")));
    }

    #[test]
    fn locator_settings_supply_socket_and_save_keeps_it_and_unknown_fields() {
        let home = temp("ok");
        let settings = home.join("DB/Desktop/settings.json");
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(&settings, r#"{"schema_version":1,"agent_socket":"/tmp/db/agent.sock","note":"setup"}"#).unwrap();
        write_locator(&home, &format!(r#"{{"schema_version":1,"settings_path":"{}"}}"#, settings.display()));
        let s = Startup::resolve(None, None, &home);
        assert_eq!(s.connection, Ok(Connection::Agent("/tmp/db/agent.sock".into())));
        assert_eq!(s.prefs_path.as_deref(), Some(settings.as_path()));
        let mut p = s.prefs;
        p.view = ViewMode::Classic;
        p.save(&settings);
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
        assert_eq!(v["agent_socket"], "/tmp/db/agent.sock");
        assert_eq!(v["schema_version"], 1);
        assert_eq!(v["note"], "setup");
        assert_eq!(v["view"], "classic");
        assert_eq!(v["language"], "system", "a file without the field reads, and saves, as system");
        // The language choice round-trips with the socket and unknown fields intact.
        let mut p: Prefs = serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
        p.language = LanguagePref::En;
        p.save(&settings);
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
        assert_eq!((v["language"].as_str(), v["agent_socket"].as_str(), v["note"].as_str(), v["schema_version"].as_u64()), (Some("en"), Some("/tmp/db/agent.sock"), Some("setup"), Some(1)));
        assert_eq!(Startup::resolve(None, None, &home).prefs.language, LanguagePref::En);
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(std::fs::metadata(&settings).unwrap().permissions().mode() & 0o777, 0o600);
        // An explicit environment socket wins over the settings file.
        let s = Startup::resolve(Some("/tmp/other.sock".into()), None, &home);
        assert_eq!(s.connection, Ok(Connection::Agent("/tmp/other.sock".into())));
    }

    #[test]
    fn a_configured_but_broken_location_is_an_error_never_the_fixture() {
        let home = temp("bad");
        let settings = home.join("DB/Desktop/settings.json");
        let locator = format!(r#"{{"schema_version":1,"settings_path":"{}"}}"#, settings.display());
        let cases: Vec<(&str, Option<&str>)> = vec![
            ("{not json", None),
            (r#"{"schema_version":2,"settings_path":"/x/settings.json"}"#, None),
            (r#"{"schema_version":1,"settings_path":"relative/settings.json"}"#, None),
            (r#"{"schema_version":1,"settings_path":"/x/settings.json","agent_socket":"/x.sock"}"#, None),
            (&locator, None),                                               // settings missing
            (&locator, Some("{broken")),                                    // settings corrupt
            (&locator, Some(r#"{"agent_socket":"/tmp/a.sock"}"#)),         // no schema_version
            (&locator, Some(r#"{"schema_version":1}"#)),                   // no agent_socket
            (&locator, Some(r#"{"schema_version":1,"agent_socket":"a.sock"}"#)), // relative socket
        ];
        for (l, body) in cases {
            write_locator(&home, l);
            let _ = std::fs::remove_file(&settings);
            if let Some(b) = body {
                std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
                std::fs::write(&settings, b).unwrap();
            }
            let s = Startup::resolve(None, None, &home);
            assert!(s.connection.is_err(), "locator {l} / settings {body:?} must be an error, got {:?}", s.connection);
        }
        // An unreadable settings file is not saved over.
        write_locator(&home, &locator);
        std::fs::write(&settings, "{broken").unwrap();
        assert_eq!(Startup::resolve(None, None, &home).prefs_path, None);
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), "{broken");
    }

    #[test]
    fn development_state_env_bypasses_the_locator() {
        let home = temp("env");
        write_locator(&home, "{broken");
        let state = home.join("dev/layout.json");
        let s = Startup::resolve(None, Some(state.clone()), &home);
        assert_eq!(s.connection, Ok(Connection::Fixture));
        assert_eq!(s.prefs_path, Some(state));
    }
}
