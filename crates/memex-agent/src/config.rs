use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    net::Ipv4Addr,
    os::unix::{
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        io::AsRawFd,
    },
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    pub library: PathBuf,
    pub spool: PathBuf,
    pub database: PathBuf,
    pub socket: PathBuf,
    /// Retired local-model block. Accepted and ignored so an old config still starts;
    /// never written. Generation uses only model settings + the saved provider credential.
    #[serde(default, rename = "translation", skip_serializing)]
    pub legacy_translation: Option<serde::de::IgnoredAny>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intake: Option<IntakeConfig>,
    /// Optional owner-configured contact for Crossref's polite pool, sent only
    /// in the User-Agent of Crossref lookups. No default address exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata_lookup_contact: Option<String>,
    /// Chrome used by `SourceJob::render_page`; absent means the standard
    /// macOS install locations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chrome_executable: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntakeConfig {
    pub snapshots: PathBuf,
    pub secret_file: PathBuf,
    pub origin: String,
    pub port: u16,
}
/// Folder under ~/Library/Application Support that holds the library, queue, socket and config.
/// Public builds use LotsMemeX; a private build sets MEMEX_DATA_DIR_NAME when it compiles to keep
/// an existing data directory.
pub const DATA_DIR_NAME: &str = match option_env!("MEMEX_DATA_DIR_NAME") { Some(name) => name, None => "LotsMemeX" };
fn data_dir(home: &Path) -> PathBuf {
    home.join("Library/Application Support").join(DATA_DIR_NAME)
}
impl AgentConfig {
    pub fn read(path: &Path) -> Result<Self> {
        validate_file(path)?;
        Ok(serde_json::from_slice(&fs::read(path)?)?)
    }
    pub fn standard(home: &Path) -> Self {
        let b = data_dir(home);
        Self {
            library: b.join("Library"),
            spool: b.join("spool"),
            database: b.join("agent.db"),
            socket: b.join("agent.sock"),
            legacy_translation: None,
            intake: None,
            metadata_lookup_contact: None,
            chrome_executable: None,
        }
    }
    pub fn standard_config_path(home: &Path) -> PathBuf {
        data_dir(home).join("config.json")
    }
    pub fn validate(&self) -> Result<()> {
        for p in [&self.library, &self.spool, &self.database, &self.socket] {
            if !p.is_absolute() {
                bail!("path_not_absolute")
            }
            validate_chain(p)?
        }
        if self.library == self.spool
            || self.library.starts_with(&self.spool)
            || self.spool.starts_with(&self.library)
            || self.database.starts_with(&self.library)
            || self.database.starts_with(&self.spool)
            || self.socket.starts_with(&self.library)
            || self.socket.starts_with(&self.spool)
            || self.database == self.socket
        {
            bail!("unsafe_path_layout")
        }
        for directory in [&self.library, &self.spool] {
            if let Ok(metadata) = fs::symlink_metadata(directory) {
                if !metadata.is_dir() {
                    bail!("unsafe_path_type")
                }
            }
        }
        if let Ok(metadata) = fs::symlink_metadata(&self.database) {
            if !metadata.is_file() {
                bail!("unsafe_path_type")
            }
        }
        if let Some(contact) = &self.metadata_lookup_contact {
            if !valid_lookup_contact(contact) {
                bail!("invalid_metadata_lookup_contact")
            }
        }
        if let Some(intake) = &self.intake {
            intake.validate()?;
            if intake.snapshots == self.library
                || intake.snapshots.starts_with(&self.library)
                || self.library.starts_with(&intake.snapshots)
                || intake.snapshots == self.spool
                || intake.snapshots.starts_with(&self.spool)
                || self.spool.starts_with(&intake.snapshots)
                || intake.secret_file.starts_with(&self.library)
                || intake.secret_file.starts_with(&self.spool)
                || intake.secret_file.starts_with(&intake.snapshots)
            {
                bail!("unsafe_intake_path_layout")
            }
        }
        Ok(())
    }
}

/// A plain email address `local@domain.tld`; it is placed verbatim in a header.
pub fn valid_lookup_contact(value: &str) -> bool {
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    value.len() <= 254
        && !local.is_empty()
        && local.len() <= 64
        && local
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+'))
        && domain.contains('.')
        && !domain.starts_with(['.', '-'])
        && !domain.ends_with(['.', '-'])
        && !domain.contains("..")
        && domain
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
}

