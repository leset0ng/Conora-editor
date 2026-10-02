//! Import archives without guessing bindings from their filenames.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use corona_core::{app_icons, crpack, lvgl, project};
use serde_json::{Value, json};

use super::{Failure, Result, absolute, io_failure, json_bytes, path_text, portable_relative};

const MAX_PACK_BYTES: usize = 64 * 1024 * 1024;

#[derive(clap::Args)]
pub(super) struct Import {
    /// CRPack archive to import.
    pack: PathBuf,
    /// New project directory; it must not already exist.
    #[arg(long)]
    into: PathBuf,
    /// Firmware whose resource paths the pack replaces.
    #[arg(long)]
    firmware: PathBuf,
    #[arg(long, default_value = "default")]
    target: String,
    #[arg(long)]
    device: Option<String>,
}

fn refuse_existing(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(Failure::new(
            "destination_exists",
            format!("import destination already exists: {}", path.display()),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_failure(
            "could not inspect import destination",
            path,
            error,
        )),
    }
}

/// Publish a staged directory atomically without replacing even an empty
/// directory or dangling symlink created after the caller's preflight.
#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "linux",
    target_os = "android"
))]
pub(super) fn publish_noclobber(from: &Path, to: &Path) -> Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let from_c = CString::new(from.as_os_str().as_bytes())
        .map_err(|_| Failure::new("path", "staging path contains a NUL byte"))?;
    let to_c = CString::new(to.as_os_str().as_bytes())
        .map_err(|_| Failure::new("path", "destination path contains a NUL byte"))?;
    // SAFETY: Both pointers refer to live NUL-terminated C strings for the
    // duration of the call. The kernel performs the existence check and rename
    // as one operation; it does not follow a destination's final symlink.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    let status = unsafe { libc::renamex_np(from_c.as_ptr(), to_c.as_ptr(), libc::RENAME_EXCL) };
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let status = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from_c.as_ptr(),
            libc::AT_FDCWD,
            to_c.as_ptr(),
            libc::RENAME_NOREPLACE as _,
        )
    };
    if status == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if matches!(
        error.raw_os_error(),
        Some(libc::EEXIST) | Some(libc::ENOTEMPTY)
    ) {
        return Err(Failure::new(
            "destination_exists",
            format!("import destination already exists: {}", to.display()),
        ));
    }
    // In particular, ENOSYS/EINVAL/ENOTSUP must never fall back to a clobbering
    // rename on an older kernel or a filesystem without no-replace support.
    Err(io_failure(
        "could not publish imported project without replacement",
        to,
        error,
    ))
}

#[cfg(windows)]
pub(super) fn publish_noclobber(from: &Path, to: &Path) -> Result<()> {
    refuse_existing(to)?;
    // Windows directory rename refuses an existing destination directory.
    fs::rename(from, to).map_err(|error| {
        io_failure(
            "could not publish imported project without replacement",
            to,
            error,
        )
    })
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "linux",
    target_os = "android",
    windows
)))]
pub(super) fn publish_noclobber(_from: &Path, to: &Path) -> Result<()> {
    refuse_existing(to)?;
    Err(Failure::new(
        "publication_unsupported",
        "atomic no-replace directory publication is not supported on this platform",
    ))
}

fn suffix<'a>(path: &'a str, rule: &str) -> Option<&'a str> {
    if rule.ends_with('/') {
        path.strip_prefix(rule)
    } else if path == rule {
        Some("")
    } else {
        None
    }
}

/// A role is a label, never a resource path. Collisions are resolved in sorted
/// archive order, including collisions with names that already contain suffixes.
fn role_name(path: &str, used: &mut BTreeSet<String>) -> String {
    let stem = Path::new(path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("icon");
    let mut base: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(80)
        .collect();
    if base.is_empty() || base.bytes().all(|c| c == b'_') {
        base = "icon".into();
    }
    let mut role = base.clone();
    let mut index = 2;
    while !used.insert(role.clone()) {
        role = format!("{base}_{index}");
        index += 1;
    }
    role
}

fn write_file(root: &Path, relative: &str, bytes: &[u8]) -> Result<()> {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| io_failure("could not create import directory", parent, error))?;
    }
    fs::write(&path, bytes).map_err(|error| io_failure("could not write import file", &path, error))
}

