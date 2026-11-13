use anyhow::{bail, Result};
use fs2::FileExt;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

fn open_registry_lock(path: &Path) -> Result<fs::File> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() || !meta.is_file() {
                bail!("unsafe registry lock")
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut options = fs::OpenOptions::new();
            options.create_new(true).read(true).write(true);
            #[cfg(unix)]
            {
                options.custom_flags(libc::O_NOFOLLOW);
            }
            match options.open(path) {
                Ok(file) => return Ok(file),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e.into()),
            }
            let meta = fs::symlink_metadata(path)?;
            if meta.file_type().is_symlink() || !meta.is_file() {
                bail!("unsafe registry lock")
            }
        }
        Err(e) => return Err(e.into()),
    }
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        bail!("unsafe registry lock")
    }
    Ok(file)
}
use memex_core::{Sensitivity, SourceProfile, SCHEMA_VERSION};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub fn validate_source_id(id: &str) -> Result<()> {
    if id.len() != "legacy_source_".len() + 64
        || !id.starts_with("legacy_source_")
        || !id["legacy_source_".len()..]
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        bail!("unsafe source_id");
    }
    Ok(())
}
pub fn revision_id_for(
    source_id: &str,
    input_sha256: &str,
    revision_number: u32,
) -> Result<String> {
    validate_source_id(source_id)?;
    if revision_number == 0
        || input_sha256.len() != 7 + 64
        || !input_sha256.starts_with("sha256:")
        || !input_sha256[7..]
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        bail!("invalid revision identity input");
    }
    Ok(format!(
        "registry_revision_{:x}",
        Sha256::digest(format!("{source_id}\0{input_sha256}\0{revision_number}").as_bytes())
    ))
}
fn validate_record(record: &Record, expected: &str) -> Result<()> {
    validate_source_id(expected)?;
    let valid_text =
        |v: &str, max: usize| !v.is_empty() && v.len() <= max && !v.chars().any(char::is_control);
    let profile = &record.source_profile;
    let url_ok = profile.canonical_uris.len() == 1
        && profile.canonical_uris.iter().all(|u| {
            valid_text(u, 4096) && {
                url::Url::parse(u).is_ok_and(|u| {
                    matches!(u.scheme(), "http" | "https")
                        && u.username().is_empty()
                        && u.password().is_none()
                })
            }
        });
    let vectors_ok = |v: &Vec<String>| {
        v.iter().all(|x| valid_text(x, 256))
            && v.iter().collect::<std::collections::BTreeSet<_>>().len() == v.len()
    };
    if record.schema_version != SCHEMA_VERSION
        || record.source_id != expected
        || !valid_text(&record.external_source_id, 256)
        || looks_absolute_filesystem_path(&record.external_source_id)
        || legacy_source_id(&record.external_source_id) != expected
        || record.source_profile.source_id != expected
        || profile.schema_version != SCHEMA_VERSION
        || profile.source_kind != "legacy_registry_profile"
        || profile.default_sensitivity != Sensitivity::Internal
        || profile.preferred_extractor.is_some()
        || !valid_text(&profile.name, 1024)
        || looks_absolute_filesystem_path(&profile.name)
        || !url_ok
        || profile.evidence_roles.is_empty()
        || profile.evidence_roles.len() > 64
        || !vectors_ok(&profile.evidence_roles)
        || record.governance.admission_state != AdmissionState::ImportedUnreviewed
        || !valid_text(&record.governance.category, 256)
        || !valid_text(&record.governance.region, 256)
        || !valid_text(&record.governance.language, 256)
        || !valid_text(&record.governance.access_type, 256)
        || !valid_text(&record.governance.capture_mode, 256)
        || record.governance.topic_tags.len() > 64
        || !vectors_ok(&record.governance.topic_tags)
        || record.provenance.adapter_id != "intelligence-source-registry.legacy-profile.v0"
        || record.provenance.adapter_version != "0"
        || !record.provenance.input_sha256.starts_with("sha256:")
        || record.provenance.input_sha256.len() != 71
        || record.provenance.mapped_fields.len() > 10
        || record.provenance.unmapped_fields.len() > 4096
        || record.provenance.defaulted_fields.len() > 2
        || record.provenance.warning_codes.len() > 1
        || !vectors_ok(&record.provenance.mapped_fields)
        || !vectors_ok(&record.provenance.unmapped_fields)
        || !vectors_ok(&record.provenance.defaulted_fields)
        || !vectors_ok(&record.provenance.warning_codes)
        || record.revision_number < 1
        || record.revision_id
            != revision_id_for(
                expected,
                &record.provenance.input_sha256,
                record.revision_number,
            )?
    {
        bail!("invalid registry record")
    }
    Ok(())
}
fn safe_dir(path: &Path) -> Result<()> {
    if let Ok(m) = fs::symlink_metadata(path) {
        if m.file_type().is_symlink() || !m.is_dir() {
            bail!("unsafe registry directory")
        }
    }
    Ok(())
}
#[cfg(target_os = "macos")]
fn trusted_platform_alias(path: &Path, meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    let expected = if path == Path::new("/var") {
        "private/var"
    } else if path == Path::new("/tmp") {
        "private/tmp"
    } else if path == Path::new("/etc") {
        "private/etc"
    } else {
        return false;
    };
    meta.uid() == 0
        && fs::read_link(path)
            .map(|t| t == Path::new(expected))
            .unwrap_or(false)
}
#[cfg(not(target_os = "macos"))]
fn trusted_platform_alias(_: &Path, _: &fs::Metadata) -> bool {
    false
}
fn validate_root_ancestors(root: &Path) -> Result<()> {
    if root
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        bail!("unsafe package root")
    }
    let absolute = std::path::absolute(root)?;
    for path in absolute.ancestors().skip(1) {
        let meta = fs::symlink_metadata(path)?;
        if meta.file_type().is_symlink() {
            if !trusted_platform_alias(path, &meta) {
                bail!("unsafe package root ancestor")
            }
        } else if !meta.is_dir() {
            bail!("unsafe package root ancestor")
        }
    }
    Ok(())
}
fn safe_file(path: &Path) -> Result<()> {
    let m = fs::symlink_metadata(path)?;
    if m.file_type().is_symlink() || !m.is_file() {
        bail!("unsafe registry file");
    }
    Ok(())
}
fn atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| anyhow::anyhow!("no parent"))?;
    safe_dir(parent)?;
    if path.exists() {
        safe_file(path)?;
    }
    let mut f = tempfile::NamedTempFile::new_in(parent)?;
    use std::io::Write;
    f.write_all(bytes)?;
    f.persist(path).map_err(|e| e.error)?;
    Ok(())
}
fn atomic_noclobber(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| anyhow::anyhow!("no parent"))?;
    safe_dir(parent)?;
    if path.exists() {
        bail!("target already exists")
    }
    let mut f = tempfile::NamedTempFile::new_in(parent)?;
    use std::io::Write;
    f.write_all(bytes)?;
    f.persist_noclobber(path).map_err(|e| e.error)?;
    Ok(())
}
pub fn record_markdown(r: &Record) -> String {
    format!(
        "# {}\n\n- source_id: {}\n- revision: {}\n- admission: imported_unreviewed\n\n{}\n",
        r.source_profile.name,
        r.source_id,
        r.revision_number,
        r.source_profile.canonical_uris.join("\n")
    )
}
pub fn registry_markdown(records: &[Record]) -> String {
    let mut records = records.to_vec();
    records.sort_by(|a, b| a.source_id.cmp(&b.source_id));
    records.iter().map(record_markdown).collect()
}
fn looks_absolute_filesystem_path(s: &str) -> bool {
    Path::new(s).is_absolute()
        || s.starts_with("file://")
        || (s.len() > 2
            && s.as_bytes()[1] == b':'
            && s.as_bytes()[0].is_ascii_alphabetic()
            && matches!(s.as_bytes()[2], b'/' | b'\\'))
}
fn checked_text(value: &Value, name: &str, max: usize, allow_absolute: bool) -> Result<String> {
    let s = value
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("{name} must be string"))?
        .trim();
    if s.is_empty()
        || s.len() > max
        || s.chars().any(char::is_control)
        || (!allow_absolute && looks_absolute_filesystem_path(s))
    {
        bail!("invalid {name}")
    }
    Ok(s.into())
}
fn required_text(
    map: &serde_json::Map<String, Value>,
    key: &str,
    max: usize,
    allow_absolute: bool,
) -> Result<String> {
    checked_text(
        map.get(key)
            .ok_or_else(|| anyhow::anyhow!("missing {key}"))?,
        key,
        max,
        allow_absolute,
    )
}
fn checked_string_list(
    value: &Value,
    name: &str,
    allow_scalar: bool,
    require_nonempty: bool,
    max_items: usize,
) -> Result<Vec<String>> {
    let values = match value {
        Value::String(_) if allow_scalar => vec![value],
        Value::Array(a) => a.iter().collect(),
        _ => bail!("invalid {name}"),
    };
    if values.len() > max_items || (require_nonempty && values.is_empty()) {
        bail!("invalid {name}")
    }
    let mut out = Vec::new();
    for v in values {
        let s = checked_text(v, name, 256, false)?;
        if out.contains(&s) {
            bail!("duplicate {name}")
        }
        out.push(s)
    }
    Ok(out)
}
fn contains_absolute_filesystem_value(value: &Value) -> bool {
    match value {
        Value::String(s) => looks_absolute_filesystem_path(s),
        Value::Array(a) => a.iter().any(contains_absolute_filesystem_value),
        Value::Object(o) => o.values().any(contains_absolute_filesystem_value),
        _ => false,
    }
}
pub fn resolve_profile_file(canonical_root: &Path, relative: &Path) -> Result<PathBuf> {
    if relative.to_str().is_none() || relative.is_absolute() {
        bail!("unsafe profile path");
    }
    let parts: Vec<_> = relative.components().collect();
    if parts.is_empty()
        || parts
            .iter()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        bail!("unsafe profile path");
    }
    let mut current = canonical_root.to_path_buf();
    for (index, part) in parts.iter().enumerate() {
        let std::path::Component::Normal(part) = part else {
            unreachable!()
        };
        current.push(part);
        let meta = fs::symlink_metadata(&current)?;
        if meta.file_type().is_symlink()
            || (index + 1 < parts.len() && !meta.is_dir())
            || (index + 1 == parts.len() && !meta.is_file())
        {
            bail!("unsafe profile path");
        }
    }
    let final_path = current.canonicalize()?;
    if !final_path.starts_with(canonical_root) {
        bail!("profile path escapes root");
    }
    Ok(final_path)
}

