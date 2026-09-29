use std::collections::BTreeMap;
use std::io::{Cursor, Read, Write};

use serde_json::{Map, Value, json};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

const MAX_FILES: usize = 128;
const MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024;
const MAX_MANIFEST_BYTES: usize = 64 * 1024;
const MAX_MAPPINGS: usize = 64;
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

#[derive(Clone, Debug)]
struct Mapping {
    source: String,
    destination: String,
}

pub fn build_crpack(options: &PackOptions<'_>) -> Result<Vec<u8>, String> {
    validate_theme_id(options.theme_id)?;
    validate_text(options.name, "name", 128, true)?;
    validate_optional_text(options.version, "version", 64)?;
    validate_optional_text(options.author, "author", 128)?;
    validate_optional_text(options.description, "description", 1024)?;
    if let Some(target) = options.target {
        validate_text(target, "targets[0]", 128, true)?;
    }

    if options.replacements.is_empty() {
        return Err("replace at least one firmware resource before exporting".into());
    }
    if options.replacements.len() + 1 > MAX_FILES {
        return Err(format!(
            "CRPack v1 allows at most {} files including canora.json",
            MAX_FILES
        ));
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

    let mappings = mappings_for_paths(options.replacements.keys())?;
    validate_mappings(&mappings, options.theme_id)?;

    let mut mapping_json = Vec::with_capacity(mappings.len());
    for mapping in &mappings {
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

pub fn parse_crpack(bytes: &[u8]) -> Result<UnpackedCrpack, String> {
    if bytes.len() > MAX_TOTAL_BYTES {
        return Err("CRPack exceeds the 64 MiB limit".into());
    }

    let cursor = Cursor::new(bytes);
    let mut archive = ZipArchive::new(cursor)
        .map_err(|error| format!("could not open CRPack archive: {error}"))?;

    if archive.len() > MAX_FILES {
        return Err(format!(
            "CRPack contains {} entries, exceeding the maximum of {}",
            archive.len(),
            MAX_FILES
        ));
    }

    let mut manifest_bytes = Vec::new();
    {
        let mut manifest_file = archive
            .by_name("canora.json")
            .map_err(|_| "missing canora.json in CRPack".to_string())?;
        if manifest_file.size() > MAX_MANIFEST_BYTES as u64 {
            return Err("canora.json exceeds the 64 KiB CRPack v1 limit".into());
        }
        manifest_file
            .read_to_end(&mut manifest_bytes)
            .map_err(|error| format!("could not read canora.json: {error}"))?;
    }

    if manifest_bytes.len() > MAX_MANIFEST_BYTES {
        return Err("canora.json exceeds the 64 KiB CRPack v1 limit".into());
    }

    let manifest: Value = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("invalid canora.json: {error}"))?;

    let manifest_obj = manifest
        .as_object()
        .ok_or_else(|| "canora.json must be a JSON object".to_string())?;

    let format_str = manifest_obj
        .get("format")
        .and_then(Value::as_str)
        .ok_or_else(|| "canora.json missing 'format'".to_string())?;
    if format_str != "canopus-resource-pack" {
        return Err(format!("unsupported format in canora.json: {format_str}"));
    }

    let format_version = manifest_obj
        .get("formatVersion")
        .and_then(Value::as_u64)
        .ok_or_else(|| "canora.json missing or invalid 'formatVersion'".to_string())?;
    if format_version != 1 {
        return Err(format!(
            "unsupported formatVersion in canora.json: {format_version}"
        ));
    }

    let theme_id = manifest_obj
        .get("themeId")
        .and_then(Value::as_str)
        .ok_or_else(|| "canora.json missing 'themeId'".to_string())?
        .to_string();
    validate_theme_id(&theme_id)?;

    let name = manifest_obj
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| "canora.json missing 'name'".to_string())?
        .to_string();
    validate_text(&name, "name", 128, true)?;

    let version = manifest_obj
        .get("version")
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    validate_optional_text(version.as_deref(), "version", 64)?;

    let author = manifest_obj
        .get("author")
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    validate_optional_text(author.as_deref(), "author", 128)?;

    let description = manifest_obj
        .get("description")
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    validate_optional_text(description.as_deref(), "description", 1024)?;

    let target = manifest_obj
        .get("targets")
        .and_then(Value::as_array)
        .and_then(|targets| targets.first())
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    if let Some(target_str) = &target {
        validate_text(target_str, "targets[0]", 128, true)?;
    }

    let mut replacements = BTreeMap::new();
    let mut total_bytes = manifest_bytes.len();

    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|error| format!("failed to read ZIP entry #{i}: {error}"))?;
        let raw_name = file.name().to_string();

        if file.is_dir() || raw_name.ends_with('/') {
            continue;
        }

        if raw_name == "canora.json" || raw_name == "mappings.tsv" {
            continue;
        }

        validate_relative_path(&raw_name)?;

        let mut contents = Vec::new();
        file.read_to_end(&mut contents)
            .map_err(|error| format!("failed to extract {raw_name}: {error}"))?;

        total_bytes = total_bytes
            .checked_add(contents.len())
            .ok_or_else(|| "total byte count overflow".to_string())?;
        if total_bytes > MAX_TOTAL_BYTES {
            return Err("unpacked CRPack contents exceed the 64 MiB limit".into());
        }

        replacements.insert(raw_name, contents);
    }

    if replacements.is_empty() {
        return Err("CRPack does not contain any replacement files".into());
    }

    Ok(UnpackedCrpack {
        theme_id,
        name,
        version,
        author,
        description,
        target,
        replacements,
    })
}

