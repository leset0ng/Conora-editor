use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read, Write};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::app_icons;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

const MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024;
const MAX_MANIFEST_BYTES: usize = 64 * 1024;
const MAX_MAPPINGS: usize = 256;
const MAX_CONFIG_BYTES: usize = 32 * 1024;
const MAX_PATH_BYTES: usize = 256;
const THEME_ROOT: &str = "/data/quickapp/files/ng.lst.corona/themes/";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnpackedCrpack {
    pub theme_id: String,
    pub name: String,
    pub version: Option<String>,
    pub author: Option<String>,
    pub description: Option<String>,
    pub target: Option<String>,
    pub replacements: BTreeMap<String, Vec<u8>>,
    /// Original ordered rules; replacement filenames do not imply firmware source paths.
    pub mappings: Vec<Mapping>,
    /// Optional application declarations, separate from firmware mappings.
    pub quickapp_icons: Vec<QuickappIcon>,
    pub targets: Vec<String>,
    /// Original manifest, including unknown fields, for verbatim CLI extraction/transfer.
    pub manifest_bytes: Vec<u8>,
}

impl UnpackedCrpack {
    /// All ordinary files to extract or transfer, including the root manifest.
    /// Transport-specific file count and chunk limits are deliberately not archive limits.
    pub fn files(&self) -> impl Iterator<Item = (&str, &[u8])> {
        std::iter::once(("canora.json", self.manifest_bytes.as_slice())).chain(
            self.replacements
                .iter()
                .map(|(path, bytes)| (path.as_str(), bytes.as_slice())),
        )
    }
}

#[derive(Clone, Debug)]
pub struct PackOptions<'a> {
    pub theme_id: &'a str,
    pub name: &'a str,
    pub version: Option<&'a str>,
    pub author: Option<&'a str>,
    pub description: Option<&'a str>,
    pub target: Option<&'a str>,
    pub replacements: &'a BTreeMap<String, Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mapping {
    pub source: String,
    pub destination: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct QuickappIcon {
    pub package: String,
    pub destination: String,
}

/// Validate authoring metadata independently of whether a project has assets yet.
pub fn validate_pack_metadata(
    theme_id: &str,
    name: &str,
    version: Option<&str>,
    author: Option<&str>,
    description: Option<&str>,
    target: Option<&str>,
) -> Result<(), String> {
    validate_theme_id(theme_id)?;
    validate_text(name, "name", 128, true)?;
    validate_optional_text(version, "version", 64)?;
    validate_optional_text(author, "author", 128)?;
    if let Some(description) = description {
        validate_description(description)?;
    }
    if let Some(target) = target {
        validate_text(target, "targets[0]", 128, true)?;
    }
    Ok(())
}

pub fn build_crpack(options: &PackOptions<'_>) -> Result<Vec<u8>, String> {
    build_crpack_with_icons(options, &[], &[])
}

/// Build firmware replacements and explicit application declarations together.
/// Explicitly referenced assets never acquire an inferred /resource/ mapping.
pub fn build_crpack_with_icons(
    options: &PackOptions<'_>,
    explicit_mappings: &[Mapping],
    quickapp_icons: &[QuickappIcon],
) -> Result<Vec<u8>, String> {
    let mut mappings = firmware_mappings(options.replacements.keys().filter(|path| {
        !quickapp_icons.iter().any(|icon| icon.destination == **path)
            && !explicit_mappings.iter().any(|mapping| {
                mapping.destination == **path
                    || (mapping.destination.ends_with('/')
                        && path.starts_with(&mapping.destination))
            })
    }))?;
    mappings.extend_from_slice(explicit_mappings);
    build_crpack_with_mappings(options, &mappings, quickapp_icons)
}

/// Serialize authoritative rules exactly, leaving unreferenced imported files unmapped.
pub fn build_crpack_with_mappings(
    options: &PackOptions<'_>,
    mappings: &[Mapping],
    quickapp_icons: &[QuickappIcon],
) -> Result<Vec<u8>, String> {
    validate_pack_metadata(
        options.theme_id,
        options.name,
        options.version,
        options.author,
        options.description,
        options.target,
    )?;

    if options.replacements.is_empty() {
        return Err("replace at least one resource or application icon before exporting".into());
    }

    let mut total_bytes = 0usize;
    for (path, contents) in options.replacements {
        validate_relative_path(path)?;
        total_bytes = total_bytes
            .checked_add(contents.len())
            .ok_or_else(|| "replacement byte total overflow".to_string())?;
        if total_bytes > MAX_TOTAL_BYTES {
            return Err("replacement files exceed the 64 MiB CRPack limit".into());
        }
    }

    validate_file_ancestors(
        options.replacements.keys().map(String::as_str),
        options.replacements,
    )?;
    validate_icon_declarations(
        mappings,
        quickapp_icons,
        options.theme_id,
        options.replacements,
    )?;

    let mut mapping_json = Vec::with_capacity(mappings.len());
    for mapping in mappings {
        mapping_json.push(json!({
            "source": mapping.source,
            "destination": mapping.destination,
        }));
    }

    let mut manifest = Map::new();
    manifest.insert(
        "format".into(),
        Value::String("canopus-resource-pack".into()),
    );
    manifest.insert("formatVersion".into(), Value::Number(1.into()));
    manifest.insert(
        "themeId".into(),
        Value::String(options.theme_id.to_string()),
    );
    manifest.insert("name".into(), Value::String(options.name.to_string()));
    manifest.insert("mappings".into(), Value::Array(mapping_json));
    if !quickapp_icons.is_empty() {
        manifest.insert(
            "quickappIcons".into(),
            serde_json::to_value(quickapp_icons)
                .map_err(|error| format!("could not serialize quickappIcons: {error}"))?,
        );
    }
    if let Some(version) = options.version {
        manifest.insert("version".into(), Value::String(version.to_string()));
    }
    if let Some(author) = options.author {
        manifest.insert("author".into(), Value::String(author.to_string()));
    }
    if let Some(description) = &options.description {
        manifest.insert("description".into(), Value::String(description.to_string()));
    }
    if let Some(target) = options.target {
        manifest.insert(
            "targets".into(),
            Value::Array(vec![Value::String(target.to_string())]),
        );
    }

    let manifest_bytes = serde_json::to_vec_pretty(&Value::Object(manifest))
        .map_err(|error| format!("could not serialize canora.json: {error}"))?;
    if manifest_bytes.len() > MAX_MANIFEST_BYTES {
        return Err("canora.json exceeds the 64 KiB CRPack v1 limit".into());
    }
    total_bytes = total_bytes
        .checked_add(manifest_bytes.len())
        .ok_or_else(|| "CRPack total byte count overflow".to_string())?;
    if total_bytes > MAX_TOTAL_BYTES {
        return Err("replacement files plus canora.json exceed the 64 MiB CRPack limit".into());
    }

    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let file_options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(6));
    writer
        .start_file("canora.json", file_options)
        .map_err(|error| format!("could not add canora.json to CRPack: {error}"))?;
    writer
        .write_all(&manifest_bytes)
        .map_err(|error| format!("could not write canora.json: {error}"))?;

    for (path, contents) in options.replacements {
        writer
            .start_file(path, file_options)
            .map_err(|error| format!("could not add {path} to CRPack: {error}"))?;
        writer
            .write_all(contents)
            .map_err(|error| format!("could not write {path}: {error}"))?;
    }

    writer
        .finish()
        .map(|cursor| cursor.into_inner())
        .map_err(|error| format!("could not finish CRPack ZIP: {error}"))
}

