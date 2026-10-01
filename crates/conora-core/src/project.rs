//! Declarative themes with shared source assets and fingerprinted firmware bindings.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::app_icons;
pub use crate::app_icons::AppIconAsset;
use crate::crpack::{self, Mapping, PackOptions, QuickappIcon};
use crate::firmware::FirmwareIndex;
use crate::lvgl;

const MAX_CONFIG_BYTES: usize = 1024 * 1024;
/// Maximum native CLI firmware input size; independent of the CRPack output budget.
pub const MAX_FIRMWARE_BYTES: usize = 512 * 1024 * 1024;
/// Bound original templates before inflating or materializing resource bytes.
pub const MAX_TEMPLATE_BYTES: usize = 64 * 1024 * 1024;
const MAX_ASSET_BYTES: usize = 64 * 1024 * 1024;
const MAX_EXCLUSION_REASON_BYTES: usize = 1024;
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
    #[serde(default)]
    pub icons: BTreeMap<String, Asset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canopus_icon: Option<AppIconAsset>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub quickapp_icons: BTreeMap<String, AppIconAsset>,
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
    #[serde(default, skip_serializing_if = "crate::lvgl::ResizeFilter::is_default")]
    pub filter: crate::lvgl::ResizeFilter,
}

impl Asset {
    fn options(&self) -> AssetOptions {
        match self {
            Self::Png(input) => AssetOptions {
                input: input.clone(),
                mode: AssetMode::Png,
                allow_quantize: false,
                filter: crate::lvgl::ResizeFilter::default(),
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
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub excluded: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canopus_icon: Option<AppIconAsset>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub quickapp_icons: BTreeMap<String, AppIconAsset>,
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
    pub filter: crate::lvgl::ResizeFilter,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetReport {
    pub schema_version: u32,
    pub target: String,
    pub firmware_sha256: Option<String>,
    pub valid: bool,
    pub resources: Vec<ResourceSummary>,
    pub excluded: BTreeMap<String, String>,
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
    for package in theme.quickapp_icons.keys() {
        app_icons::validate_package(package)?;
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
        || id.starts_with('.')
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err("target ID must be 1-64 ASCII letters, digits, '_', '-' or '.', and cannot start with '.'".into());
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

struct RoleWork {
    role: String,
    input: PathBuf,
    options: AssetOptions,
    source: Option<Vec<u8>>,
    paths: Vec<String>,
    remaining: usize,
}

pub fn prepare_target(project: &ThemeProject, id: &str) -> PreparedTarget {
    prepare_target_with_progress(project, id, |_| {})
}

/// Prepare a target while reporting human-readable loading/conversion progress.
/// The caller chooses where to display it; core never writes to stdout/stderr.
pub fn prepare_target_with_progress(
    project: &ThemeProject,
    id: &str,
    mut progress: impl FnMut(&str),
) -> PreparedTarget {
    let mut report = TargetReport {
        schema_version: 1,
        target: id.into(),
        firmware_sha256: None,
        valid: false,
        resources: Vec::new(),
        excluded: BTreeMap::new(),
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
    report.excluded = target.excluded.clone();
    let firmware_path = target_firmware_path(project, id, &target);
    progress(&format!("Loading firmware for target {id}"));
    let firmware = match load_firmware(&firmware_path, Some(&target.firmware_sha256)) {
        Ok(firmware) => firmware,
        Err(error) => {
            report.errors.push(Diagnostic::new("firmware", error));
            return PreparedTarget { report, pack: None };
        }
    };
    report.firmware_sha256 = Some(firmware.sha256);
    for role in target
        .bindings
        .keys()
        .chain(target.overrides.keys())
        .chain(target.excluded.keys())
    {
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
    for (role, reason) in &target.excluded {
        if reason.trim().is_empty()
            || reason.len() > MAX_EXCLUSION_REASON_BYTES
            || reason.chars().any(char::is_control)
        {
            report.errors.push(
                Diagnostic::new("exclusion_reason", "exclusion reason must be 1-1024 UTF-8 bytes, nonblank and without control characters")
                    .at(role, None, None),
            );
        }
        if target.bindings.contains_key(role) || target.overrides.contains_key(role) {
            report.errors.push(
                Diagnostic::new(
                    "excluded_conflict",
                    "excluded role cannot have a binding or override",
                )
                .at(role, None, None),
            );
        }
    }
    for role in target.overrides.keys() {
        if !target.bindings.contains_key(role) && !target.excluded.contains_key(role) {
            report.errors.push(
                Diagnostic::new("unused_override", "target override has no binding")
                    .at(role, None, None),
            );
        }
    }
    // Keep source assets under a single aggregate budget, rather than retaining
    // an unbounded asset per role. Each is read once and released after its last
    // resource has been converted (physical order may interleave roles).
    let mut work = Vec::<RoleWork>::new();
    let mut claimed_paths = BTreeSet::new();
    let mut source_bytes = 0usize;
    let mut template_bytes = 0usize;
    let mut template_budget_exceeded = false;
    for (role, shared_asset) in &project.theme.icons {
        if target.excluded.contains_key(role) {
            continue;
        }
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
        progress(&format!("Loading asset for role {role}"));
        let source = if options.input.as_os_str().is_empty() {
            Err("asset input path cannot be empty".into())
        } else {
            read_limited(&input, MAX_ASSET_BYTES.saturating_sub(source_bytes))
        };
        let source = match source {
            Ok(bytes) => {
                source_bytes += bytes.len();
                Some(bytes)
            }
            Err(error) => {
                report.errors.push(Diagnostic::new("asset_input", error).at(
                    role,
                    None,
                    Some(&input),
                ));
                None
            }
        };
        let mut valid_paths = Vec::new();
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
            if options.mode == AssetMode::Png {
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
            }
            if source.is_none() {
                continue;
            }
            if options.mode == AssetMode::Png {
                template_bytes = template_bytes.saturating_add(file.size);
                if template_bytes > MAX_TEMPLATE_BYTES && !template_budget_exceeded {
                    template_budget_exceeded = true;
                    report.errors.push(
                        Diagnostic::new(
                            "template_size",
                            "original templates exceed the aggregate 64 MiB conversion limit",
                        )
                        .at(role, Some(path), Some(&input)),
                    );
                }
            }
            valid_paths.push(path.to_owned());
        }
        if valid_paths.is_empty() {
            source_bytes -= source.as_ref().map_or(0, Vec::len);
        } else {
            let remaining = valid_paths.len();
            work.push(RoleWork {
                role: role.clone(),
                input,
                options,
                source,
                paths: valid_paths,
                remaining,
            });
        }
    }
    let mut replacements = BTreeMap::new();
    let mut total_bytes = 0usize;
    let mut template_paths = Vec::new();
    let mut owners = BTreeMap::new();
    for (index, role) in work.iter_mut().enumerate() {
        if role.options.mode == AssetMode::Raw {
            progress(&format!("Converting role {}", role.role));
            let source = role.source.take().expect("validated source");
            for path in &role.paths {
                // Check before cloning a raw source for multiple resources.
                if source.len() > MAX_PACK_BYTES.saturating_sub(total_bytes) {
                    report.errors.push(
                        Diagnostic::new(
                            "pack_size",
                            "replacement files exceed the 64 MiB CRPack limit",
                        )
                        .at(&role.role, Some(path), Some(&role.input)),
                    );
                    continue;
                }
                record_replacement(
                    &mut report,
                    &mut replacements,
                    &mut total_bytes,
                    role,
                    firmware.index.file(path).expect("validated resource"),
                    source.clone(),
                    false,
                );
            }
        } else {
            for path in &role.paths {
                template_paths.push(path.clone());
                owners.insert(path.clone(), index);
            }
        }
    }
    if !template_budget_exceeded {
        let mut notified = BTreeSet::new();
        let result = firmware.index.visit_file_bytes(
            &template_paths,
            MAX_TEMPLATE_BYTES,
            MAX_TEMPLATE_BYTES,
            |path, template| {
                let role = &mut work[owners[path]];
                if notified.insert(role.role.clone()) {
                    progress(&format!("Converting role {}", role.role));
                }
                match lvgl::encode_png_to_template_with_filter(
                    role.source.as_ref().expect("validated source"),
                    template,
                    role.options.allow_quantize,
                    role.options.filter,
                ) {
                    Ok(encoded) => record_replacement(
                        &mut report,
                        &mut replacements,
                        &mut total_bytes,
                        role,
                        firmware.index.file(path).expect("validated resource"),
                        encoded.bytes,
                        encoded.lossy_quantization,
                    ),
                    Err(error) => {
                        report
                            .errors
                            .push(Diagnostic::new("image_conversion", error).at(
                                &role.role,
                                Some(path),
                                Some(&role.input),
                            ))
                    }
                }
                role.remaining -= 1;
                if role.remaining == 0 {
                    role.source.take();
                }
                Ok(())
            },
        );
        if let Err(error) = result {
            report.errors.push(Diagnostic::new("resource_read", error));
        }
    }
    let mut explicit_mappings = Vec::new();
    let mut quickapp_icons = Vec::new();
    for package in target.quickapp_icons.keys() {
        if !project.theme.quickapp_icons.contains_key(package) {
            report.errors.push(Diagnostic::new(
                "unknown_app_icon",
                format!("target overrides unknown QuickApp {package}"),
            ));
        }
    }
    if target.canopus_icon.is_some() && project.theme.canopus_icon.is_none() {
        report.errors.push(Diagnostic::new(
            "unknown_app_icon",
            "target overrides an undeclared Canopus icon",
        ));
    }
    if let Some(asset) = target
        .canopus_icon
        .as_ref()
        .or(project.theme.canopus_icon.as_ref())
    {
        progress("Converting Canopus application icon");
        prepare_app_icon(
            project,
            asset,
            None,
            &mut report,
            &mut replacements,
            &mut total_bytes,
            &mut source_bytes,
            &mut template_bytes,
            &mut explicit_mappings,
            &mut quickapp_icons,
        );
    }
    for (package, shared) in &project.theme.quickapp_icons {
        progress(&format!("Converting QuickApp icon {package}"));
        prepare_app_icon(
            project,
            target.quickapp_icons.get(package).unwrap_or(shared),
            Some(package),
            &mut report,
            &mut replacements,
            &mut total_bytes,
            &mut source_bytes,
            &mut template_bytes,
            &mut explicit_mappings,
            &mut quickapp_icons,
        );
    }
    // Reports remain deterministic by semantic role/path, independent of ROMFS layout.
    report
        .resources
        .sort_by(|a, b| (&a.role, &a.resource).cmp(&(&b.role, &b.resource)));
    if project.theme.icons.is_empty()
        && project.theme.canopus_icon.is_none()
        && project.theme.quickapp_icons.is_empty()
    {
        report.errors.push(Diagnostic::new(
            "empty_theme",
            "declare firmware source assets or application icons before building",
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
        match crpack::build_crpack_with_icons(&options, &explicit_mappings, &quickapp_icons) {
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

#[allow(clippy::too_many_arguments)]
fn prepare_app_icon(
    project: &ThemeProject,
    asset: &AppIconAsset,
    package: Option<&str>,
    report: &mut TargetReport,
    replacements: &mut BTreeMap<String, Vec<u8>>,
    total_bytes: &mut usize,
    source_bytes: &mut usize,
    template_bytes: &mut usize,
    mappings: &mut Vec<Mapping>,
    icons: &mut Vec<QuickappIcon>,
) {
    let resource = package
        .map(app_icons::source)
        .unwrap_or_else(|| app_icons::CANOPUS_SOURCE.into());
    let role = package
        .map(|p| format!("quickapp:{p}"))
        .unwrap_or_else(|| "canopus".into());
    let destination = package
        .map(app_icons::destination)
        .unwrap_or_else(|| app_icons::CANOPUS_DESTINATION.into());
    let input_path = project.root.join(&asset.input);
    let result = (|| {
        if let Some(package) = package {
            app_icons::validate_package(package)?;
        }
        if replacements.contains_key(&destination) {
            return Err(format!(
                "application icon destination conflicts with firmware replacement: {destination}"
            ));
        }
        if asset.input.as_os_str().is_empty() {
            return Err("application icon input cannot be empty".into());
        }
        if asset.mode == AssetMode::Raw && asset.template.is_some() {
            return Err("raw application icons must not specify a conversion template".into());
        }
        let input = read_limited(&input_path, MAX_ASSET_BYTES.saturating_sub(*source_bytes))?;
        *source_bytes += input.len();
        let template = if let Some(path) = &asset.template {
            if path.as_os_str().is_empty() {
                return Err("application icon template path cannot be empty".into());
            }
            let bytes = read_limited(
                &project.root.join(path),
                MAX_TEMPLATE_BYTES.saturating_sub(*template_bytes),
            )?;
            *template_bytes += bytes.len();
            Some(bytes)
        } else if package.is_none() && asset.mode == AssetMode::Png {
            let bytes = app_icons::canopus_template();
            if bytes.len() > MAX_TEMPLATE_BYTES.saturating_sub(*template_bytes) {
                return Err(
                    "original templates exceed the aggregate 64 MiB conversion limit".into(),
                );
            }
            *template_bytes += bytes.len();
            Some(bytes)
        } else {
            None
        };
        let encoded = app_icons::encode_detailed_with_filter(
            &input,
            template.as_deref(),
            package.is_none(),
            asset.mode == AssetMode::Raw,
            asset.allow_quantize,
            asset.filter,
        )?;
        if encoded.bytes.len() > MAX_PACK_BYTES.saturating_sub(*total_bytes) {
            return Err("replacement files exceed the 64 MiB CRPack limit".into());
        }
        Ok(encoded)
    })();
    let encoded = match result {
        Ok(encoded) => encoded,
        Err(error) => {
            report.errors.push(Diagnostic::new("app_icon", error).at(
                &role,
                Some(&resource),
                Some(&input_path),
            ));
            return;
        }
    };
    let info = lvgl::inspect_image(&encoded.bytes).expect("validated application BIN");
    if package.is_some() {
        report.warnings.push(Diagnostic::new("quickapp_receiver_required",
            "QuickApp icon declarations require an updated Manager/module and a supported exact device target (currently Band 11 .139/.155); app installation and refresh are not verified here")
            .at(&role, Some(&resource), Some(&input_path)));
    } else if asset.mode == AssetMode::Png && asset.template.is_none() {
        report.warnings.push(Diagnostic::new("canopus_preset",
            "using the known Canopus LVGL v9 117x117 template; other layouts require an explicit template")
            .at(&role, Some(&resource), Some(&input_path)));
    }
    if encoded.lossy_quantization {
        report.warnings.push(
            Diagnostic::new(
                "lossy_conversion",
                "application icon conversion reduced color precision or transparency",
            )
            .at(&role, Some(&resource), Some(&input_path)),
        );
    }
    report.resources.push(ResourceSummary {
        role,
        resource: resource.clone(),
        input: input_path,
        mode: asset.mode,
        size_bytes: encoded.bytes.len(),
        format: Some(info.format.display_name().into()),
        width: Some(info.width),
        height: Some(info.height),
        lossy: encoded.lossy_quantization,
        filter: asset.filter,
    });
    *total_bytes += encoded.bytes.len();
    replacements.insert(destination.clone(), encoded.bytes);
    match package {
        Some(package) => icons.push(QuickappIcon {
            package: package.into(),
            destination,
        }),
        None => mappings.push(Mapping {
            source: resource,
            destination,
        }),
    }
}

fn record_replacement(
    report: &mut TargetReport,
    replacements: &mut BTreeMap<String, Vec<u8>>,
    total_bytes: &mut usize,
    work: &RoleWork,
    file: &crate::firmware::ResourceFile,
    bytes: Vec<u8>,
    lossy: bool,
) {
    let role = work.role.as_str();
    let input = work.input.as_path();
    let mode = work.options.mode;
    if mode == AssetMode::Raw {
        report.warnings.push(
            Diagnostic::new(
                "raw_unverified",
                "raw replacement is copied without certifying its device-specific format",
            )
            .at(role, Some(&file.path), Some(input)),
        );
    }
    if lossy {
        report.warnings.push(
            Diagnostic::new(
                "lossy_conversion",
                "conversion reduced color precision or discarded transparency; inspect the result",
            )
            .at(role, Some(&file.path), Some(input)),
        );
    }
    if bytes.len() > MAX_PACK_BYTES.saturating_sub(*total_bytes) {
        report.errors.push(
            Diagnostic::new(
                "pack_size",
                "replacement files exceed the 64 MiB CRPack limit",
            )
            .at(role, Some(&file.path), Some(input)),
        );
        return;
    }
    *total_bytes += bytes.len();
    report.resources.push(ResourceSummary {
        role: role.into(),
        resource: file.path.clone(),
        input: input.to_path_buf(),
        mode,
        size_bytes: bytes.len(),
        format: file.image.map(|info| info.format.display_name().into()),
        width: file.image.map(|info| info.width),
        height: file.image.map(|info| info.height),
        lossy,
        filter: work.options.filter,
    });
    replacements.insert(file.path.clone(), bytes);
}