fn mappings_for_paths<'a>(paths: impl Iterator<Item = &'a String>) -> Result<Vec<Mapping>, String> {
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

    if mappings.len() > MAX_MAPPINGS {
        return Err(format!("CRPack v1 allows at most {MAX_MAPPINGS} mappings"));
    }
    for mapping in mappings {
        validate_absolute_source(&mapping.source)?;
        validate_relative_destination(&mapping.destination)?;
        if mapping.source.ends_with('/') != mapping.destination.ends_with('/') {
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
            .checked_add(mapping.source.len() + 1 + absolute_destination.len() + 1)
            .ok_or_else(|| "generated mappings.tsv size overflow".to_string())?;
    }
    if serialized_bytes > MAX_CONFIG_BYTES {
        return Err("generated mappings.tsv exceeds the 32 KiB device limit".into());
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
    if path == "canora.json" || path == "mappings.tsv" {
        return Err(format!(
            "{path} is reserved and must not be included as a replacement"
        ));
    }
    if path.is_empty() || path.starts_with('/') || path.len() >= MAX_PATH_BYTES {
        return Err(format!("unsafe CRPack relative path: {path:?}"));
    }
    if path.contains('\\') || path.contains(':') || path.chars().any(char::is_control) {
        return Err(format!("unsafe CRPack relative path: {path:?}"));
    }
    let mut components = path.split('/').peekable();
    while let Some(component) = components.next() {
        if component.is_empty() || component == "." || component == ".." {
            return Err(format!("unsafe CRPack relative path: {path:?}"));
        }
        if components.peek().is_none() && component.ends_with('/') {
            return Err(format!("resource file path cannot end in '/': {path:?}"));
        }
    }
    Ok(())
}

fn validate_absolute_source(path: &str) -> Result<(), String> {
    if !path.starts_with("/resource/") || path.len() >= MAX_PATH_BYTES {
        return Err(format!(
            "mapping source must be a short /resource/ path: {path:?}"
        ));
    }
    if path.contains('\\') || path.contains(':') || path.chars().any(char::is_control) {
        return Err(format!("unsafe CRPack mapping source: {path:?}"));
    }
    let body = path.strip_suffix('/').unwrap_or(path);
    if body[1..]
        .split('/')
        .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(format!("unsafe CRPack mapping source: {path:?}"));
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
    fn rejects_unsafe_paths_and_more_than_64_mapping_roots() {
        assert!(validate_relative_path("../escape.bin").is_err());
        assert!(validate_relative_path("app\\escape.bin").is_err());
        assert!(validate_relative_path("canora.json").is_err());
        assert!(validate_relative_path("mappings.tsv").is_err());
        let replacements = (0..65)
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
    fn parse_crpack_rejects_unsafe_paths_and_empty_replacements() {
        // Zip with only canora.json (no replacements)
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let file_options = SimpleFileOptions::default();
        writer.start_file("canora.json", file_options).unwrap();
        let manifest = json!({
            "format": "canopus-resource-pack",
            "formatVersion": 1,
            "themeId": "safe_theme",
            "name": "Safe Theme",
        });
        writer.write_all(&serde_json::to_vec(&manifest).unwrap()).unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        let err = parse_crpack(&bytes).unwrap_err();
        assert!(err.contains("does not contain any replacement"));

        // Zip with path traversal
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        writer.start_file("canora.json", file_options).unwrap();
        writer.write_all(&serde_json::to_vec(&manifest).unwrap()).unwrap();
        writer.start_file("../unsafe.bin", file_options).unwrap();
        writer.write_all(b"evil").unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        assert!(parse_crpack(&bytes).is_err());
    }
}