/// Import a CRPack using the same strict validation used by CLI callers.
pub fn parse_crpack(bytes: &[u8]) -> Result<UnpackedCrpack, String> {
    validate_crpack(bytes)
}

/// Validate and safely decompress CRPack v1 without rewriting paths or mapping rules.
/// The 64 MiB budget includes canora.json; no container file-count limit is imposed.
pub fn validate_crpack(bytes: &[u8]) -> Result<UnpackedCrpack, String> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| format!("could not open CRPack archive: {error}"))?;
    validate_central_directory(bytes, archive.central_directory_start(), archive.len())?;

    let mut manifest_bytes = None;
    let mut replacements = BTreeMap::new();
    let mut seen = BTreeSet::new();
    let mut total_bytes = 0usize;
    for index in 0..archive.len() {
        let raw = archive
            .by_index_raw(index)
            .map_err(|error| format!("failed to read ZIP entry #{index}: {error}"))?;
        let path = raw.name().to_string();
        let directory = raw.is_dir();
        let body = if directory {
            path.strip_suffix('/').unwrap_or(&path)
        } else {
            &path
        };
        if path.len() >= MAX_PATH_BYTES {
            return Err(format!("unsafe CRPack relative path: {path:?}"));
        }
        validate_archive_path(body)?;
        if !seen.insert(body.to_string()) {
            return Err(format!("duplicate ZIP entry: {path}"));
        }
        if body.rsplit('/').next() == Some("mappings.tsv") {
            return Err("CRPack must not contain reserved mappings.tsv".into());
        }
        if directory && body == "canora.json" {
            return Err("canora.json must be an ordinary file at the ZIP root".into());
        }
        if raw.encrypted() {
            return Err(format!("encrypted ZIP entry is not supported: {path}"));
        }
        if !matches!(
            raw.compression(),
            CompressionMethod::Stored | CompressionMethod::Deflated
        ) {
            return Err(format!("unsupported ZIP compression for {path}"));
        }
        // is_file() alone also accepts Unix devices, sockets and FIFOs in zip 2.4.
        if let Some(mode) = raw.unix_mode() {
            let kind = mode & 0o170000;
            if kind != 0 && kind != if directory { 0o040000 } else { 0o100000 } {
                return Err(format!(
                    "symlink or special ZIP entry is not supported: {path}"
                ));
            }
        }
        if raw.is_symlink() {
            return Err(format!("symlink ZIP entry is not supported: {path}"));
        }
        let remaining = MAX_TOTAL_BYTES - total_bytes;
        let budget = if path == "canora.json" {
            remaining.min(MAX_MANIFEST_BYTES)
        } else {
            remaining
        };
        let declared_size = raw.size();
        if declared_size > budget as u64 {
            return Err(format!(
                "{path} exceeds the decompression limit ({budget} bytes remaining)"
            ));
        }
        drop(raw);
        let mut file = archive
            .by_index(index)
            .map_err(|error| format!("failed to read ZIP entry {path}: {error}"))?;
        let contents = read_bounded(&mut file, budget, &path)?;
        if contents.len() as u64 != declared_size {
            return Err(format!("ZIP entry size mismatch: {path}"));
        }
        total_bytes += contents.len();
        if directory {
            continue; // Still read to EOF to check CRC and actual decompressed size.
        }
        if path == "canora.json" {
            manifest_bytes = Some(contents);
        } else {
            replacements.insert(path, contents);
        }
    }
    validate_file_ancestors(seen.iter().map(String::as_str), &replacements)?;
    let manifest_bytes =
        manifest_bytes.ok_or_else(|| "missing canora.json in CRPack".to_string())?;
    let manifest: Value = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("invalid canora.json: {error}"))?;
    let obj = manifest
        .as_object()
        .ok_or_else(|| "canora.json must be a JSON object".to_string())?;
    if obj.get("format").and_then(Value::as_str) != Some("canopus-resource-pack") {
        return Err("missing or unsupported format in canora.json".into());
    }
    if obj.get("formatVersion").and_then(Value::as_u64) != Some(1) {
        return Err("missing or unsupported formatVersion in canora.json".into());
    }
    let theme_id = required_string(obj, "themeId")?.to_string();
    let name = required_string(obj, "name")?.to_string();
    let version = optional_string(obj, "version")?;
    let author = optional_string(obj, "author")?;
    let description = optional_string(obj, "description")?;
    validate_pack_metadata(
        &theme_id,
        &name,
        version.as_deref(),
        author.as_deref(),
        description.as_deref(),
        None,
    )?;

    let mut targets = Vec::new();
    if let Some(value) = obj.get("targets") {
        let values = value
            .as_array()
            .ok_or_else(|| "targets must be an array".to_string())?;
        if values.len() > 16 {
            return Err("targets allows at most 16 entries".into());
        }
        for (index, value) in values.iter().enumerate() {
            let target = value
                .as_str()
                .ok_or_else(|| format!("targets[{index}] must be a string"))?;
            validate_text(target, &format!("targets[{index}]"), 128, false)?;
            targets.push(target.to_string());
        }
    }
    let values = obj
        .get("mappings")
        .and_then(Value::as_array)
        .ok_or_else(|| "canora.json missing or invalid 'mappings' array".to_string())?;
    if values.len() > MAX_MAPPINGS {
        return Err(format!("CRPack v1 allows at most {MAX_MAPPINGS} mappings"));
    }
    let mut mappings = Vec::with_capacity(values.len());
    for (index, value) in values.iter().enumerate() {
        let mapping = value
            .as_object()
            .ok_or_else(|| format!("mappings[{index}] must be an object"))?;
        mappings.push(Mapping {
            source: required_string(mapping, "source")?.to_string(),
            destination: required_string(mapping, "destination")?.to_string(),
        });
    }
    let quickapp_icons: Vec<QuickappIcon> = match obj.get("quickappIcons") {
        None => Vec::new(),
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|error| format!("invalid quickappIcons: {error}"))?,
    };
    validate_icon_declarations(&mappings, &quickapp_icons, &theme_id, &replacements)?;
    Ok(UnpackedCrpack {
        theme_id,
        name,
        version,
        author,
        description,
        target: targets.first().cloned(),
        targets,
        replacements,
        mappings,
        quickapp_icons,
        manifest_bytes,
    })
}