impl IntakeConfig {
    pub fn validate(&self) -> Result<()> {
        if !self.snapshots.is_absolute()
            || !self.secret_file.is_absolute()
            || !(1024..=u16::MAX).contains(&self.port)
        {
            bail!("invalid_intake_config")
        }
        validate_chain(&self.snapshots)?;
        validate_chain(&self.secret_file)?;
        if self.secret_file.exists() {
            validate_file(&self.secret_file)?;
        }
        let valid_id = self
            .origin
            .strip_prefix("chrome-extension://")
            .is_some_and(|host| {
                host.len() == 32 && host.bytes().all(|byte| (b'a'..=b'p').contains(&byte))
            });
        if !valid_id {
            bail!("invalid_intake_origin")
        }
        Ok(())
    }
}
fn validate_chain(path: &Path) -> Result<()> {
    let uid = unsafe { libc::geteuid() };
    let mut owned = false;
    for p in path.ancestors().take_while(|p| !p.as_os_str().is_empty()) {
        if let Ok(m) = fs::symlink_metadata(p) {
            if m.file_type().is_symlink() {
                bail!("unsafe_symlink")
            };
            if m.uid() != uid {
                if owned {
                    break;
                } else {
                    bail!("unsafe_owner")
                }
            }
            owned = true;
            if m.permissions().mode() & 0o022 != 0 {
                bail!("unsafe_mode")
            }
        }
    }
    Ok(())
}
fn validate_file(path: &Path) -> Result<()> {
    validate_chain(path)?;
    let m = fs::metadata(path)?;
    if m.permissions().mode() & 0o077 != 0 {
        bail!("unsafe_mode")
    }
    Ok(())
}
pub fn render_launchd_plist(executable: &Path, config: &Path) -> Result<String> {
    validate_executable(executable)?;
    validate_file(config)?;
    let e = xml(executable.to_str().context("invalid_path")?);
    let c = xml(config.to_str().context("invalid_path")?);
    let label = agent_label();
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>Label</key><string>{label}</string><key>ProgramArguments</key><array><string>{e}</string><string>daemon</string><string>run</string><string>--config</string><string>{c}</string></array><key>RunAtLoad</key><true/><key>KeepAlive</key><true/></dict></plist>
"#
    ))
}
fn validate_executable(path: &Path) -> Result<()> {
    validate_chain(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o022 != 0
        || metadata.permissions().mode() & 0o111 == 0
    {
        bail!("unsafe_executable")
    }
    Ok(())
}
fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
fn mac_only() -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!("platform_not_supported")
    }
    Ok(())
}
/// LaunchAgent label prefix for the agent and surface services. Public builds use com.lotsmemex;
/// a private build sets MEMEX_SERVICE_LABEL_PREFIX when it compiles to keep its installed labels.
const LABEL_PREFIX: &str = match option_env!("MEMEX_SERVICE_LABEL_PREFIX") { Some(prefix) => prefix, None => "com.lotsmemex" };
fn agent_label() -> String {
    format!("{LABEL_PREFIX}.agent")
}
fn surface_label() -> String {
    format!("{LABEL_PREFIX}.surface")
}
fn plist(home: &Path) -> PathBuf {
    home.join(format!("Library/LaunchAgents/{}.plist", agent_label()))
}
fn surface_plist(home: &Path) -> PathBuf {
    home.join(format!("Library/LaunchAgents/{}.plist", surface_label()))
}

pub fn validate_private_lan(address: Ipv4Addr, hostname: &str, port: u16) -> Result<String> {
    if !address.is_private() || address.is_loopback() || port < 1024 {
        bail!("invalid_private_lan")
    }
    let hostname = hostname.trim_end_matches('.').to_ascii_lowercase();
    if hostname.len() > 253
        || !hostname.ends_with(".local")
        || hostname.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
    {
        bail!("invalid_private_lan")
    }
    Ok(hostname)
}

