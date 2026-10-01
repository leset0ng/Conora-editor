//! Declarative themes with shared source assets and fingerprinted firmware bindings.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::crpack::{self, PackOptions};
use crate::firmware::FirmwareIndex;
use crate::lvgl;

const MAX_CONFIG_BYTES: usize = 1024 * 1024;
/// Maximum native CLI firmware input size; independent of the CRPack output budget.
pub const MAX_FIRMWARE_BYTES: usize = 512 * 1024 * 1024;
/// Bound original templates before inflating or materializing resource bytes.
pub const MAX_TEMPLATE_BYTES: usize = 64 * 1024 * 1024;
const MAX_ASSET_BYTES: usize = 64 * 1024 * 1024;
const MAX_PACK_BYTES: usize = 64 * 1024 * 1024;

fn schema_version() -> u32 {
    1
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Theme {
    #[serde(default = "schema_version")]
    pub schema_version: u32,
    pub theme_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub icons: BTreeMap<String, Asset>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Asset {
    Png(PathBuf),
    Detailed(AssetOptions),
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AssetMode {
    #[default]
    Png,
    Raw,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssetOptions {
    pub input: PathBuf,
    #[serde(default)]
    pub mode: AssetMode,
    #[serde(default)]
    pub allow_quantize: bool,
}

impl Asset {
    fn options(&self) -> AssetOptions {
        match self {
            Self::Png(input) => AssetOptions {
                input: input.clone(),
                mode: AssetMode::Png,
                allow_quantize: false,
            },
            Self::Detailed(options) => options.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Target {
    #[serde(default = "schema_version")]
    pub schema_version: u32,
    pub firmware: PathBuf,
    pub firmware_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    pub bindings: BTreeMap<String, Binding>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub overrides: BTreeMap<String, Asset>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Binding {
    One(String),
    Many(Vec<String>),
}

impl Binding {
    fn resources(&self) -> Vec<&str> {
        match self {
            Self::One(path) => vec![path],
            Self::Many(paths) => paths.iter().map(String::as_str).collect(),
        }
    }
}

#[derive(Debug)]
pub struct ThemeProject {
    pub root: PathBuf,
    pub theme: Theme,
}

pub struct LoadedFirmware {
    pub index: FirmwareIndex,
    pub sha256: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceSummary {
    pub role: String,
    pub resource: String,
    pub input: PathBuf,
    pub mode: AssetMode,
    pub size_bytes: usize,
    pub format: Option<String>,
    pub width: Option<u16>,
    pub height: Option<u16>,
    pub lossy: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetReport {
    pub schema_version: u32,
    pub target: String,
    pub firmware_sha256: Option<String>,
    pub valid: bool,
    pub resources: Vec<ResourceSummary>,
    pub errors: Vec<Diagnostic>,
    pub warnings: Vec<Diagnostic>,
    pub pack_bytes: usize,
}

pub struct PreparedTarget {
    pub report: TargetReport,
    pub pack: Option<Vec<u8>>,
}

impl Diagnostic {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            role: None,
            resource: None,
            input: None,
        }
    }

    fn at(mut self, role: &str, resource: Option<&str>, input: Option<&Path>) -> Self {
        self.role = Some(role.into());
        self.resource = resource.map(str::to_owned);
        self.input = input.map(Path::to_path_buf);
        self
    }
}

pub fn read_limited(path: &Path, max_bytes: usize) -> Result<Vec<u8>, String> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("could not inspect {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("not a regular file: {}", path.display()));
    }
    if metadata.len() > max_bytes as u64 {
        return Err(format!(
            "{} exceeds the {max_bytes}-byte input limit",
            path.display()
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    // Avoid blocking if a regular file is replaced with a FIFO after the preflight.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .map_err(|error| format!("could not open {}: {error}", path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("could not inspect {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("not a regular file: {}", path.display()));
    }
    if metadata.len() > max_bytes as u64 {
        return Err(format!(
            "{} exceeds the {max_bytes}-byte input limit",
            path.display()
        ));
    }
    let mut bytes = Vec::new();
    file.take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    if bytes.len() > max_bytes {
        return Err(format!(
            "{} exceeds the {max_bytes}-byte input limit",
            path.display()
        ));
    }
    Ok(bytes)
}

fn read_config<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let bytes = read_limited(path, MAX_CONFIG_BYTES)?;
    serde_json::from_slice(&bytes).map_err(|error| format!("invalid {}: {error}", path.display()))
}

pub fn load_theme(path: &Path) -> Result<ThemeProject, String> {
    let path = if path.is_dir() {
        path.join("theme.json")
    } else {
        path.to_path_buf()
    };
    let path = path
        .canonicalize()
        .map_err(|error| format!("could not resolve {}: {error}", path.display()))?;
    let theme: Theme = read_config(&path)?;
    if theme.schema_version != 1 {
        return Err(format!(
            "unsupported theme schemaVersion: {}",
            theme.schema_version
        ));
    }
    crpack::validate_pack_metadata(
        &theme.theme_id,
        &theme.name,
        theme.version.as_deref(),
        theme.author.as_deref(),
        theme.description.as_deref(),
        None,
    )?;
    for role in theme.icons.keys() {
        validate_role(role)?;
    }
    Ok(ThemeProject {
        root: path.parent().unwrap().to_path_buf(),
        theme,
    })
}

fn validate_role(role: &str) -> Result<(), String> {
    if role.is_empty() || role.len() > 128 || role.chars().any(char::is_control) {
        return Err(format!(
            "icon role must be 1-128 UTF-8 bytes without control characters: {role:?}"
        ));
    }
    Ok(())
}

pub fn validate_target_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err("target ID must be 1-64 ASCII letters, digits, '_' or '-'".into());
    }
    Ok(())
}

pub fn target_ids(project: &ThemeProject) -> Result<Vec<String>, String> {
    let directory = project.root.join("targets");
    let mut ids = Vec::new();
    for entry in std::fs::read_dir(&directory)
        .map_err(|error| format!("could not list {}: {error}", directory.display()))?
    {
        let entry = entry.map_err(|error| format!("could not read target entry: {error}"))?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or("target filename is not valid UTF-8")?;
        validate_target_id(id)?;
        ids.push(id.into());
    }
    ids.sort();
    if ids.is_empty() {
        return Err("no target configs found; use 'conora target add' first".into());
    }
    Ok(ids)
}

pub fn load_target(project: &ThemeProject, id: &str) -> Result<Target, String> {
    validate_target_id(id)?;
    let target: Target = read_config(&project.root.join("targets").join(format!("{id}.json")))?;
    if target.schema_version != 1 {
        return Err(format!(
            "unsupported target schemaVersion: {}",
            target.schema_version
        ));
    }
    if target.firmware.as_os_str().is_empty() {
        return Err("target firmware path cannot be empty".into());
    }
    if target.firmware_sha256.len() != 64
        || !target
            .firmware_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("firmwareSha256 must be 64 lowercase hexadecimal characters; use 'conora target add' to pin a firmware".into());
    }
    crpack::validate_pack_metadata(
        &project.theme.theme_id,
        &project.theme.name,
        project.theme.version.as_deref(),
        project.theme.author.as_deref(),
        project.theme.description.as_deref(),
        target.device.as_deref(),
    )?;
    Ok(target)
}

pub fn target_firmware_path(project: &ThemeProject, _id: &str, target: &Target) -> PathBuf {
    project.root.join("targets").join(&target.firmware)
}

pub fn load_firmware(path: &Path, expected_sha256: Option<&str>) -> Result<LoadedFirmware, String> {
    let bytes = read_limited(path, MAX_FIRMWARE_BYTES)?;
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    if let Some(expected) = expected_sha256
        && expected != sha256
    {
        return Err(format!(
            "firmware SHA-256 mismatch for {}: expected {expected}, found {sha256}; verify the firmware and explicitly re-pin the target",
            path.display()
        ));
    }
    let index = FirmwareIndex::from_firmware(bytes)?;
    Ok(LoadedFirmware { index, sha256 })
}

pub fn prepare_target(project: &ThemeProject, id: &str) -> PreparedTarget {
    let mut report = TargetReport {
        schema_version: 1,
        target: id.into(),
        firmware_sha256: None,
        valid: false,
        resources: Vec::new(),
        errors: Vec::new(),
        warnings: Vec::new(),
        pack_bytes: 0,
    };
    let target = match load_target(project, id) {
        Ok(target) => target,
        Err(error) => {
            report.errors.push(Diagnostic::new("target_config", error));
            return PreparedTarget { report, pack: None };
        }
    };
    let firmware_path = target_firmware_path(project, id, &target);
    let firmware = match load_firmware(&firmware_path, Some(&target.firmware_sha256)) {
        Ok(firmware) => firmware,
        Err(error) => {
            report.errors.push(Diagnostic::new("firmware", error));
            return PreparedTarget { report, pack: None };
        }
    };
    report.firmware_sha256 = Some(firmware.sha256);
    for role in target.bindings.keys().chain(target.overrides.keys()) {
        if !project.theme.icons.contains_key(role) {
            report.errors.push(
                Diagnostic::new(
                    "unknown_role",
                    "target references a role absent from theme.icons",
                )
                .at(role, None, None),
            );
        }
    }
    for role in target.overrides.keys() {
        if !target.bindings.contains_key(role) {
            report.errors.push(
                Diagnostic::new("unused_override", "target override has no binding")
                    .at(role, None, None),
            );
        }
    }
    let mut claimed_paths = BTreeSet::new();
    let mut replacements = BTreeMap::new();
    let mut total_bytes = 0usize;
    for (role, shared_asset) in &project.theme.icons {
        let Some(binding) = target.bindings.get(role) else {
            report.errors.push(
                Diagnostic::new("missing_binding", "icon role has no binding in this target")
                    .at(role, None, None),
            );
            continue;
        };
        let paths = binding.resources();
        if paths.is_empty() {
            report.errors.push(
                Diagnostic::new(
                    "empty_binding",
                    "binding must contain at least one resource path",
                )
                .at(role, None, None),
            );
            continue;
        }
        let options = target.overrides.get(role).unwrap_or(shared_asset).options();
        let input = project.root.join(&options.input);
        let source = if options.input.as_os_str().is_empty() {
            Err("asset input path cannot be empty".into())
        } else {
            read_limited(&input, MAX_ASSET_BYTES)
        };
        if let Err(error) = &source {
            report
                .errors
                .push(Diagnostic::new("asset_input", error.clone()).at(role, None, Some(&input)));
        }
        for path in paths {
            if let Err(error) = crpack::validate_relative_path(path) {
                report
                    .errors
                    .push(Diagnostic::new("resource_path", error).at(
                        role,
                        Some(path),
                        Some(&input),
                    ));
                continue;
            }
            if !claimed_paths.insert(path.to_owned()) {
                report.errors.push(
                    Diagnostic::new(
                        "duplicate_binding",
                        "resource is bound more than once; each resource must have one owner",
                    )
                    .at(role, Some(path), Some(&input)),
                );
                continue;
            }
            let Some(file) = firmware.index.file(path) else {
                report.errors.push(
                    Diagnostic::new(
                        "missing_resource",
                        "resource path does not exist in the pinned firmware",
                    )
                    .at(role, Some(path), Some(&input)),
                );
                continue;
            };
            let Ok(source) = &source else {
                continue;
            };
            let (bytes, lossy) = match options.mode {
                AssetMode::Raw => (source.clone(), false),
                AssetMode::Png => {
                    if file.image.is_none() {
                        report.errors.push(
                            Diagnostic::new(
                                "unsupported_template",
                                "original resource is not a supported image template",
                            )
                            .at(role, Some(path), Some(&input)),
                        );
                        continue;
                    }
                    if file.size > MAX_TEMPLATE_BYTES {
                        report.errors.push(
                            Diagnostic::new(
                                "template_size",
                                "original template exceeds the 64 MiB conversion limit",
                            )
                            .at(role, Some(path), Some(&input)),
                        );
                        continue;
                    }
                    let template = match firmware.index.file_bytes(path) {
                        Ok(Some(template)) => template,
                        Ok(None) => unreachable!("resource metadata already exists"),
                        Err(error) => {
                            report
                                .errors
                                .push(Diagnostic::new("resource_read", error).at(
                                    role,
                                    Some(path),
                                    Some(&input),
                                ));
                            continue;
                        }
                    };
                    match lvgl::encode_png_to_template_detailed(
                        source,
                        &template,
                        options.allow_quantize,
                    ) {
                        Ok(encoded) => (encoded.bytes, encoded.lossy_quantization),
                        Err(error) => {
                            report
                                .errors
                                .push(Diagnostic::new("image_conversion", error).at(
                                    role,
                                    Some(path),
                                    Some(&input),
                                ));
                            continue;
                        }
                    }
                }
            };
            if options.mode == AssetMode::Raw {
                report.warnings.push(
                    Diagnostic::new(
                        "raw_unverified",
                        "raw replacement is copied without certifying its device-specific format",
                    )
                    .at(role, Some(path), Some(&input)),
                );
            }
            if lossy {
                report.warnings.push(Diagnostic::new("lossy_conversion", "conversion reduced color precision or discarded transparency; inspect the result").at(role, Some(path), Some(&input)));
            }
            if bytes.len() > MAX_PACK_BYTES.saturating_sub(total_bytes) {
                report.errors.push(
                    Diagnostic::new(
                        "pack_size",
                        "replacement files exceed the 64 MiB CRPack limit",
                    )
                    .at(role, Some(path), Some(&input)),
                );
                continue;
            }
            total_bytes += bytes.len();
            report.resources.push(ResourceSummary {
                role: role.clone(),
                resource: path.into(),
                input: input.clone(),
                mode: options.mode,
                size_bytes: bytes.len(),
                format: file.image.map(|info| info.format.display_name().into()),
                width: file.image.map(|info| info.width),
                height: file.image.map(|info| info.height),
                lossy,
            });
            replacements.insert(path.into(), bytes);
        }
    }
    if project.theme.icons.is_empty() {
        report.errors.push(Diagnostic::new(
            "empty_theme",
            "theme.icons is empty; declare source assets and bind them to firmware resources",
        ));
    }
    let pack = if report.errors.is_empty() {
        let options = PackOptions {
            theme_id: &project.theme.theme_id,
            name: &project.theme.name,
            version: project.theme.version.as_deref(),
            author: project.theme.author.as_deref(),
            description: project.theme.description.as_deref(),
            target: target.device.as_deref(),
            replacements: &replacements,
        };
        match crpack::build_crpack(&options) {
            Ok(bytes) => {
                report.valid = true;
                report.pack_bytes = bytes.len();
                Some(bytes)
            }
            Err(error) => {
                report
                    .errors
                    .push(Diagnostic::new("pack_validation", error));
                None
            }
        }
    } else {
        None
    };
    PreparedTarget { report, pack }
}