// A file cannot also serve as an ancestor directory of another entry.
fn validate_file_ancestors<'a>(
    paths: impl Iterator<Item = &'a str>,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<(), String> {
    for path in paths {
        for (index, _) in path.match_indices('/') {
            let ancestor = &path[..index];
            if files.contains_key(ancestor) || ancestor == "canora.json" {
                return Err(format!("ZIP file/directory conflict: {ancestor}"));
            }
        }
    }
    Ok(())
}

fn required_string<'a>(object: &'a Map<String, Value>, key: &str) -> Result<&'a str, String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("canora.json missing or invalid '{key}' string"))
}

fn optional_string(object: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    object
        .get(key)
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("canora.json '{key}' must be a string"))
        })
        .transpose()
}

/// Read at most budget bytes plus a one-byte overflow probe, never allocating from ZIP sizes.
/// Reading through EOF (including at an exact budget boundary) lets zip verify the CRC.
fn read_bounded(reader: &mut impl Read, budget: usize, path: &str) -> Result<Vec<u8>, String> {
    let mut contents = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let requested = buffer.len().min(budget - contents.len() + 1);
        let read = reader
            .read(&mut buffer[..requested])
            .map_err(|error| format!("failed to extract {path}: {error}"))?;
        if read == 0 {
            return Ok(contents);
        }
        if read > budget - contents.len() {
            return Err(format!(
                "{path} exceeds the decompression limit ({budget} bytes)"
            ));
        }
        if contents.capacity() - contents.len() < read {
            let capacity = contents
                .capacity()
                .saturating_mul(2)
                .max(contents.len() + read)
                .min(budget);
            contents
                .try_reserve_exact(capacity - contents.len())
                .map_err(|error| {
                    format!("could not allocate decompression buffer for {path}: {error}")
                })?;
        }
        contents.extend_from_slice(&buffer[..read]);
    }
}

/// zip 2.4 indexes entries by name, silently discarding earlier duplicate entries.
/// Walk the raw central records as well, otherwise by_index cannot detect duplicates.
fn validate_central_directory(
    bytes: &[u8],
    start: u64,
    indexed_count: usize,
) -> Result<(), String> {
    let mut offset = usize::try_from(start).map_err(|_| "ZIP central directory offset overflow")?;
    let mut count = 0usize;
    while bytes.get(offset..offset.saturating_add(4)) == Some(b"PK\x01\x02") {
        let header = bytes
            .get(offset..offset.saturating_add(46))
            .ok_or_else(|| "truncated ZIP central directory".to_string())?;
        let length = |index| u16::from_le_bytes([header[index], header[index + 1]]) as usize;
        offset = offset
            .checked_add(46 + length(28) + length(30) + length(32))
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| "truncated ZIP central directory entry".to_string())?;
        count += 1;
    }
    if count != indexed_count {
        return Err("duplicate ZIP entries (canora.json must appear exactly once)".into());
    }
    Ok(())
}

/// Infer directory-grouped mappings only for intentionally authored firmware files.
pub fn firmware_mappings<'a>(
    paths: impl Iterator<Item = &'a String>,
) -> Result<Vec<Mapping>, String> {
    let mut groups = BTreeMap::<String, bool>::new();
    for path in paths {
        let group = match path.split_once('/') {
            Some((top_level, _)) => format!("{top_level}/"),
            None => path.clone(),
        };
        groups.insert(group, true);
    }

    if groups.len() > MAX_MAPPINGS {
        return Err(format!(
            "these replacements need {} mapping rules; CRPack v1 allows at most {}. Keep replacements under fewer top-level resource directories.",
            groups.len(),
            MAX_MAPPINGS
        ));
    }

    Ok(groups
        .into_keys()
        .map(|destination| {
            let source = format!("/resource/{destination}");
            Mapping {
                source,
                destination,
            }
        })
        .collect())
}