pub(super) fn run(args: Import) -> Result<Value> {
    // UTF-8 validation precedes any mutation, including creation of parents.
    for path in [&args.pack, &args.into, &args.firmware] {
        path_text(path)?;
    }
    project::validate_target_id(&args.target)?;
    super::validate_device(args.device.as_deref())?;
    refuse_existing(&args.into)?;
    let root = absolute(&args.into)?;
    path_text(&root)?;
    refuse_existing(&root)?;
    let firmware_path = absolute(&args.firmware)?;
    path_text(&firmware_path)?;
    let pack_bytes = project::read_limited(&args.pack, MAX_PACK_BYTES)?;
    let pack = crpack::parse_crpack(&pack_bytes)?;
    eprintln!(
        "Import: loading and fingerprinting firmware {}",
        firmware_path.display()
    );
    let firmware = project::load_firmware(&firmware_path, None)?;
    let mappings: Vec<Value> = pack
        .mappings
        .iter()
        .map(|rule| json!({"source": rule.source, "destination": rule.destination}))
        .collect();
    let mut diagnostics = Vec::new();
    let mut resolved = BTreeMap::new();
    let mut claimed = BTreeSet::new();
    let mut unmapped = Vec::new();
    let mut native = BTreeMap::<String, Vec<String>>::new();
    let mut native_claimed = BTreeSet::new();
    for icon in &pack.quickapp_icons {
        let identity = format!("{}{}", app_icons::QUICKAPP_SOURCE_PREFIX, icon.package);
        if !native_claimed.insert(identity.clone()) {
            return Err(Failure::new(
                "ambiguous_mapping",
                "duplicate QuickApp icon declaration",
            ));
        }
        native
            .entry(icon.destination.clone())
            .or_default()
            .push(identity);
    }

    // The ordered rules are retained, but the project builder only supports
    // resource replacements, not arbitrary filesystem remaps. Never silently
    // turn a /system rule into a guessed /resource binding.
    for rule in &pack.mappings {
        if rule.source == app_icons::CANOPUS_SOURCE
            || rule.source.starts_with(app_icons::QUICKAPP_SOURCE_PREFIX)
        {
            if let Some(package) = rule.source.strip_prefix(app_icons::QUICKAPP_SOURCE_PREFIX) {
                app_icons::validate_package(package)?;
            }
            if rule.destination.ends_with('/') || !rule.destination.ends_with(".bin") {
                return Err(Failure::new(
                    "nonportable_mapping",
                    "native icon mapping must name a .bin file",
                ));
            }
            if !native_claimed.insert(rule.source.clone()) {
                return Err(Failure::new(
                    "ambiguous_mapping",
                    "duplicate native icon mapping",
                ));
            }
            native
                .entry(rule.destination.clone())
                .or_default()
                .push(rule.source.clone());
            continue;
        }
        if !rule.source.starts_with("/resource/") {
            return Err(Failure::new(
                "nonportable_mapping",
                format!(
                    "mapping source {} is outside /resource/; this project format cannot preserve its semantics",
                    rule.source
                ),
            ));
        }
        if rule.source != format!("/resource/{}", rule.destination) {
            diagnostics.push(json!({
                "code": "mapping_normalized",
                "message": "Builds use firmware resource paths instead of the original archive rename; the original ordered rule is preserved in source/corona.json.",
                "source": rule.source, "destination": rule.destination
            }));
        }
    }
    for path in pack.replacements.keys() {
        if native.contains_key(path) {
            // Sharing one archive BIN among native consumers is unambiguous.
            // A simultaneous ordinary firmware rule has separate resource-path
            // semantics, which this importer must not silently discard.
            if pack.mappings.iter().any(|rule| {
                rule.source.starts_with("/resource/") && suffix(path, &rule.destination).is_some()
            }) {
                return Err(Failure::new(
                    "ambiguous_mapping",
                    "native icon overlaps an ordinary firmware mapping destination",
                ));
            }
            continue;
        }
        let matches: Vec<_> = pack
            .mappings
            .iter()
            .filter_map(|rule| suffix(path, &rule.destination).map(|tail| (rule, tail)))
            .collect();
        if matches.is_empty() {
            unmapped.push(path.clone());
            diagnostics.push(json!({
                "code": "unmapped_resource", "resource": path,
                "message": "Preserved in source/raw but not bound; builds do not include this ordinary archive file."
            }));
            continue;
        }
        if matches.len() != 1 {
            return Err(Failure::new(
                "ambiguous_mapping",
                format!(
                    "archive file {path} matches multiple ordered mapping destinations; cannot import faithfully"
                ),
            ));
        }
        let (rule, tail) = matches[0];
        let source = format!("{}{tail}", rule.source);
        // Overlapping source prefixes also have order-dependent semantics.
        if pack
            .mappings
            .iter()
            .filter(|rule| suffix(&source, &rule.source).is_some())
            .count()
            != 1
        {
            return Err(Failure::new(
                "ambiguous_mapping",
                format!(
                    "firmware source {source} matches multiple ordered mapping sources; cannot import faithfully"
                ),
            ));
        }
        let resource = source
            .strip_prefix("/resource/")
            .expect("validated mapping source");
        crpack::validate_relative_path(resource)?;
        if firmware.index.file(resource).is_none() {
            return Err(Failure::new(
                "missing_binding",
                format!(
                    "mapping for {path} resolves to {resource}, which does not exist in the supplied firmware"
                ),
            ));
        }
        if !claimed.insert(resource.to_owned()) {
            return Err(Failure::new(
                "ambiguous_mapping",
                format!("multiple archive files resolve to firmware resource {resource}"),
            ));
        }
        resolved.insert(path.clone(), resource.to_owned());
    }
    if resolved.is_empty() && native.is_empty() {
        return Err(Failure::new(
            "missing_binding",
            "pack has no resource mappings that can become project bindings",
        ));
    }

    eprintln!(
        "Import: resolved {} mapped assets; {} unmapped files preserved",
        resolved.len(),
        unmapped.len()
    );
    let parent = root
        .parent()
        .ok_or_else(|| Failure::new("path", "import destination has no parent"))?;
    let target_path = root.join("targets").join(format!("{}.json", args.target));
    let firmware_relative = portable_relative(&firmware_path, target_path.parent().unwrap())?;
    fs::create_dir_all(parent)
        .map_err(|error| io_failure("could not create import parent", parent, error))?;
    let stage = tempfile::Builder::new()
        .prefix(".corona-import-")
        .tempdir_in(parent)
        .map_err(|error| io_failure("could not stage import", parent, error))?;
    let stage_root = stage.path();
    write_file(stage_root, "source/original.crpack", &pack_bytes)?;
    write_file(stage_root, "source/corona.json", &pack.manifest_bytes)?;
    let mut icons = BTreeMap::new();
    let mut quickapp_icons = BTreeMap::new();
    let mut canopus_icon = None;
    let mut bindings = BTreeMap::new();
    let mut overrides = BTreeMap::new();
    let mut used = BTreeSet::new();
    let mut resources = Vec::new();
    for (path, bytes) in &pack.replacements {
        let raw_path = format!("source/raw/{path}");
        write_file(stage_root, &raw_path, bytes)?;
        if let Some(identities) = native.get(path) {
            // Native slots have no ROMFS binding. Multiple consumers safely
            // share the original validated BIN in the project-owned raw tree.
            app_icons::inspect_bin(bytes)?;
            for identity in identities {
                let asset = json!({"input":raw_path, "mode":"raw"});
                if identity == app_icons::CANOPUS_SOURCE {
                    canopus_icon = Some(asset);
                } else {
                    quickapp_icons.insert(
                        identity
                            .strip_prefix(app_icons::QUICKAPP_SOURCE_PREFIX)
                            .unwrap()
                            .to_owned(),
                        asset,
                    );
                }
                resources.push(
                    json!({"archivePath":path, "resource":identity, "mode":"raw", "native":true}),
                );
            }
            continue;
        }
        let Some(resource) = resolved.get(path) else {
            continue;
        };
        let role = role_name(path, &mut used);
        // Core decoding bounds dimensions to 16 megapixels before allocating.
        // Archive decompression and each generated PNG also have a 64 MiB cap.
        let decoded = lvgl::decode_image_png(bytes);
        let (asset, mode) = match decoded {
            Ok((_, png)) if png.len() <= MAX_PACK_BYTES => {
                let png_path = format!("assets/{role}.png");
                write_file(stage_root, &png_path, &png)?;
                // The original target is deliberately raw: conversion against a
                // different firmware template cannot certify byte identity.
                overrides.insert(role.clone(), json!({"input": raw_path, "mode": "raw"}));
                (json!(png_path), "png")
            }
            result => {
                let message = match result {
                    Err(error) => error,
                    Ok(_) => "decoded PNG exceeds the 64 MiB asset limit".into(),
                };
                diagnostics.push(json!({"code": "raw_fallback", "role": role, "resource": resource, "message": message}));
                (json!({"input": raw_path, "mode": "raw"}), "raw")
            }
        };
        icons.insert(role.clone(), asset);
        bindings.insert(role.clone(), resource.clone());
        resources
            .push(json!({"role": role, "archivePath": path, "resource": resource, "mode": mode}));
        if resources.len().is_multiple_of(8) {
            eprintln!(
                "Import: staged {}/{} assets",
                resources.len(),
                resolved.len()
            );
        }
    }
    diagnostics.push(json!({
        "code": "original_target_raw",
        "message": "The imported target copies original resource bytes in raw mode (device format unverified). Shared PNGs are editable; remove a role's target override to build its edited PNG. Original-target builds preserve mapped resource bytes, not the ZIP container, unmapped files, or arbitrary mapping layout."
    }));
    let mut theme = json!({
        "schemaVersion": 1, "themeId": pack.theme_id, "name": pack.name, "icons": icons
    });
    if let Some(asset) = canopus_icon {
        theme["canopusIcon"] = asset;
    }
    if !quickapp_icons.is_empty() {
        theme["quickappIcons"] = json!(quickapp_icons);
    }
    for (key, value) in [
        ("version", &pack.version),
        ("author", &pack.author),
        ("description", &pack.description),
    ] {
        if let Some(value) = value {
            theme[key] = json!(value);
        }
    }
    if let Some(version_code) = pack.version_code {
        theme["versionCode"] = json!(version_code);
    }
    let mut target = json!({
        "schemaVersion": 1, "firmware": firmware_relative,
        "firmwareSha256": firmware.sha256, "bindings": bindings, "overrides": overrides
    });
    if let Some(device) = args.device {
        target["device"] = json!(device);
    }
    write_file(stage_root, "theme.json", &json_bytes(&theme)?)?;
    write_file(
        stage_root,
        &format!("targets/{}.json", args.target),
        &json_bytes(&target)?,
    )?;
    for directory in ["assets", "previews"] {
        let path = stage_root.join(directory);
        fs::create_dir_all(&path)
            .map_err(|error| io_failure("could not create import directory", &path, error))?;
    }
    let quickapp_declarations: Vec<_> = pack
        .quickapp_icons
        .iter()
        .map(|icon| json!({"package":icon.package,"destination":icon.destination}))
        .collect();
    let report = json!({"quickappIcons":quickapp_declarations,"mappings": mappings, "unmapped": unmapped, "resources": resources, "diagnostics": diagnostics});
    write_file(stage_root, "source/import.json", &json_bytes(&report)?)?;

    // Validate the staged configuration and all assets before publishing. The
    // relative firmware path is interpreted against the final location, not the
    // temporary directory name (both have the same parent and depth).
    eprintln!(
        "Import: validating {} staged assets before publication",
        resources.len()
    );
    let staged_project = project::load_theme(stage_root)?;
    let prepared = project::prepare_target(&staged_project, &args.target);
    if !prepared.report.valid {
        return Err(Failure {
            code: "validation",
            message: "imported project failed validation; destination was not published".into(),
            details: json!({"targets": [prepared.report], "import": report}),
        });
    }
    // Inspect the original lexical path too: canonicalization must not hide a
    // dangling destination symlink that appeared while assets were staged.
    refuse_existing(&args.into)?;
    refuse_existing(&root)?;
    publish_noclobber(stage_root, &root)?;
    Ok(json!({
        "theme": root.join("theme.json"), "target": args.target, "firmwareSha256": firmware.sha256,
        "mappings": mappings, "quickappIcons":quickapp_declarations,
        "unmapped": unmapped, "resources": resources, "diagnostics": diagnostics
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publication_does_not_replace_a_foreign_empty_directory_after_preflight() {
        let root = tempfile::tempdir().unwrap();
        let staged = root.path().join("staged");
        let destination = root.path().join("destination");
        fs::create_dir(&staged).unwrap();
        fs::write(staged.join("theme.json"), b"staged project").unwrap();
        assert!(refuse_existing(&destination).is_ok());
        // Simulate another process creating an empty directory after preflight.
        fs::create_dir(&destination).unwrap();
        #[cfg(unix)]
        let original_inode = {
            use std::os::unix::fs::MetadataExt;
            fs::metadata(&destination).unwrap().ino()
        };
        assert!(publish_noclobber(&staged, &destination).is_err());
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
        assert_eq!(
            fs::read(staged.join("theme.json")).unwrap(),
            b"staged project"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(fs::metadata(&destination).unwrap().ino(), original_inode);
        }
    }

    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "linux",
        target_os = "android",
        windows
    ))]
    #[test]
    fn publication_moves_a_directory_to_an_absent_destination() {
        let root = tempfile::tempdir().unwrap();
        let staged = root.path().join("staged");
        let destination = root.path().join("destination");
        fs::create_dir(&staged).unwrap();
        fs::write(staged.join("theme.json"), b"complete project").unwrap();
        publish_noclobber(&staged, &destination)
            .unwrap_or_else(|error| panic!("{}", error.message));
        assert!(!staged.exists());
        assert_eq!(
            fs::read(destination.join("theme.json")).unwrap(),
            b"complete project"
        );
    }

    #[cfg(unix)]
    #[test]
    fn publication_does_not_replace_a_racing_dangling_symlink() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let staged = root.path().join("staged");
        let destination = root.path().join("destination");
        fs::create_dir(&staged).unwrap();
        assert!(refuse_existing(&destination).is_ok());
        symlink("missing", &destination).unwrap();
        assert!(publish_noclobber(&staged, &destination).is_err());
        assert_eq!(fs::read_link(&destination).unwrap(), Path::new("missing"));
        assert!(staged.is_dir());
        assert!(!root.path().join("missing").exists());
    }
}