/// Return the canonical MemeX source identity for the current legacy IA lineage.
///
/// Phase 2 used this exact format for normalized captures. Keeping the helper in
/// the registry module makes source identity a registry-owned contract instead
/// of an adapter implementation detail.
pub fn legacy_source_id(external_source_id: &str) -> String {
    format!(
        "legacy_source_{:x}",
        Sha256::digest(external_source_id.as_bytes())
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionState {
    ImportedUnreviewed,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryGovernance {
    pub admission_state: AdmissionState,
    pub category: String,
    pub topic_tags: Vec<String>,
    pub region: String,
    pub language: String,
    pub access_type: String,
    pub capture_mode: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryProvenance {
    pub adapter_id: String,
    pub adapter_version: String,
    pub input_sha256: String,
    pub mapped_fields: Vec<String>,
    pub unmapped_fields: Vec<String>,
    pub defaulted_fields: Vec<String>,
    pub warning_codes: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub schema_version: u32,
    pub source_id: String,
    pub revision_id: String,
    pub revision_number: u32,
    pub external_source_id: String,
    pub source_profile: SourceProfile,
    pub governance: RegistryGovernance,
    pub provenance: RegistryProvenance,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyStatus {
    Created,
    Revised,
    Replayed,
}
impl ApplyStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Revised => "revised",
            Self::Replayed => "replayed",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyReceipt {
    pub record: Record,
    pub status: ApplyStatus,
    pub warning_codes: Vec<String>,
}
pub struct Prepared {
    pub relative_path: String,
    pub digest: String,
    pub record: Record,
    pub unmapped: Vec<String>,
    pub defaults: Vec<String>,
    pub warnings: Vec<String>,
}
fn hash(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}
fn credentials(v: &Value) -> bool {
    const K: &[&str] = &[
        "password",
        "passwd",
        "secret",
        "token",
        "access_token",
        "refresh_token",
        "authorization",
        "cookie",
        "cookies",
        "set-cookie",
    ];
    match v {
        Value::Object(m) => m
            .iter()
            .any(|(k, v)| K.iter().any(|x| k.eq_ignore_ascii_case(x)) || credentials(v)),
        Value::Array(a) => a.iter().any(credentials),
        _ => false,
    }
}
pub fn preflight(root: &Path, paths: &[PathBuf]) -> Result<Vec<Prepared>> {
    validate_root_ancestors(root)?;
    if paths.is_empty() || paths.len() > 1000 {
        bail!("profile count invalid");
    }
    let root_meta = fs::symlink_metadata(root)?;
    if root_meta.file_type().is_symlink() || !root_meta.is_dir() {
        bail!("unsafe package root")
    }
    let root = root.canonicalize()?;
    let mut ids = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for p in paths {
        let full = resolve_profile_file(&root, p)?;
        let m = fs::symlink_metadata(&full)?;
        if m.file_type().is_symlink() || m.len() > 1024 * 1024 {
            bail!("unsafe or oversize profile")
        }
        let b = fs::read(&full)?;
        if b.len() > 1024 * 1024 {
            bail!("oversize profile")
        }
        let v: Value = serde_json::from_slice(&b)?;
        if credentials(&v) {
            bail!("credential-like key")
        }
        let o = v
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("profile object required"))?;
        let external = required_text(o, "source_id", 256, false)?;
        if !ids.insert(external.clone()) {
            bail!("duplicate source_id")
        }
        let name = required_text(o, "name", 1024, false)?;
        let url = required_text(o, "url", 4096, true)?;
        let parsed = url::Url::parse(&url)?;
        if !matches!(parsed.scheme(), "http" | "https")
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            bail!("invalid url")
        }
        let category = required_text(o, "category", 256, false)?;
        let region = required_text(o, "region", 256, false)?;
        let language = required_text(o, "language", 256, false)?;
        let access_type = required_text(o, "access_type", 256, false)?;
        let capture_mode = required_text(o, "capture_mode", 256, false)?;
        let mut defaults = Vec::new();
        let roles = match o.get("evidence_role") {
            Some(v) => checked_string_list(v, "evidence_role", true, true, 64)?,
            None => {
                defaults.push("evidence_role:legacy_unclassified".into());
                vec!["legacy_unclassified".into()]
            }
        };
        let tags = match o.get("topic_tags") {
            Some(v) => checked_string_list(v, "topic_tags", false, false, 64)?,
            None => {
                defaults.push("topic_tags:[]".into());
                vec![]
            }
        };
        let mut unmapped: Vec<String> = o
            .keys()
            .filter(|k| {
                ![
                    "source_id",
                    "name",
                    "url",
                    "evidence_role",
                    "category",
                    "topic_tags",
                    "region",
                    "language",
                    "access_type",
                    "capture_mode",
                ]
                .contains(&k.as_str())
            })
            .cloned()
            .collect();
        unmapped.sort();
        let warnings = if unmapped
            .iter()
            .any(|key| contains_absolute_filesystem_value(&o[key]))
        {
            vec!["absolute_path_value_omitted".into()]
        } else {
            vec![]
        };
        let digest = hash(&b);
        let sid = legacy_source_id(&external);
        let profile = SourceProfile {
            schema_version: SCHEMA_VERSION,
            source_id: sid.clone(),
            name,
            source_kind: "legacy_registry_profile".into(),
            canonical_uris: vec![url],
            evidence_roles: roles,
            default_sensitivity: Sensitivity::Internal,
            preferred_extractor: None,
        };
        let mut mapped = vec![
            "source_id",
            "name",
            "url",
            "category",
            "region",
            "language",
            "access_type",
            "capture_mode",
        ];
        if o.contains_key("topic_tags") {
            mapped.push("topic_tags");
        }
        if o.contains_key("evidence_role") {
            mapped.push("evidence_role");
        }
        let mapped = mapped.into_iter().map(str::to_string).collect();
        let gov = RegistryGovernance {
            admission_state: AdmissionState::ImportedUnreviewed,
            category,
            topic_tags: tags,
            region,
            language,
            access_type,
            capture_mode,
        };
        let provenance = RegistryProvenance {
            adapter_id: "intelligence-source-registry.legacy-profile.v0".into(),
            adapter_version: "0".into(),
            input_sha256: digest.clone(),
            mapped_fields: mapped,
            unmapped_fields: unmapped.clone(),
            defaulted_fields: defaults.clone(),
            warning_codes: warnings.clone(),
        };
        out.push(Prepared {
            relative_path: p.to_str().unwrap().to_owned(),
            digest: digest.clone(),
            record: Record {
                schema_version: SCHEMA_VERSION,
                source_id: sid,
                revision_id: String::new(),
                revision_number: 0,
                external_source_id: external,
                source_profile: profile,
                governance: gov,
                provenance,
            },
            unmapped,
            defaults,
            warnings,
        });
    }
    Ok(out)
}
fn refresh_projections(library: &Path, record: &Record) -> Vec<String> {
    let registry = library.join("registry");
    let source = registry.join("sources").join(&record.source_id);
    let mut warnings = Vec::new();
    if atomic(&source.join("index.md"), record_markdown(record).as_bytes()).is_err() {
        warnings.push("source_index_projection_pending".into());
    }
    let global = match list_current(library) {
        Ok(mut records) => {
            records.sort_by(|a, b| a.source_id.cmp(&b.source_id));
            atomic(
                &registry.join("index.md"),
                registry_markdown(&records).as_bytes(),
            )
        }
        Err(_) => Err(anyhow::anyhow!("global projection")),
    };
    if global.is_err() {
        warnings.push("global_index_projection_pending".into());
    }
    warnings
}
pub fn apply(library: &Path, mut p: Prepared) -> Result<ApplyReceipt> {
    validate_source_id(&p.record.source_id)?;
    validate_root_ancestors(library)?;
    fs::create_dir_all(library)?;
    safe_dir(library)?;
    let registry = library.join("registry");
    fs::create_dir_all(&registry)?;
    safe_dir(&registry)?;
    let lock = open_registry_lock(&registry.join(".lock"))?;
    lock.lock_exclusive()?;
    let sources = registry.join("sources");
    fs::create_dir_all(&sources)?;
    safe_dir(&sources)?;
    let dir = sources.join(&p.record.source_id);
    fs::create_dir_all(&dir)?;
    safe_dir(&dir)?;
    let current = dir.join("current.json");
    match fs::symlink_metadata(&current) {
        Ok(meta) => {
            if meta.file_type().is_symlink() || !meta.is_file() {
                bail!("unsafe registry file");
            }
            let old = read_verified_current(library, &p.record.source_id)?.0;
            if old.provenance.input_sha256 == p.digest {
                let warnings = refresh_projections(library, &old);
                return Ok(ApplyReceipt {
                    record: old,
                    status: ApplyStatus::Replayed,
                    warning_codes: warnings,
                });
            }
            p.record.revision_number = old.revision_number + 1;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => p.record.revision_number = 1,
        Err(e) => return Err(e.into()),
    }
    p.record.revision_id =
        revision_id_for(&p.record.source_id, &p.digest, p.record.revision_number)?;
    let revisions = dir.join("revisions");
    fs::create_dir_all(&revisions)?;
    safe_dir(&revisions)?;
    let body = serde_json::to_vec_pretty(&p.record)?;
    let revision = revisions.join(format!("{}.json", p.record.revision_id));
    if revision.exists() {
        safe_file(&revision)?;
        if fs::read(&revision)? != body {
            bail!("immutable revision conflict");
        }
    } else {
        atomic_noclobber(&revision, &body)?;
    }
    atomic(&current, &body)?;
    let status = if p.record.revision_number == 1 {
        ApplyStatus::Created
    } else {
        ApplyStatus::Revised
    };
    let warnings = refresh_projections(library, &p.record);
    Ok(ApplyReceipt {
        record: p.record,
        status,
        warning_codes: warnings,
    })
}
fn read_verified_current(library: &Path, id: &str) -> Result<(Record, Vec<u8>)> {
    validate_root_ancestors(library)?;
    safe_dir(library)?;
    validate_source_id(id)?;
    let registry = library.join("registry");
    safe_dir(&registry)?;
    let sources = registry.join("sources");
    safe_dir(&sources)?;
    let source = sources.join(id);
    safe_dir(&source)?;
    let current = source.join("current.json");
    safe_file(&current)?;
    let current_bytes = fs::read(current)?;
    let record: Record = serde_json::from_slice(&current_bytes)?;
    validate_record(&record, id)?;
    let revisions = source.join("revisions");
    safe_dir(&revisions)?;
    let revision = revisions.join(format!("{}.json", record.revision_id));
    safe_file(&revision)?;
    if fs::read(revision)? != current_bytes {
        bail!("current/revision mismatch");
    }
    Ok((record, current_bytes))
}
pub fn read_current(library: &Path, id: &str) -> Result<Record> {
    Ok(read_verified_current(library, id)?.0)
}
pub fn read_current_optional(library: &Path, id: &str) -> Result<Option<Record>> {
    if validate_source_id(id).is_err() {
        return Ok(None);
    }
    validate_root_ancestors(library)?;
    match fs::symlink_metadata(library) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(meta) if !meta.file_type().is_symlink() && meta.is_dir() => {}
        Ok(_) => bail!("unsafe library root"),
        Err(e) => return Err(e.into()),
    }
    let registry = library.join("registry");
    match fs::symlink_metadata(&registry) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(meta) if !meta.file_type().is_symlink() && meta.is_dir() => {}
        Ok(_) => bail!("unsafe registry directory"),
        Err(e) => return Err(e.into()),
    }
    let sources = registry.join("sources");
    match fs::symlink_metadata(&sources) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(meta) if !meta.file_type().is_symlink() && meta.is_dir() => {}
        Ok(_) => bail!("unsafe registry directory"),
        Err(e) => return Err(e.into()),
    }
    let source = sources.join(id);
    match fs::symlink_metadata(&source) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Ok(meta) if !meta.file_type().is_symlink() && meta.is_dir() => {
            read_current(library, id).map(Some)
        }
        Ok(_) => bail!("unsafe registry entry"),
        Err(e) => Err(e.into()),
    }
}
pub fn list_current(library: &Path) -> Result<Vec<Record>> {
    validate_root_ancestors(library)?;
    match fs::symlink_metadata(library) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => bail!("unsafe library root"),
        Ok(_) => {}
        Err(e) => return Err(e.into()),
    }
    let root = library.join("registry/sources");
    match fs::symlink_metadata(&root) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
            bail!("unsafe registry directory")
        }
        Ok(_) => {}
        Err(e) => return Err(e.into()),
    }
    safe_dir(&root)?;
    let mut out = Vec::new();
    for e in fs::read_dir(root)? {
        let e = e?;
        if e.file_type()?.is_symlink() || !e.file_type()?.is_dir() {
            bail!("unsafe registry entry")
        }
        let id = e.file_name().to_string_lossy().into_owned();
        validate_source_id(&id)?;
        out.push(read_current(library, &id)?)
    }
    out.sort_by(|a, b| a.source_id.cmp(&b.source_id));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const PROFILE: &str = r#"{"source_id":"fixture.source","name":"Synthetic Source","url":"https://example.invalid/source","category":"test","region":"global","language":"en","access_type":"public","capture_mode":"manual"}"#;
    fn prepared(root: &Path) -> Prepared {
        fs::create_dir_all(root).unwrap();
        if !root.join("source.json").exists() {
            fs::write(root.join("source.json"), PROFILE).unwrap();
        }
        preflight(root, &[PathBuf::from("source.json")])
            .unwrap()
            .pop()
            .unwrap()
    }

    #[test]
    fn legacy_identity_is_stable_and_namespaced() {
        assert_eq!(
            legacy_source_id("fixture.source"),
            "legacy_source_eee092319386a85ef13764b46272a685bbbdf382798740297cead0fb66d2db0c"
        );
        assert_eq!(
            legacy_source_id("fixture.source"),
            legacy_source_id("fixture.source")
        );
        assert_ne!(
            legacy_source_id("fixture.source"),
            legacy_source_id("fixture.other")
        );
    }

    #[test]
    fn apply_creates_replays_revises_and_renders_indexes() {
        let temp = tempfile::tempdir().unwrap();
        let package = temp.path().join("package");
        let library = temp.path().join("library");
        let first = prepared(&package);
        let id = first.record.source_id.clone();
        assert_eq!(apply(&library, first).unwrap().status, ApplyStatus::Created);
        assert_eq!(
            apply(&library, prepared(&package)).unwrap().status,
            ApplyStatus::Replayed
        );
        fs::write(
            package.join("source.json"),
            PROFILE.replace(r#""category":"test""#, r#""category":"changed""#),
        )
        .unwrap();
        let receipt = apply(&library, prepared(&package)).unwrap();
        assert_eq!(receipt.status, ApplyStatus::Revised);
        let r = receipt.record;
        assert_eq!(r.revision_number, 2);
        let source = library.join("registry/sources").join(id);
        assert_eq!(fs::read_dir(source.join("revisions")).unwrap().count(), 2);
        assert!(fs::read_to_string(source.join("index.md"))
            .unwrap()
            .contains("Synthetic Source"));
        assert!(fs::read_to_string(library.join("registry/index.md"))
            .unwrap()
            .contains("Synthetic Source"));
    }
    #[test]
    fn returning_to_historical_digest_creates_third_revision() {
        let temp = tempfile::tempdir().unwrap();
        let package = temp.path().join("p");
        let library = temp.path().join("l");
        let original = PROFILE.to_string();
        let s1 = apply(&library, prepared(&package)).unwrap();
        assert_eq!(s1.status, ApplyStatus::Created);
        let r1 = read_current(&library, &legacy_source_id("fixture.source")).unwrap();
        fs::write(
            package.join("source.json"),
            original.replace(r#""category":"test""#, r#""category":"changed""#),
        )
        .unwrap();
        let r2 = apply(&library, prepared(&package)).unwrap();
        assert_eq!(r2.status, ApplyStatus::Revised);
        let r2 = r2.record;
        fs::write(package.join("source.json"), original).unwrap();
        let r3 = apply(&library, prepared(&package)).unwrap();
        assert_eq!(r3.status, ApplyStatus::Revised);
        let r3 = r3.record;
        assert_eq!(
            (r1.revision_number, r2.revision_number, r3.revision_number),
            (1, 2, 3)
        );
        assert_eq!(r1.provenance.input_sha256, r3.provenance.input_sha256);
        assert_ne!(r1.revision_id, r3.revision_id);
        assert_eq!(
            fs::read_dir(
                library
                    .join("registry/sources")
                    .join(&r1.source_id)
                    .join("revisions")
            )
            .unwrap()
            .count(),
            3
        );
        let replay = apply(&library, prepared(&package)).unwrap();
        assert_eq!(replay.status, ApplyStatus::Replayed);
        assert_eq!(replay.record, r3);
    }
    fn applied_paths() -> (tempfile::TempDir, PathBuf, PathBuf, Record) {
        let temp = tempfile::tempdir().unwrap();
        let package = temp.path().join("p");
        let library = temp.path().join("l");
        let record = apply(&library, prepared(&package)).unwrap().record;
        (temp, package, library, record)
    }
    #[test]
    fn current_byte_tamper_rejects_read_list_and_replay() {
        let (_t, p, l, r) = applied_paths();
        let current = l
            .join("registry/sources")
            .join(&r.source_id)
            .join("current.json");
        let mut v: Value = serde_json::from_slice(&fs::read(&current).unwrap()).unwrap();
        v["source_profile"]["name"] = Value::String("changed".into());
        fs::write(&current, serde_json::to_vec(&v).unwrap()).unwrap();
        assert!(read_current(&l, &r.source_id).is_err());
        assert!(list_current(&l).is_err());
        assert!(apply(&l, prepared(&p)).is_err());
    }
    #[test]
    fn revision_byte_tamper_rejects_read_and_replay() {
        let (_t, p, l, r) = applied_paths();
        let revision = l
            .join("registry/sources")
            .join(&r.source_id)
            .join("revisions")
            .join(format!("{}.json", r.revision_id));
        let mut v: Value = serde_json::from_slice(&fs::read(&revision).unwrap()).unwrap();
        v["source_profile"]["name"] = Value::String("changed".into());
        fs::write(&revision, serde_json::to_vec(&v).unwrap()).unwrap();
        assert!(read_current(&l, &r.source_id).is_err());
        assert!(apply(&l, prepared(&p)).is_err());
    }
    #[test]
    fn missing_current_revision_rejects_read_list_and_replay() {
        let (_t, p, l, r) = applied_paths();
        let revision = l
            .join("registry/sources")
            .join(&r.source_id)
            .join("revisions")
            .join(format!("{}.json", r.revision_id));
        fs::remove_file(revision).unwrap();
        assert!(read_current(&l, &r.source_id).is_err());
        assert!(list_current(&l).is_err());
        assert!(apply(&l, prepared(&p)).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn source_index_failure_after_current_returns_revised_warning() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let package = temp.path().join("p");
        let library = temp.path().join("l");
        let first = apply(&library, prepared(&package)).unwrap();
        let sentinel = temp.path().join("sentinel");
        fs::write(&sentinel, "outside").unwrap();
        let index = library
            .join("registry/sources")
            .join(&first.record.source_id)
            .join("index.md");
        fs::remove_file(&index).unwrap();
        symlink(&sentinel, &index).unwrap();
        fs::write(
            package.join("source.json"),
            PROFILE.replace(r#""category":"test""#, r#""category":"changed""#),
        )
        .unwrap();
        let receipt = apply(&library, prepared(&package)).unwrap();
        assert_eq!(receipt.status, ApplyStatus::Revised);
        assert_eq!(
            receipt.warning_codes,
            vec!["source_index_projection_pending"]
        );
        assert_eq!(receipt.record.revision_number, 2);
        assert_eq!(
            read_current(&library, &first.record.source_id)
                .unwrap()
                .revision_number,
            2
        );
        assert_eq!(fs::read(&sentinel).unwrap(), b"outside");
        assert!(fs::read_to_string(library.join("registry/index.md"))
            .unwrap()
            .contains("revision: 2"));
    }
    #[cfg(unix)]
    #[test]
    fn global_index_failure_after_current_returns_revised_warning() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let package = temp.path().join("p");
        let library = temp.path().join("l");
        let first = apply(&library, prepared(&package)).unwrap();
        let sentinel = temp.path().join("sentinel");
        fs::write(&sentinel, "outside").unwrap();
        let index = library.join("registry/index.md");
        fs::remove_file(&index).unwrap();
        symlink(&sentinel, &index).unwrap();
        fs::write(
            package.join("source.json"),
            PROFILE.replace(r#""category":"test""#, r#""category":"changed""#),
        )
        .unwrap();
        let receipt = apply(&library, prepared(&package)).unwrap();
        assert_eq!(receipt.status, ApplyStatus::Revised);
        assert_eq!(
            receipt.warning_codes,
            vec!["global_index_projection_pending"]
        );
        assert_eq!(
            read_current(&library, &first.record.source_id)
                .unwrap()
                .revision_number,
            2
        );
        assert_eq!(fs::read(&sentinel).unwrap(), b"outside");
        assert!(fs::read_to_string(
            library
                .join("registry/sources")
                .join(&first.record.source_id)
                .join("index.md")
        )
        .unwrap()
        .contains("revision: 2"));
    }
    #[test]
    fn replay_repairs_missing_markdown_indexes() {
        let temp = tempfile::tempdir().unwrap();
        let package = temp.path().join("p");
        let library = temp.path().join("l");
        let first = apply(&library, prepared(&package)).unwrap();
        let source = library
            .join("registry/sources")
            .join(&first.record.source_id);
        fs::remove_file(source.join("index.md")).unwrap();
        fs::remove_file(library.join("registry/index.md")).unwrap();
        let replay = apply(&library, prepared(&package)).unwrap();
        assert_eq!(replay.status, ApplyStatus::Replayed);
        assert!(replay.warning_codes.is_empty());
        let expected = record_markdown(&replay.record);
        assert_eq!(
            fs::read_to_string(source.join("index.md")).unwrap(),
            expected
        );
        assert!(fs::read_to_string(library.join("registry/index.md"))
            .unwrap()
            .contains(&replay.record.source_id));
    }
    #[test]
    fn validate_record_rejects_structural_tampering() {
        let temp = tempfile::tempdir().unwrap();
        let package = temp.path().join("p");
        let library = temp.path().join("l");
        let record = apply(&library, prepared(&package)).unwrap().record;
        assert!(validate_record(&record, &record.source_id).is_ok());
        let mut uri = record.clone();
        uri.source_profile.canonical_uris.clear();
        assert!(validate_record(&uri, &record.source_id).is_err());
        let mut external = record.clone();
        external.external_source_id = "bad\nid".into();
        assert!(validate_record(&external, &record.source_id).is_err());
        let mut warnings = record.clone();
        warnings.provenance.warning_codes = vec!["one".into(), "two".into()];
        assert!(validate_record(&warnings, &record.source_id).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn preflight_rejects_package_root_symlink() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        let alias = temp.path().join("alias");
        fs::create_dir_all(&real).unwrap();
        fs::write(real.join("source.json"), PROFILE).unwrap();
        symlink(&real, &alias).unwrap();
        assert!(preflight(&alias, &[PathBuf::from("source.json")]).is_err());
        assert_eq!(
            fs::read(real.join("source.json")).unwrap(),
            PROFILE.as_bytes()
        );
    }
    #[cfg(unix)]
    #[test]
    fn preflight_rejects_ancestor_symlink_escape() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("real-parent");
        let package = parent.join("package");
        let link = temp.path().join("link");
        fs::create_dir_all(&package).unwrap();
        fs::write(package.join("source.json"), PROFILE).unwrap();
        symlink(&parent, &link).unwrap();
        assert!(preflight(&link.join("package"), &[PathBuf::from("source.json")]).is_err());
        assert_eq!(
            fs::read(package.join("source.json")).unwrap(),
            PROFILE.as_bytes()
        );
    }
    #[cfg(unix)]
    #[test]
    fn apply_rejects_output_library_ancestor_symlink_without_side_effect() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let package = temp.path().join("p");
        let outside = temp.path().join("outside");
        let link = temp.path().join("link");
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, &link).unwrap();
        assert!(apply(&link.join("jump"), prepared(&package)).is_err());
        assert!(!outside.join("jump").exists());
    }
    #[cfg(unix)]
    #[test]
    fn query_rejects_library_root_and_ancestor_symlink_aliases() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let package = temp.path().join("p");
        let parent = temp.path().join("parent");
        let library = parent.join("library");
        fs::create_dir_all(&parent).unwrap();
        let receipt = apply(&library, prepared(&package)).unwrap();
        let id = receipt.record.source_id;
        let root_alias = temp.path().join("root-alias");
        symlink(&library, &root_alias).unwrap();
        assert!(read_current(&root_alias, &id).is_err());
        assert!(list_current(&root_alias).is_err());
        let parent_alias = temp.path().join("parent-alias");
        symlink(&parent, &parent_alias).unwrap();
        let alias = parent_alias.join("library");
        assert!(read_current(&alias, &id).is_err());
        assert!(list_current(&alias).is_err());
        assert!(read_current(&library, &id).is_ok());
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_var_alias_is_trusted_when_present() {
        let path = Path::new("/var");
        if let Ok(meta) = fs::symlink_metadata(path) {
            if meta.file_type().is_symlink() {
                assert!(trusted_platform_alias(path, &meta));
            }
        }
    }
    #[test]
    fn read_rejects_unsafe_source_ids() {
        let temp = tempfile::tempdir().unwrap();
        assert!(read_current(temp.path(), "../escape").is_err());
        assert!(read_current(temp.path(), "legacy_source_ABCDEF").is_err());
    }
    #[cfg(unix)]
    #[test]
    fn registry_symlinks_fail_without_touching_outside() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let package = temp.path().join("package");
        let library = temp.path().join("library");
        let outside = temp.path().join("outside");
        fs::write(&outside, "sentinel").unwrap();
        fs::create_dir_all(library.join("registry")).unwrap();
        symlink(&outside, library.join("registry/.lock")).unwrap();
        assert!(apply(&library, prepared(&package)).is_err());
        assert_eq!(fs::read(&outside).unwrap(), b"sentinel");
    }
    #[test]
    fn tampered_revision_is_never_overwritten() {
        let temp = tempfile::tempdir().unwrap();
        let package = temp.path().join("package");
        let library = temp.path().join("library");
        let p = prepared(&package);
        let revision_id = revision_id_for(&p.record.source_id, &p.digest, 1).unwrap();
        let revision = library
            .join("registry/sources")
            .join(&p.record.source_id)
            .join("revisions")
            .join(format!("{}.json", revision_id));
        fs::create_dir_all(revision.parent().unwrap()).unwrap();
        fs::write(&revision, "tampered").unwrap();
        let error = match apply(&library, p) {
            Ok(_) => panic!("tampered revision was accepted"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("immutable revision conflict"));
        assert_eq!(fs::read(&revision).unwrap(), b"tampered");
        assert!(!revision
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("current.json")
            .exists());
    }
    #[test]
    fn unknown_absolute_values_warn_without_persisting_values() {
        let temp = tempfile::tempdir().unwrap();
        let package = temp.path().join("package");
        fs::create_dir_all(&package).unwrap();
        fs::write(
            package.join("source.json"),
            PROFILE.replace(
                "}",
                r#","notes":"PRIVATE_NOTE","nested":{"path":"/Users/SYNTHETIC_PRIVATE/x"}}"#,
            ),
        )
        .unwrap();
        let p = preflight(&package, &[PathBuf::from("source.json")])
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(p.warnings, vec!["absolute_path_value_omitted"]);
        assert_eq!(p.warnings, p.record.provenance.warning_codes);
        assert!(p.unmapped.contains(&"notes".into()));
        let encoded = serde_json::to_string(&p.record).unwrap();
        assert!(!encoded.contains("PRIVATE_NOTE"));
        assert!(!encoded.contains("/Users/SYNTHETIC_PRIVATE/x"));
    }
    #[test]
    fn preflight_rejects_unsafe_paths_credentials_and_urls() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir_all(&root).unwrap();
        assert!(preflight(&root, &[PathBuf::from("../x")]).is_err());
        assert!(preflight(&root, &[temp.path().join("x")]).is_err());
        for body in [
            PROFILE.replace("}", r#","nested":{"ToKeN":"x"}}"#),
            PROFILE.replace("https://example.invalid/source", "ftp://example.invalid"),
            PROFILE.replace(
                "https://example.invalid/source",
                "https://u:p@example.invalid",
            ),
        ] {
            fs::write(root.join("source.json"), body).unwrap();
            assert!(preflight(&root, &[PathBuf::from("source.json")]).is_err());
        }
    }
    #[test]
    fn preflight_rejects_invalid_lists_and_mapped_strings() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir_all(&root).unwrap();
        for body in [
            PROFILE.replace("}", r#","evidence_role":[]}"#),
            PROFILE.replace("}", r#","topic_tags":"bad"}"#),
            PROFILE.replace(r#""category":"test""#, r#""category":"/Users/x""#),
            PROFILE.replace(
                r#""name":"Synthetic Source""#,
                &format!(r#""name":"{}""#, "x".repeat(1025)),
            ),
        ] {
            fs::write(root.join("source.json"), body).unwrap();
            assert!(preflight(&root, &[PathBuf::from("source.json")]).is_err());
        }
    }
    #[test]
    fn preflight_enforces_file_size_and_profile_count() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir_all(&root).unwrap();
        let mut exact = PROFILE.as_bytes().to_vec();
        exact.resize(1024 * 1024, b' ');
        fs::write(root.join("source.json"), &exact).unwrap();
        assert!(preflight(&root, &[PathBuf::from("source.json")]).is_ok());
        exact.push(b' ');
        fs::write(root.join("source.json"), exact).unwrap();
        assert!(preflight(&root, &[PathBuf::from("source.json")]).is_err());
        let paths = (0..1001)
            .map(|i| PathBuf::from(format!("{i}.json")))
            .collect::<Vec<_>>();
        assert!(preflight(&root, &paths).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn preflight_rejects_parent_symlink_escape() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("source.json"), PROFILE).unwrap();
        symlink(&outside, root.join("link")).unwrap();
        assert!(preflight(&root, &[PathBuf::from("link/source.json")]).is_err());
        assert_eq!(
            fs::read(outside.join("source.json")).unwrap(),
            PROFILE.as_bytes()
        );
    }
    #[test]
    fn preflight_accepts_1000_unique_and_rejects_duplicate_identity() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir_all(&root).unwrap();
        let mut paths = Vec::new();
        for i in 0..1000 {
            let p = PathBuf::from(format!("{i}.json"));
            fs::write(
                root.join(&p),
                PROFILE.replace("fixture.source", &format!("fixture.{i}")),
            )
            .unwrap();
            paths.push(p);
        }
        assert_eq!(preflight(&root, &paths).unwrap().len(), 1000);
        let other = temp.path().join("other");
        fs::create_dir_all(&other).unwrap();
        fs::write(other.join("a.json"), PROFILE).unwrap();
        fs::write(other.join("b.json"), PROFILE).unwrap();
        assert!(preflight(&other, &[PathBuf::from("a.json"), PathBuf::from("b.json")]).is_err());
    }
    #[test]
    fn empty_list_does_not_create_library() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("missing");
        assert!(list_current(&root).unwrap().is_empty());
        assert!(!root.exists());
    }
    #[test]
    fn tampered_current_identity_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let package = temp.path().join("p");
        let library = temp.path().join("l");
        let p = prepared(&package);
        let id = p.record.source_id.clone();
        apply(&library, p).unwrap();
        let path = library
            .join("registry/sources")
            .join(&id)
            .join("current.json");
        let mut v: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        v["source_id"] = Value::String("legacy_source_bad".into());
        fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
        assert!(read_current(&library, &id).is_err());
        assert!(list_current(&library).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn current_symlink_is_not_replaced() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let package = temp.path().join("p");
        let library = temp.path().join("l");
        let p = prepared(&package);
        let id = p.record.source_id.clone();
        apply(&library, p).unwrap();
        let outside = temp.path().join("outside");
        fs::write(&outside, "sentinel").unwrap();
        let current = library
            .join("registry/sources")
            .join(id)
            .join("current.json");
        fs::remove_file(&current).unwrap();
        symlink(&outside, &current).unwrap();
        fs::write(
            package.join("source.json"),
            PROFILE.replace(r#""category":"test""#, r#""category":"changed""#),
        )
        .unwrap();
        assert!(apply(&library, prepared(&package)).is_err());
        assert_eq!(fs::read(&outside).unwrap(), b"sentinel");
    }
    #[cfg(unix)]
    #[test]
    fn dangling_lock_symlink_never_creates_outside_target() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let package = temp.path().join("p");
        let library = temp.path().join("l");
        let outside = temp.path().join("outside-lock");
        fs::create_dir_all(library.join("registry")).unwrap();
        symlink(&outside, library.join("registry/.lock")).unwrap();
        assert!(apply(&library, prepared(&package)).is_err());
        assert!(!outside.exists());
        assert!(fs::symlink_metadata(library.join("registry/.lock"))
            .unwrap()
            .file_type()
            .is_symlink());
    }
}