fn validate_mappings(mappings: &[Mapping], theme_id: &str) -> Result<(), String> {
    let mut seen = BTreeMap::<&str, ()>::new();
    let mut serialized_bytes = 0usize;
    let destination_root = format!("{THEME_ROOT}{theme_id}/");
    let relative_root = format!("themes/{theme_id}/");

    if mappings.len() > MAX_MAPPINGS {
        return Err(format!("CRPack v1 allows at most {MAX_MAPPINGS} mappings"));
    }
    for mapping in mappings {
        validate_absolute_source(&mapping.source)?;
        validate_relative_destination(&mapping.destination)?;
        let quickapp = mapping
            .source
            .starts_with(app_icons::QUICKAPP_SOURCE_PREFIX);
        if quickapp && !mapping.destination.ends_with(".bin") {
            return Err("QuickApp icon destination must be a lowercase .bin file".into());
        }
        // A QuickApp source is a semantic key, even when its package ends in '/'.
        // Its .bin destination is always a file, never a directory mapping.
        if !quickapp && mapping.source.ends_with('/') != mapping.destination.ends_with('/') {
            return Err(
                "mapping source and destination must both be files or both be directories".into(),
            );
        }
        if seen.insert(&mapping.source, ()).is_some() {
            return Err(format!("duplicate mapping source: {}", mapping.source));
        }
        let absolute_destination = format!("{destination_root}{}", mapping.destination);
        if absolute_destination.len() >= MAX_PATH_BYTES {
            return Err(format!(
                "mapped device path exceeds 255 UTF-8 bytes: {absolute_destination}"
            ));
        }
        serialized_bytes = serialized_bytes
            .checked_add(
                mapping.source.len() + 1 + relative_root.len() + mapping.destination.len() + 1,
            )
            .ok_or_else(|| "generated mappings.tsv size overflow".to_string())?;
    }
    if serialized_bytes > MAX_CONFIG_BYTES {
        return Err("generated mappings.tsv exceeds the 32 KiB device limit".into());
    }
    Ok(())
}

fn validate_icon_declarations(
    mappings: &[Mapping],
    icons: &[QuickappIcon],
    theme_id: &str,
    replacements: &BTreeMap<String, Vec<u8>>,
) -> Result<(), String> {
    if mappings.len().saturating_add(icons.len()) > MAX_MAPPINGS {
        return Err(format!(
            "mappings and quickappIcons allow at most {MAX_MAPPINGS} combined rules"
        ));
    }
    let mut combined = mappings.to_vec();
    for icon in icons {
        app_icons::validate_package(&icon.package)?;
        if !icon.destination.ends_with(".bin") {
            return Err("QuickApp icon destination must be a lowercase .bin file".into());
        }
        combined.push(Mapping {
            source: app_icons::source(&icon.package),
            destination: icon.destination.clone(),
        });
    }
    validate_mappings(&combined, theme_id)?;
    validate_mapping_destinations(&combined, theme_id, replacements)
}

fn validate_mapping_destinations(
    mappings: &[Mapping],
    theme_id: &str,
    replacements: &BTreeMap<String, Vec<u8>>,
) -> Result<(), String> {
    // Every resource is installed, even when it is not referenced by a mapping.
    validate_installed_paths(theme_id, replacements)?;
    for mapping in mappings {
        let directory = mapping.destination.ends_with('/');
        let mut matched = false;
        // A directory entry alone is not a resource destination: it must contain a file.
        for (path, _) in replacements.range(mapping.destination.clone()..) {
            let suffix = if directory {
                match path.strip_prefix(&mapping.destination) {
                    Some(suffix) => suffix,
                    None => break,
                }
            } else if path == &mapping.destination {
                ""
            } else {
                break;
            };
            matched = true;
            if mapping.source.len() + suffix.len() >= MAX_PATH_BYTES {
                return Err(format!(
                    "mapped firmware source exceeds 255 UTF-8 bytes: {}{suffix}",
                    mapping.source
                ));
            }
        }
        if !matched {
            return Err(format!(
                "mapping destination has no resource file in the archive: {}",
                mapping.destination
            ));
        }
    }
    Ok(())
}

fn validate_installed_paths(
    theme_id: &str,
    replacements: &BTreeMap<String, Vec<u8>>,
) -> Result<(), String> {
    let root = format!("{THEME_ROOT}{theme_id}/");
    for path in replacements.keys() {
        if root.len() + path.len() >= MAX_PATH_BYTES {
            return Err(format!(
                "installed device file path exceeds 255 UTF-8 bytes: {root}{path}"
            ));
        }
    }
    Ok(())
}

fn validate_theme_id(theme_id: &str) -> Result<(), String> {
    if theme_id.is_empty()
        || theme_id.len() > 12
        || !theme_id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
        })
    {
        return Err("themeId must be 1-12 lowercase ASCII letters, digits, '_' or '-'".into());
    }
    Ok(())
}

fn validate_text(value: &str, label: &str, max_bytes: usize, required: bool) -> Result<(), String> {
    if (required && value.is_empty()) || value.len() > max_bytes {
        return Err(format!(
            "{label} must be {}-{} UTF-8 bytes",
            usize::from(required),
            max_bytes
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(format!("{label} cannot contain control characters"));
    }
    Ok(())
}

fn validate_description(value: &str) -> Result<(), String> {
    if value.len() > 1024 {
        return Err("description must be 0-1024 UTF-8 bytes".into());
    }
    if value
        .chars()
        .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
    {
        return Err(
            "description cannot contain control characters other than TAB, CR or LF".into(),
        );
    }
    Ok(())
}

fn validate_optional_text(
    value: Option<&str>,
    label: &str,
    max_bytes: usize,
) -> Result<(), String> {
    if let Some(value) = value {
        validate_text(value, label, max_bytes, false)?;
    }
    Ok(())
}

pub fn validate_relative_path(path: &str) -> Result<(), String> {
    if path == "canora.json" || path.rsplit('/').next() == Some("mappings.tsv") {
        return Err(format!(
            "{path} is reserved and must not be included as a replacement"
        ));
    }
    validate_archive_path(path)
}

fn validate_archive_path(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.starts_with('/')
        || path.len() >= MAX_PATH_BYTES
        || path.contains('\\')
        || path.contains(':')
        || path.chars().any(char::is_control)
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(format!("unsafe CRPack relative path: {path:?}"));
    }
    Ok(())
}

fn validate_absolute_source(path: &str) -> Result<(), String> {
    // This is the serialized source budget, including a semantic key's prefix.
    if path.len() >= MAX_PATH_BYTES {
        return Err(format!("mapping source exceeds 255 UTF-8 bytes: {path:?}"));
    }
    if let Some(package) = path.strip_prefix(app_icons::QUICKAPP_SOURCE_PREFIX) {
        return app_icons::validate_package(package);
    }
    if !path.starts_with('/') {
        return Err(format!(
            "mapping source must be a short absolute firmware path: {path:?}"
        ));
    }
    if path.contains('\\') || path.contains(':') || path.chars().any(char::is_control) {
        return Err(format!("unsafe CRPack mapping source: {path:?}"));
    }
    if path != "/" {
        let body = path.strip_suffix('/').unwrap_or(path);
        if body[1..]
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err(format!("unsafe CRPack mapping source: {path:?}"));
        }
    }
    Ok(())
}