pub fn render_surface_launchd_plist(
    home: &Path,
    executable: &Path,
    address: Ipv4Addr,
    hostname: &str,
    port: u16,
) -> Result<String> {
    validate_executable(executable)?;
    let hostname = validate_private_lan(address, hostname, port)?;
    let library = AgentConfig::standard(home).library;
    let e = xml(executable.to_str().context("invalid_path")?);
    let l = xml(library.to_str().context("invalid_path")?);
    let label = surface_label();
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>Label</key><string>{label}</string><key>ProgramArguments</key><array><string>{e}</string><string>serve</string><string>--library</string><string>{l}</string><string>--max-sensitivity</string><string>internal</string><string>--port</string><string>{port}</string><string>--lan-address</string><string>{address}</string><string>--lan-hostname</string><string>{hostname}</string></array><key>RunAtLoad</key><true/><key>KeepAlive</key><true/><key>ThrottleInterval</key><integer>5</integer></dict></plist>
"#
    ))
}
pub(crate) fn sync_owner_directory(path: &Path) -> Result<()> {
    let directory = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)?;
    if unsafe { libc::fsync(directory.as_raw_fd()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}
pub(crate) fn ensure_owner_directory(path: &Path) -> Result<()> {
    validate_chain(path)?;
    let uid = unsafe { libc::geteuid() };
    let mut missing = Vec::new();
    let mut current = path;
    loop {
        match fs::symlink_metadata(current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink()
                    || !metadata.is_dir()
                    || metadata.uid() != uid
                    || metadata.permissions().mode() & 0o022 != 0
                {
                    bail!("unsafe_directory")
                }
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(current.to_owned());
                current = current.parent().context("unsafe_directory")?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    for directory in missing.into_iter().rev() {
        let parent = directory.parent().context("unsafe_directory")?;
        fs::DirBuilder::new().mode(0o700).create(&directory)?;
        let metadata = fs::symlink_metadata(&directory)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || metadata.uid() != uid
            || metadata.permissions().mode() & 0o077 != 0
        {
            bail!("unsafe_directory")
        }
        sync_owner_directory(parent)?;
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        bail!("unsafe_directory")
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    validate_chain(path)
}
pub fn daemon_install(home: &Path, exe: &Path, config: &Path) -> Result<()> {
    mac_only()?;
    validate_executable(exe)?;
    if !config.exists() {
        if config != AgentConfig::standard_config_path(home) {
            bail!("config_missing")
        }
        let value = AgentConfig::standard(home);
        let base = config.parent().context("invalid_path")?;
        ensure_owner_directory(base)?;
        value.validate()?;
        write_owner_file(config, &serde_json::to_vec_pretty(&value)?)?;
    }
    AgentConfig::read(config)?.validate()?;
    let text = render_launchd_plist(exe, config)?;
    let p = plist(home);
    ensure_owner_directory(p.parent().unwrap())?;
    if let Ok(metadata) = fs::symlink_metadata(&p) {
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            bail!("unsafe_plist")
        }
        let _ = Command::new("launchctl")
            .args([
                "bootout",
                &format!("gui/{}/{}", unsafe { libc::geteuid() }, agent_label()),
            ])
            .status();
    }
    write_owner_file(&p, text.as_bytes())?;
    let ok = Command::new("launchctl")
        .args(["bootstrap", &format!("gui/{}", unsafe { libc::geteuid() })])
        .arg(&p)
        .status()?
        .success();
    if !ok {
        bail!("launchctl_failed")
    }
    Ok(())
}
pub fn daemon_uninstall(home: &Path) -> Result<()> {
    mac_only()?;
    let p = plist(home);
    match fs::symlink_metadata(&p) {
        Ok(metadata)
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.uid() != unsafe { libc::geteuid() } =>
        {
            bail!("unsafe_plist")
        }
        Ok(_) => {
            let _ = Command::new("launchctl")
                .args(["bootout", &format!("gui/{}", unsafe { libc::geteuid() })])
                .arg(&p)
                .status();
            fs::remove_file(p)?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let config = AgentConfig::standard_config_path(home);
    match fs::symlink_metadata(&config) {
        Ok(_) => {
            validate_file(&config)?;
            fs::remove_file(config)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

pub fn surface_install(
    home: &Path,
    executable: &Path,
    address: Ipv4Addr,
    hostname: &str,
    port: u16,
) -> Result<()> {
    mac_only()?;
    let text = render_surface_launchd_plist(home, executable, address, hostname, port)?;
    let path = surface_plist(home);
    ensure_owner_directory(path.parent().unwrap())?;
    if let Ok(metadata) = fs::symlink_metadata(&path) {
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            bail!("unsafe_plist")
        }
        let _ = Command::new("launchctl")
            .args([
                "bootout",
                &format!("gui/{}/{}", unsafe { libc::geteuid() }, surface_label()),
            ])
            .status();
    }
    write_owner_file(&path, text.as_bytes())?;
    if !Command::new("launchctl")
        .args(["bootstrap", &format!("gui/{}", unsafe { libc::geteuid() })])
        .arg(&path)
        .status()?
        .success()
    {
        bail!("launchctl_failed")
    }
    Ok(())
}

pub fn surface_uninstall(home: &Path) -> Result<()> {
    mac_only()?;
    let path = surface_plist(home);
    match fs::symlink_metadata(&path) {
        Ok(metadata)
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.uid() != unsafe { libc::geteuid() } =>
        {
            bail!("unsafe_plist")
        }
        Ok(_) => {
            let _ = Command::new("launchctl")
                .args([
                    "bootout",
                    &format!("gui/{}/{}", unsafe { libc::geteuid() }, surface_label()),
                ])
                .status();
            fs::remove_file(path)?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct DaemonStatus {
    pub installed: bool,
    pub running: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct SurfaceStatus {
    pub installed: bool,
    pub running: bool,
}

pub fn surface_status(home: &Path) -> Result<SurfaceStatus> {
    mac_only()?;
    let installed = match fs::symlink_metadata(surface_plist(home)) {
        Ok(metadata)
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.uid() != unsafe { libc::geteuid() } =>
        {
            bail!("unsafe_plist")
        }
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    let running = Command::new("launchctl")
        .args([
            "print",
            &format!("gui/{}/{}", unsafe { libc::geteuid() }, surface_label()),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    Ok(SurfaceStatus { installed, running })
}

pub fn daemon_status(home: &Path) -> Result<DaemonStatus> {
    mac_only()?;
    let installed = match fs::symlink_metadata(plist(home)) {
        Ok(metadata)
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.uid() != unsafe { libc::geteuid() } =>
        {
            bail!("unsafe_plist")
        }
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    let running =
        std::os::unix::net::UnixStream::connect(AgentConfig::standard(home).socket).is_ok();
    Ok(DaemonStatus { installed, running })
}

fn write_owner_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("invalid_path")?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name().context("invalid_path")?.to_string_lossy(),
        uuid::Uuid::now_v7().simple()
    ));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plist_is_minimal_and_platform_fails() {
        let d = tempfile::tempdir().unwrap();
        let c = d.path().join("c.json");
        fs::write(&c, "{}").unwrap();
        fs::set_permissions(&c, fs::Permissions::from_mode(0o600)).unwrap();
        let e = d.path().join("memex");
        fs::write(&e, "").unwrap();
        fs::set_permissions(&e, fs::Permissions::from_mode(0o700)).unwrap();
        let p = render_launchd_plist(&e, &c).unwrap();
        assert!(
            p.contains("daemon</string><string>run")
                && !p.contains("EnvironmentVariables")
                && !p.contains("Amp")
                && !p.contains("LaunchDaemon")
        );
        let surface = render_surface_launchd_plist(
            d.path(),
            &e,
            Ipv4Addr::new(192, 168, 1, 20),
            "Owner-Mac.local.",
            43191,
        )
        .unwrap();
        assert!(surface.contains(&format!("<string>{}</string>", surface_label())));
        assert!(surface.contains("owner-mac.local"));
        assert!(surface.contains("--lan-address"));
        assert!(surface.contains("--lan-hostname"));
        assert!(!surface.contains("EnvironmentVariables"));
        for address in [
            Ipv4Addr::LOCALHOST,
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::new(8, 8, 8, 8),
        ] {
            assert!(validate_private_lan(address, "owner.local", 43191).is_err());
        }
        let standard = AgentConfig::standard(d.path());
        standard.validate().unwrap();
        // A retired `translation` block (any shape) is accepted, ignored and never written back.
        let legacy: AgentConfig = serde_json::from_value(serde_json::json!({
            "library": standard.library,
            "spool": standard.spool,
            "database": standard.database,
            "socket": standard.socket,
            "translation": {"origin": "http://127.0.0.1:1/", "model_id": "retired", "extra": [1]},
        }))
        .unwrap();
        assert!(legacy.legacy_translation.is_some());
        legacy.validate().unwrap();
        assert!(serde_json::to_value(&legacy)
            .unwrap()
            .get("translation")
            .is_none());
        let compatible: AgentConfig = serde_json::from_value(serde_json::json!({
            "library": standard.library,
            "spool": standard.spool,
            "database": standard.database,
            "socket": standard.socket,
        }))
        .unwrap();
        assert!(compatible.legacy_translation.is_none());
        assert!(compatible.intake.is_none());
        let snapshots = d.path().join("intake-snapshots");
        ensure_owner_directory(&snapshots).unwrap();
        let secret = d.path().join("intake-secret");
        write_owner_file(
            &secret,
            b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .unwrap();
        let mut intake = compatible.clone();
        intake.intake = Some(IntakeConfig {
            snapshots,
            secret_file: secret,
            origin: "chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            port: 43871,
        });
        intake.validate().unwrap();
        for origin in [
            "https://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/",
            "chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/",
            "chrome-extension://qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq/",
            "chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/",
            "chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/path",
        ] {
            intake.intake.as_mut().unwrap().origin = origin.into();
            assert_eq!(
                intake.validate().unwrap_err().to_string(),
                "invalid_intake_origin"
            );
        }
        let nested = d.path().join("durable/owner/chain");
        ensure_owner_directory(&nested).unwrap();
        for directory in [
            d.path().join("durable"),
            d.path().join("durable/owner"),
            nested,
        ] {
            assert_eq!(
                fs::metadata(directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        let mut overlapping = compatible;
        overlapping.spool = overlapping.library.join("spool");
        assert_eq!(
            overlapping.validate().unwrap_err().to_string(),
            "unsafe_path_layout"
        );
        if !cfg!(target_os = "macos") {
            assert_eq!(
                daemon_install(d.path(), &e, &c).unwrap_err().to_string(),
                "platform_not_supported"
            )
        }
    }
}