fn validate_relative_destination(path: &str) -> Result<(), String> {
    if path.is_empty() || path.starts_with('/') || path.len() >= MAX_PATH_BYTES {
        return Err(format!("unsafe CRPack destination: {path:?}"));
    }
    if path.contains('\\') || path.contains(':') || path.chars().any(char::is_control) {
        return Err(format!("unsafe CRPack destination: {path:?}"));
    }
    let body = path.strip_suffix('/').unwrap_or(path);
    if body.is_empty()
        || body
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(format!("unsafe CRPack destination: {path:?}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use zip::ZipArchive;

    fn options<'a>(replacements: &'a BTreeMap<String, Vec<u8>>) -> PackOptions<'a> {
        PackOptions {
            theme_id: "dark",
            name: "Dark",
            version: Some("1.0.0"),
            author: None,
            description: None,
            target: Some("xiaomi-band-11-4.100.155"),
            replacements,
        }
    }

    #[test]
    fn exports_only_replacements_at_zip_root_with_a_valid_manifest() {
        let mut replacements = BTreeMap::new();
        replacements.insert("app/common/icon/confirm.bin".into(), vec![1, 2, 3]);
        replacements.insert("app/settings/launcher.bin".into(), vec![4, 5]);
        let bytes = build_crpack(&options(&replacements)).unwrap();
        let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
        assert_eq!(archive.len(), 3);

        let manifest: Value = serde_json::from_slice(&{
            let mut file = archive.by_name("canora.json").unwrap();
            let mut data = Vec::new();
            file.read_to_end(&mut data).unwrap();
            data
        })
        .unwrap();
        assert_eq!(manifest["format"], "canopus-resource-pack");
        assert_eq!(manifest["formatVersion"], 1);
        assert_eq!(manifest["targets"][0], "xiaomi-band-11-4.100.155");
        assert_eq!(manifest["mappings"].as_array().unwrap().len(), 1);
        assert_eq!(manifest["mappings"][0]["source"], "/resource/app/");
        assert_eq!(manifest["mappings"][0]["destination"], "app/");
        assert!(archive.by_name("app/common/icon/confirm.bin").is_ok());
        assert!(archive.by_name("wrapper/canora.json").is_err());
    }

    #[test]
    fn rejects_unsafe_paths_and_more_than_256_mapping_roots() {
        assert!(validate_relative_path("../escape.bin").is_err());
        assert!(validate_relative_path("app\\escape.bin").is_err());
        assert!(validate_relative_path("canora.json").is_err());
        assert!(validate_relative_path("mappings.tsv").is_err());
        let replacements = (0..257)
            .map(|index| (format!("root{index:02}/image.bin"), vec![1]))
            .collect();
        assert!(build_crpack(&options(&replacements)).is_err());
    }

    #[test]
    fn manifest_and_assets_share_the_64_mib_limit() {
        let mut replacements = BTreeMap::new();
        replacements.insert("app/file.bin".into(), vec![0; MAX_TOTAL_BYTES]);
        assert!(build_crpack(&options(&replacements)).is_err());
    }

    #[test]
    fn parses_exported_crpack_roundtrip() {
        let mut replacements = BTreeMap::new();
        replacements.insert("app/common/icon/confirm.bin".into(), vec![1, 2, 3]);
        replacements.insert("app/settings/launcher.bin".into(), vec![4, 5]);
        let pack_opts = PackOptions {
            theme_id: "test_pack",
            name: "Test Theme",
            version: Some("1.2.3"),
            author: Some("Tester"),
            description: Some("Description of test"),
            target: Some("xiaomi-band-11-4.100.155"),
            replacements: &replacements,
        };
        let bytes = build_crpack(&pack_opts).unwrap();
        let unpacked = parse_crpack(&bytes).unwrap();

        assert_eq!(unpacked.theme_id, "test_pack");
        assert_eq!(unpacked.name, "Test Theme");
        assert_eq!(unpacked.version.as_deref(), Some("1.2.3"));
        assert_eq!(unpacked.author.as_deref(), Some("Tester"));
        assert_eq!(unpacked.description.as_deref(), Some("Description of test"));
        assert_eq!(unpacked.target.as_deref(), Some("xiaomi-band-11-4.100.155"));
        assert_eq!(unpacked.replacements, replacements);
    }

    #[test]
    fn roundtrips_packs_beyond_the_former_128_file_limit() {
        for count in [127, 128, 129, 1024] {
            let replacements: BTreeMap<String, Vec<u8>> = (0..count)
                .map(|index| {
                    (
                        format!("app/icons/icon_{index:04}.bin"),
                        (index as u32).to_le_bytes().to_vec(),
                    )
                })
                .collect();
            let bytes = build_crpack(&options(&replacements)).unwrap();
            let mut archive = ZipArchive::new(Cursor::new(&bytes)).unwrap();
            assert_eq!(archive.len(), count + 1);

            let mut manifest_bytes = Vec::new();
            archive
                .by_name("canora.json")
                .unwrap()
                .read_to_end(&mut manifest_bytes)
                .unwrap();
            let manifest: Value = serde_json::from_slice(&manifest_bytes).unwrap();
            assert_eq!(manifest["mappings"].as_array().unwrap().len(), 1);
            assert_eq!(parse_crpack(&bytes).unwrap().replacements, replacements);
        }
    }

    #[test]
    fn parse_crpack_rejects_malformed_archive_and_missing_manifest() {
        assert!(parse_crpack(&[]).is_err());
        assert!(parse_crpack(b"not a zip file").is_err());

        // Create zip without canora.json
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let file_options = SimpleFileOptions::default();
        writer.start_file("app/test.bin", file_options).unwrap();
        writer.write_all(b"123").unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        let err = parse_crpack(&bytes).unwrap_err();
        assert!(err.contains("missing canora.json"));
    }

    #[test]
    fn parse_crpack_rejects_unsafe_paths_and_accepts_empty_mappings() {
        // Zip with only canora.json (no replacements)
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let file_options = SimpleFileOptions::default();
        writer.start_file("canora.json", file_options).unwrap();
        let manifest = json!({
            "format": "canopus-resource-pack",
            "formatVersion": 1,
            "themeId": "safe_theme",
            "name": "Safe Theme",
            "mappings": [],
        });
        writer
            .write_all(&serde_json::to_vec(&manifest).unwrap())
            .unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        let unpacked = parse_crpack(&bytes).unwrap();
        assert!(unpacked.replacements.is_empty());
        assert!(unpacked.mappings.is_empty());
        assert_eq!(unpacked.files().count(), 1);

        // Zip with path traversal
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        writer.start_file("canora.json", file_options).unwrap();
        writer
            .write_all(&serde_json::to_vec(&manifest).unwrap())
            .unwrap();
        writer.start_file("../unsafe.bin", file_options).unwrap();
        writer.write_all(b"evil").unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        assert!(parse_crpack(&bytes).is_err());
    }

    fn manifest() -> Value {
        json!({
            "format": "canopus-resource-pack", "formatVersion": 1,
            "themeId": "dark", "name": "Dark",
            "mappings": [{"source": "/resource/icon.bin", "destination": "icon.bin"}],
        })
    }

    fn archive_with(manifest: &Value, entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        writer.start_file("canora.json", opts).unwrap();
        writer
            .write_all(&serde_json::to_vec(manifest).unwrap())
            .unwrap();
        for (path, contents) in entries {
            if path.ends_with('/') {
                writer.add_directory(*path, opts).unwrap();
            } else {
                writer.start_file(*path, opts).unwrap();
                writer.write_all(contents).unwrap();
            }
        }
        writer.finish().unwrap().into_inner()
    }

    fn central_offsets(bytes: &[u8]) -> Vec<usize> {
        let archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut offset = archive.central_directory_start() as usize;
        let mut offsets = Vec::new();
        while bytes.get(offset..offset + 4) == Some(b"PK\x01\x02") {
            offsets.push(offset);
            let length = |index| {
                u16::from_le_bytes([bytes[offset + index], bytes[offset + index + 1]]) as usize
            };
            offset += 46 + length(28) + length(30) + length(32);
        }
        offsets
    }

    #[test]
    fn metadata_can_be_checked_without_assets() {
        assert!(validate_pack_metadata("dark", "Dark", None, None, None, None).is_ok());
        assert!(validate_pack_metadata("Dark", "Dark", None, None, None, None).is_err());
        assert!(validate_pack_metadata("dark", "", None, None, None, None).is_err());
        assert!(
            validate_pack_metadata("dark", "Dark", Some(&"v".repeat(65)), None, None, None)
                .is_err()
        );
    }

    #[test]
    fn accepts_256_mapping_roots() {
        let replacements = (0..256)
            .map(|index| (format!("root{index:03}/image.bin"), vec![1]))
            .collect();
        let bytes = build_crpack(&options(&replacements)).unwrap();
        let unpacked = validate_crpack(&bytes).unwrap();
        assert_eq!(unpacked.mappings.len(), 256);
        assert_eq!(unpacked.replacements, replacements);
    }

    #[test]
    fn preserves_arbitrary_ordered_rules_and_original_manifest_for_cli() {
        let mut manifest = manifest();
        manifest["targets"] = json!(["band-10", "band-11"]);
        manifest["extra"] = json!({"leave": "untouched"});
        manifest["mappings"] = json!([
            {"source": "/system/fonts/", "destination": "fonts/"},
            {"source": "/resource/icon.bin", "destination": "icons/alternate.bin"},
            {"source": "/system/fonts/exact.bin", "destination": "icons/alternate.bin"},
        ]);
        let bytes = archive_with(
            &manifest,
            &[
                ("fonts/", b""),
                ("fonts/exact.bin", b"font"),
                ("icons/alternate.bin", b"icon"),
                ("unmapped.bin", b"extra"),
            ],
        );
        let unpacked = validate_crpack(&bytes).unwrap();
        assert_eq!(unpacked.mappings[0].source, "/system/fonts/");
        assert_eq!(unpacked.mappings[1].destination, "icons/alternate.bin");
        assert_eq!(unpacked.targets, vec!["band-10", "band-11"]);
        assert_eq!(unpacked.target.as_deref(), Some("band-10"));
        assert_eq!(
            unpacked.manifest_bytes,
            serde_json::to_vec(&manifest).unwrap()
        );
        assert_eq!(unpacked.files().count(), 4); // directories are not transmitted
        assert_eq!(unpacked.replacements["unmapped.bin"], b"extra");
    }

    #[test]
    fn validates_manifest_types_markers_and_all_targets() {
        for (key, value) in [
            ("format", json!("wrong")),
            ("formatVersion", json!(2)),
            ("formatVersion", json!(1.0)),
            ("formatVersion", json!("1")),
            ("mappings", Value::Null),
            ("mappings", json!([true])),
            (
                "mappings",
                json!([{"source": 1, "destination": "icon.bin"}]),
            ),
            ("version", json!(5)),
            ("author", Value::Null),
            ("description", json!(false)),
            ("targets", json!("band")),
            ("targets", json!(["ok", 1])),
            ("targets", json!(["ok", "x".repeat(129)])),
            ("targets", json!(vec!["band"; 17])),
        ] {
            let mut manifest = manifest();
            manifest[key] = value;
            assert!(
                validate_crpack(&archive_with(&manifest, &[("icon.bin", b"x")])).is_err(),
                "{key}: {manifest}"
            );
        }
        let mut missing = manifest();
        missing.as_object_mut().unwrap().remove("mappings");
        assert!(validate_crpack(&archive_with(&missing, &[("icon.bin", b"x")])).is_err());
        let mut too_many = manifest();
        too_many["mappings"] = json!(vec![
            json!({"source":"/resource/icon.bin", "destination":"icon.bin"});
            257
        ]);
        assert!(
            validate_crpack(&archive_with(&too_many, &[("icon.bin", b"x")]))
                .unwrap_err()
                .contains("256")
        );
    }

    #[test]
    fn rejects_unsafe_rules_and_nonexistent_destinations() {
        for (source, destination) in [
            ("resource/icon.bin", "icon.bin"),
            ("/resource/../icon.bin", "icon.bin"),
            ("/resource//icon.bin", "icon.bin"),
            ("/resource/\ticon.bin", "icon.bin"),
            ("/resource/icon.bin", "../icon.bin"),
            ("/resource/icon.bin", "/icon.bin"),
            ("/resource/icon.bin", "dir\\icon.bin"),
            ("/resource/icon.bin", "missing.bin"),
            ("/resource/", "empty/"),
            ("/resource/", "icon.bin"),
            ("/resource/icon.bin", "canora.json"),
            ("/resource/", "icons2/"),
        ] {
            let mut manifest = manifest();
            manifest["mappings"] = json!([{"source":source, "destination":destination}]);
            let bytes = archive_with(
                &manifest,
                &[
                    ("icon.bin", b"x"),
                    ("empty/", b""),
                    ("icons/file.bin", b"x"),
                ],
            );
            assert!(
                validate_crpack(&bytes).is_err(),
                "{source} -> {destination}"
            );
        }
    }

    #[test]
    fn checks_path_limits_including_directory_suffixes_and_relative_tsv_budget() {
        let root_len = format!("{THEME_ROOT}dark/").len();
        let path = "a".repeat(255 - root_len);
        let mut valid = manifest();
        valid["mappings"] = json!([{"source":"/resource/icon.bin", "destination":path}]);
        assert!(validate_crpack(&archive_with(&valid, &[(&path, b"x")])).is_ok());
        let too_long = format!("{path}a");
        valid["mappings"][0]["destination"] = json!(too_long);
        assert!(validate_crpack(&archive_with(&valid, &[(&too_long, b"x")])).is_err());

        let mut valid = manifest();
        valid["mappings"] =
            json!([{"source":format!("/{}/", "s".repeat(253)), "destination":"icons/"}]);
        assert!(validate_crpack(&archive_with(&valid, &[("icons/x", b"x")])).is_err());
        valid["mappings"][0]["source"] = json!(format!("/{}", "s".repeat(255)));
        valid["mappings"][0]["destination"] = json!("icon.bin");
        assert!(validate_crpack(&archive_with(&valid, &[("icon.bin", b"x")])).is_err());

        // TSV uses themes/... relative paths, not the longer native filesystem root.
        let mappings: Vec<Mapping> = (0..256)
            .map(|i| Mapping {
                source: format!("/{}-{i:03}", "s".repeat(55)),
                destination: "d".repeat(40),
            })
            .collect();
        assert!(validate_mappings(&mappings, "dark").is_ok());
        let larger: Vec<Mapping> = mappings
            .into_iter()
            .map(|mut mapping| {
                mapping.source.push_str(&"s".repeat(50));
                mapping
            })
            .collect();
        assert!(
            validate_mappings(&larger, "dark")
                .unwrap_err()
                .contains("32 KiB")
        );
    }

    #[test]
    fn rejects_unsafe_directories_reserved_files_and_file_directory_conflicts() {
        for path in [
            "../escape/",
            "/absolute/",
            "empty//",
            "./",
            "a/../",
            "a\\b/",
            "mappings.tsv",
            "nested/mappings.tsv",
            "canora.json/",
        ] {
            let bytes = archive_with(&manifest(), &[("icon.bin", b"x"), (path, b"")]);
            assert!(validate_crpack(&bytes).is_err(), "{path}");
        }
        for entries in [
            vec![("icon.bin", b"x".as_slice()), ("icon.bin/child", b"x")],
            vec![("icon.bin", b"x".as_slice()), ("icon.bin/", b"")],
        ] {
            assert!(validate_crpack(&archive_with(&manifest(), &entries)).is_err());
        }
        let mut empty = manifest();
        empty["mappings"] = json!([]);
        assert!(validate_crpack(&archive_with(&empty, &[("safe/", b"")])).is_ok());
    }

    #[test]
    fn rejects_duplicate_manifest_and_resource_entries_hidden_by_zip_index() {
        // ZipWriter itself prohibits duplicate names, so replace an equal-length name.
        for (old, new) in [("second.json", "canora.json"), ("copy.bin", "icon.bin")] {
            let mut bytes = archive_with(&manifest(), &[("icon.bin", b"x"), (old, b"x")]);
            assert_eq!(old.len(), new.len());
            let positions: Vec<usize> = bytes
                .windows(old.len())
                .enumerate()
                .filter_map(|(index, bytes)| (bytes == old.as_bytes()).then_some(index))
                .collect();
            assert_eq!(positions.len(), 2);
            for offset in positions {
                bytes[offset..offset + old.len()].copy_from_slice(new.as_bytes());
            }
            let indexed = ZipArchive::new(Cursor::new(&bytes)).unwrap();
            assert_eq!(indexed.len(), 2); // zip silently hid one entry
            assert!(validate_crpack(&bytes).unwrap_err().contains("duplicate"));
        }
    }

    #[test]
    fn rejects_encryption_unsupported_compression_and_special_modes_even_on_directories() {
        for directory in [false, true] {
            let extra = if directory { "safe/" } else { "safe.bin" };
            let bytes = archive_with(&manifest(), &[("icon.bin", b"x"), (extra, b"")]);
            let offsets = central_offsets(&bytes);
            let central = offsets[2];
            let local =
                u32::from_le_bytes(bytes[central + 42..central + 46].try_into().unwrap()) as usize;
            let mut encrypted = bytes.clone();
            encrypted[central + 8] |= 1;
            encrypted[local + 6] |= 1;
            assert!(
                validate_crpack(&encrypted)
                    .unwrap_err()
                    .contains("encrypted")
            );
            let mut unsupported = bytes.clone();
            unsupported[central + 10..central + 12].copy_from_slice(&99u16.to_le_bytes());
            unsupported[local + 8..local + 10].copy_from_slice(&99u16.to_le_bytes());
            assert!(validate_crpack(&unsupported).is_err());
            for mode in [0o120777u32, 0o010644, 0o020644, 0o060644, 0o140644] {
                let mut special = bytes.clone();
                special[central + 5] = 3; // Unix host; mode is in upper 16 attribute bits.
                special[central + 38..central + 42].copy_from_slice(&(mode << 16).to_le_bytes());
                assert!(validate_crpack(&special).is_err(), "mode {mode:o}");
            }
        }
    }

    #[test]
    fn crc_and_declared_sizes_are_checked_by_reading_to_eof() {
        let bytes = archive_with(&manifest(), &[("icon.bin", b"xyz")]);
        let mut archive = ZipArchive::new(Cursor::new(&bytes)).unwrap();
        for index in 0..2 {
            let start = archive.by_index_raw(index).unwrap().data_start() as usize;
            let mut corrupt = bytes.clone();
            corrupt[start] ^= 1;
            assert!(validate_crpack(&corrupt).unwrap_err().contains("extract"));
        }
        let central = central_offsets(&bytes)[1];
        let mut forged = bytes.clone();
        forged[central + 24..central + 28].copy_from_slice(&1u32.to_le_bytes());
        assert!(
            validate_crpack(&forged)
                .unwrap_err()
                .contains("size mismatch")
        );
    }

    #[test]
    fn bounds_actual_manifest_decompression_even_when_declared_size_is_forged() {
        let mut manifest = manifest();
        manifest["extra"] = json!("x".repeat(MAX_MANIFEST_BYTES));
        let mut bytes = archive_with(&manifest, &[("icon.bin", b"x")]);
        let central = central_offsets(&bytes)[0];
        bytes[central + 24..central + 28].copy_from_slice(&1u32.to_le_bytes());
        assert!(
            validate_crpack(&bytes)
                .unwrap_err()
                .contains("decompression limit")
        );
    }

    #[test]
    fn bounded_reader_stops_at_one_overflow_byte_and_checks_exact_boundary_eof() {
        let mut reader = Cursor::new(vec![0; 100_000]);
        assert!(read_bounded(&mut reader, 16, "test").is_err());
        assert_eq!(reader.position(), 17);
        let mut exact = Cursor::new(vec![0; 16]);
        assert_eq!(read_bounded(&mut exact, 16, "test").unwrap().len(), 16);
        assert!(read_bounded(&mut Cursor::new(vec![1]), 0, "test").is_err());
        assert!(
            read_bounded(&mut Cursor::new(Vec::new()), 0, "test")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn actual_total_budget_includes_manifest_despite_forged_resource_size() {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        writer.start_file("canora.json", opts).unwrap();
        writer
            .write_all(&serde_json::to_vec(&manifest()).unwrap())
            .unwrap();
        writer.start_file("icon.bin", opts).unwrap();
        // Stream a compressible 64 MiB asset so the test fixture itself stays tiny.
        let zeros = [0; 8192];
        for _ in 0..MAX_TOTAL_BYTES / zeros.len() {
            writer.write_all(&zeros).unwrap();
        }
        let mut bytes = writer.finish().unwrap().into_inner();
        let central = central_offsets(&bytes)[1];
        bytes[central + 24..central + 28].copy_from_slice(&1u32.to_le_bytes());
        assert!(
            validate_crpack(&bytes)
                .unwrap_err()
                .contains("decompression limit")
        );
    }

    #[test]
    fn builder_rejects_file_directory_conflicts_and_manifest_ancestors() {
        for replacements in [
            BTreeMap::from([("file".into(), vec![1]), ("file/child".into(), vec![1])]),
            BTreeMap::from([("canora.json/child".into(), vec![1])]),
        ] {
            assert!(
                build_crpack(&options(&replacements))
                    .unwrap_err()
                    .contains("conflict")
            );
        }
        let directory = format!("{}/", "x".repeat(255));
        assert!(
            validate_crpack(&archive_with(
                &manifest(),
                &[("icon.bin", b"x"), (&directory, b"")]
            ))
            .is_err()
        );
    }

    #[test]
    fn multiline_descriptions_roundtrip_in_builder_and_parser() {
        let replacements = BTreeMap::from([("icon.bin".into(), vec![1])]);
        let description = "First line\nSecond line\r\n\tIndented details";
        let mut opts = options(&replacements);
        opts.description = Some(description);
        let bytes = build_crpack(&opts).unwrap();
        assert_eq!(
            parse_crpack(&bytes).unwrap().description.as_deref(),
            Some(description)
        );

        let mut imported = manifest();
        imported["description"] = json!(description);
        let bytes = archive_with(&imported, &[("icon.bin", b"x")]);
        assert_eq!(
            validate_crpack(&bytes).unwrap().description.as_deref(),
            Some(description)
        );
        assert!(validate_description(&"\n".repeat(1024)).is_ok());
        assert!(validate_description(&"\n".repeat(1025)).is_err());
        for control in ["\u{0}", "\u{1b}", "\u{7f}", "\u{85}"] {
            opts.description = Some(control);
            assert!(build_crpack(&opts).is_err());
            imported["description"] = json!(control);
            assert!(validate_crpack(&archive_with(&imported, &[("icon.bin", b"x")])).is_err());
        }
        // Only descriptions permit these whitespace controls, not other metadata or paths.
        assert!(validate_pack_metadata("dark", "Dark\n", None, None, None, None).is_err());
        assert!(validate_relative_path("icons/\ticon.bin").is_err());
        assert!(validate_absolute_source("/resource/\nicon.bin").is_err());
        assert!(validate_relative_destination("icons/\ricon.bin").is_err());
    }

    #[test]
    fn installed_path_limit_applies_to_unmapped_resources_and_empty_mappings() {
        let root_len = format!("{THEME_ROOT}dark/").len();
        let boundary = "a".repeat(255 - root_len);
        let too_long = format!("{boundary}a");
        assert!(validate_relative_path(&too_long).is_ok());
        for mappings in [json!([]), manifest()["mappings"].clone()] {
            let mut imported = manifest();
            imported["mappings"] = mappings;
            let valid = archive_with(&imported, &[("icon.bin", b"x"), (&boundary, b"x")]);
            assert!(validate_crpack(&valid).is_ok());
            let invalid = archive_with(&imported, &[("icon.bin", b"x"), (&too_long, b"x")]);
            let error = validate_crpack(&invalid).unwrap_err();
            assert!(error.contains("installed device file path"), "{error}");
        }
        let replacements = BTreeMap::from([(too_long, vec![1])]);
        assert!(build_crpack(&options(&replacements)).is_err());
    }
}
