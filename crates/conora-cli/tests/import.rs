use std::collections::BTreeMap;
use std::fs;
use std::io::{Cursor, Write};
use std::path::Path;
use std::process::{Command, Output};

use conora_core::{crpack, lvgl};
use serde_json::{Value, json};
use zip::write::SimpleFileOptions;

fn run(root: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_conora"))
        .current_dir(root)
        .args(arguments)
        .output()
        .unwrap()
}

fn result(output: Output, ok: bool) -> Value {
    assert_eq!(
        output.status.success(),
        ok,
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schemaVersion"], 1);
    assert_eq!(value["ok"], ok);
    value
}

fn import(root: &Path, into: &str) -> Value {
    result(
        run(
            root,
            &[
                "import",
                "input.crpack",
                "--into",
                into,
                "--firmware",
                "watch.bin",
                "--json",
            ],
        ),
        true,
    )
}

fn rejected(root: &Path) -> Value {
    let value = result(
        run(
            root,
            &[
                "import",
                "input.crpack",
                "--into",
                "theme",
                "--firmware",
                "watch.bin",
                "--json",
            ],
        ),
        false,
    );
    assert!(!root.join("theme").exists());
    assert!(!fs::read_dir(root).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".conora-import-")
    }));
    value
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn template(size: u16, cf: u8) -> Vec<u8> {
    let stride = if cf == 0x10 { size * 4 } else { size };
    let palette = if cf == 0x0a { 1024 } else { 0 };
    let mut bytes = vec![0; 12 + palette + usize::from(stride) * usize::from(size)];
    bytes[0] = 0x19;
    bytes[1] = cf;
    bytes[4..6].copy_from_slice(&size.to_le_bytes());
    bytes[6..8].copy_from_slice(&size.to_le_bytes());
    bytes[8..10].copy_from_slice(&stride.to_le_bytes());
    if palette > 0 {
        bytes[12..16].copy_from_slice(&[0, 0, 255, 255]);
    }
    bytes
}

/// Local, synthetic raw ROMFS; app/icons contains arbitrarily many image files.
fn firmware(files: &BTreeMap<String, Vec<u8>>) -> Vec<u8> {
    let mut bytes = vec![0; 96];
    bytes[..8].copy_from_slice(b"-rom1fs-");
    bytes[16..24].copy_from_slice(b"resource");
    bytes[32..36].copy_from_slice(&1u32.to_be_bytes());
    bytes[36..40].copy_from_slice(&64u32.to_be_bytes());
    bytes[48..51].copy_from_slice(b"app");
    bytes[64..68].copy_from_slice(&1u32.to_be_bytes());
    bytes[68..72].copy_from_slice(&96u32.to_be_bytes());
    bytes[80..85].copy_from_slice(b"icons");
    for (index, (path, contents)) in files.iter().enumerate() {
        let name = path.strip_prefix("app/icons/").unwrap().as_bytes();
        let start = bytes.len();
        let header = (16 + name.len() + 1).next_multiple_of(16);
        let end = (start + header + contents.len()).next_multiple_of(16);
        bytes.resize(end, 0);
        let next = if index + 1 == files.len() {
            2
        } else {
            end as u32 | 2
        };
        bytes[start..start + 4].copy_from_slice(&next.to_be_bytes());
        bytes[start + 8..start + 12].copy_from_slice(&(contents.len() as u32).to_be_bytes());
        bytes[start + 16..start + 16 + name.len()].copy_from_slice(name);
        bytes[start + header..start + header + contents.len()].copy_from_slice(contents);
    }
    let length = bytes.len() as u32;
    bytes[8..12].copy_from_slice(&length.to_be_bytes());
    bytes
}

fn manifest(mappings: Value) -> Value {
    json!({
        "format": "canopus-resource-pack", "formatVersion": 1,
        "themeId": "fictional", "name": "Fictional Icons", "version": "1.2",
        "author": "Local Fixture", "description": "Not watch firmware",
        "targets": ["fictional-watch", "fictional-watch-2"],
        "extra": {"preserve": true}, "mappings": mappings
    })
}

fn archive(manifest_bytes: &[u8], files: &BTreeMap<String, Vec<u8>>) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    writer.start_file("canora.json", options).unwrap();
    writer.write_all(manifest_bytes).unwrap();
    for (path, bytes) in files {
        writer.start_file(path, options).unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn fixture(
    files: &BTreeMap<String, Vec<u8>>,
    pack_files: &BTreeMap<String, Vec<u8>>,
    manifest: &Value,
) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("watch.bin"), firmware(files)).unwrap();
    fs::write(
        root.path().join("input.crpack"),
        archive(&serde_json::to_vec_pretty(manifest).unwrap(), pack_files),
    )
    .unwrap();
    root
}

fn standard_manifest() -> Value {
    manifest(json!([{"source": "/resource/app/", "destination": "app/"}]))
}

#[test]
fn fictional_two_image_and_all_forty_image_roundtrip_preserve_originals() {
    for count in [2, 40] {
        let files: BTreeMap<_, _> = (0..count)
            .map(|i| {
                (
                    format!("app/icons/icon{i:02}.bin"),
                    template(2, if i % 2 == 0 { 0x0a } else { 0x10 }),
                )
            })
            .collect();
        let root = fixture(&files, &files, &standard_manifest());
        let original = fs::read(root.path().join("input.crpack")).unwrap();
        let parsed = crpack::parse_crpack(&original).unwrap();
        let report = import(root.path(), "theme");
        assert_eq!(report["resources"].as_array().unwrap().len(), count);
        assert_eq!(report["mappings"], standard_manifest()["mappings"]);
        assert_eq!(
            fs::read(root.path().join("theme/source/original.crpack")).unwrap(),
            original
        );
        assert_eq!(
            fs::read(root.path().join("theme/source/canora.json")).unwrap(),
            parsed.manifest_bytes
        );
        let theme = read_json(&root.path().join("theme/theme.json"));
        for key in ["themeId", "name", "version", "author", "description"] {
            assert_eq!(theme[key], standard_manifest()[key]);
        }
        let target = read_json(&root.path().join("theme/targets/default.json"));
        assert_eq!(target["firmwareSha256"].as_str().unwrap().len(), 64);
        assert_eq!(target["bindings"].as_object().unwrap().len(), count);
        assert_eq!(target["overrides"].as_object().unwrap().len(), count);
        assert!(target.get("resourceHashes").is_none());
        for asset in theme["icons"].as_object().unwrap().values() {
            let bytes = fs::read(root.path().join("theme").join(asset.as_str().unwrap())).unwrap();
            assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        }
        let checked = result(
            run(
                root.path(),
                &["check", "--theme", "theme", "--target", "default", "--json"],
            ),
            true,
        );
        assert_eq!(
            checked["targets"][0]["resources"].as_array().unwrap().len(),
            count
        );
        result(
            run(
                root.path(),
                &["build", "--theme", "theme", "--target", "default", "--json"],
            ),
            true,
        );
        let rebuilt = crpack::parse_crpack(
            &fs::read(root.path().join("theme/dist/fictional-default.crpack")).unwrap(),
        )
        .unwrap();
        assert_eq!(rebuilt.replacements, files);
        assert_eq!(rebuilt.name, parsed.name);
        assert_eq!(rebuilt.author, parsed.author);
    }
}

#[test]
fn raw_fallback_and_unmapped_files_are_explicit_not_guessed() {
    let files = BTreeMap::from([("app/icons/test.bin".into(), template(2, 0x10))]);
    let pack_files = BTreeMap::from([
        ("app/icons/test.bin".into(), b"not an image".to_vec()),
        ("notes/readme.txt".into(), b"keep me".to_vec()),
    ]);
    let root = fixture(&files, &pack_files, &standard_manifest());
    let report = import(root.path(), "theme");
    assert_eq!(report["unmapped"], json!(["notes/readme.txt"]));
    assert!(
        report["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "raw_fallback")
    );
    let theme = read_json(&root.path().join("theme/theme.json"));
    assert_eq!(theme["icons"]["test"]["mode"], "raw");
    assert_eq!(theme["icons"].as_object().unwrap().len(), 1);
    assert_eq!(
        fs::read(root.path().join("theme/source/raw/notes/readme.txt")).unwrap(),
        b"keep me"
    );
    result(
        run(
            root.path(),
            &["build", "--theme", "theme", "--target", "default", "--json"],
        ),
        true,
    );
    let rebuilt = crpack::parse_crpack(
        &fs::read(root.path().join("theme/dist/fictional-default.crpack")).unwrap(),
    )
    .unwrap();
    assert_eq!(rebuilt.replacements["app/icons/test.bin"], b"not an image");
    assert!(!rebuilt.replacements.contains_key("notes/readme.txt"));
}

#[test]
fn exact_and_directory_renames_resolve_both_sides_and_names_are_unique() {
    let files = BTreeMap::from([
        ("app/icons/one.bin".into(), template(2, 0x10)),
        ("app/icons/two.bin".into(), template(2, 0x10)),
    ]);
    let pack_files = BTreeMap::from([
        ("alternate/test.bin".into(), template(2, 0x10)),
        ("renamed/test.bin".into(), template(2, 0x10)),
    ]);
    let metadata = manifest(json!([
        {"source": "/resource/app/icons/two.bin", "destination": "renamed/test.bin"},
        {"source": "/resource/app/icons/one.bin", "destination": "alternate/test.bin"}
    ]));
    let root = fixture(&files, &pack_files, &metadata);
    let report = import(root.path(), "theme");
    assert_eq!(report["mappings"], metadata["mappings"]); // original order, not lexical sort
    let target = read_json(&root.path().join("theme/targets/default.json"));
    assert_eq!(
        target["bindings"],
        json!({"test": "app/icons/one.bin", "test_2": "app/icons/two.bin"})
    );
    result(
        run(
            root.path(),
            &["build", "--theme", "theme", "--target", "default", "--json"],
        ),
        true,
    );
    let rebuilt = crpack::parse_crpack(
        &fs::read(root.path().join("theme/dist/fictional-default.crpack")).unwrap(),
    )
    .unwrap();
    assert_eq!(rebuilt.replacements, files);

    let pack_files = BTreeMap::from([("different/one.bin".into(), template(2, 0x10))]);
    let metadata =
        manifest(json!([{"source": "/resource/app/icons/", "destination": "different/"}]));
    let root = fixture(&files, &pack_files, &metadata);
    import(root.path(), "theme");
    assert_eq!(
        read_json(&root.path().join("theme/targets/default.json"))["bindings"]["one"],
        "app/icons/one.bin"
    );
}

#[test]
fn png_edits_can_opt_out_of_original_target_raw_override() {
    let files = BTreeMap::from([("app/icons/test.bin".into(), template(2, 0x10))]);
    let root = fixture(&files, &files, &standard_manifest());
    import(root.path(), "theme");
    let mut changed = template(2, 0x10);
    changed[12..16].copy_from_slice(&[0, 0, 255, 255]);
    let (_, png) = lvgl::decode_image_png(&changed).unwrap();
    fs::write(root.path().join("theme/assets/test.png"), png).unwrap();
    let path = root.path().join("theme/targets/default.json");
    let mut target = read_json(&path);
    target["overrides"] = json!({});
    fs::write(path, serde_json::to_vec(&target).unwrap()).unwrap();
    result(
        run(
            root.path(),
            &["build", "--theme", "theme", "--target", "default", "--json"],
        ),
        true,
    );
    let rebuilt = crpack::parse_crpack(
        &fs::read(root.path().join("theme/dist/fictional-default.crpack")).unwrap(),
    )
    .unwrap();
    assert_eq!(rebuilt.replacements["app/icons/test.bin"], changed);
}

#[test]
fn missing_binding_and_nonportable_or_ambiguous_rules_fail_before_publication() {
    let files = BTreeMap::from([("app/icons/test.bin".into(), template(2, 0x10))]);
    for (rules, code) in [
        (
            json!([{"source": "/resource/app/icons/missing.bin", "destination": "app/icons/test.bin"}]),
            "missing_binding",
        ),
        (
            json!([{"source": "/system/icons/test.bin", "destination": "app/icons/test.bin"}]),
            "nonportable_mapping",
        ),
        (
            json!([
                {"source": "/resource/app/", "destination": "app/"},
                {"source": "/resource/app/icons/test.bin", "destination": "app/icons/test.bin"}
            ]),
            "ambiguous_mapping",
        ),
        (json!([]), "missing_binding"),
    ] {
        let root = fixture(&files, &files, &manifest(rules));
        assert_eq!(rejected(root.path())["errors"][0]["code"], code);
    }
}

#[test]
fn existing_destination_is_never_modified_even_if_empty_or_without_theme() {
    let files = BTreeMap::from([("app/icons/test.bin".into(), template(2, 0x10))]);
    let root = fixture(&files, &files, &standard_manifest());
    fs::create_dir(root.path().join("theme")).unwrap();
    let arguments = [
        "import",
        "input.crpack",
        "--into",
        "theme",
        "--firmware",
        "watch.bin",
        "--json",
    ];
    assert_eq!(
        result(run(root.path(), &arguments), false)["errors"][0]["code"],
        "destination_exists"
    );
    assert_eq!(fs::read_dir(root.path().join("theme")).unwrap().count(), 0);
    fs::write(root.path().join("theme/keep.txt"), b"keep").unwrap();
    result(run(root.path(), &arguments), false);
    assert_eq!(
        fs::read(root.path().join("theme/keep.txt")).unwrap(),
        b"keep"
    );
}

#[test]
fn invalid_archive_metadata_traversal_and_input_size_limits_are_rejected() {
    let files = BTreeMap::from([("app/icons/test.bin".into(), template(2, 0x10))]);
    let mut metadata = standard_manifest();
    metadata["themeId"] = json!("INVALID");
    let root = fixture(&files, &files, &metadata);
    rejected(root.path());

    let unsafe_files = BTreeMap::from([("../outside.bin".into(), vec![1])]);
    let root = fixture(&files, &unsafe_files, &manifest(json!([])));
    rejected(root.path());
    assert!(!root.path().parent().unwrap().join("outside.bin").exists());

    for (name, limit) in [
        ("input.crpack", 64 * 1024 * 1024),
        ("watch.bin", 512 * 1024 * 1024),
    ] {
        let root = fixture(&files, &files, &standard_manifest());
        fs::OpenOptions::new()
            .write(true)
            .open(root.path().join(name))
            .unwrap()
            .set_len(limit + 1)
            .unwrap();
        rejected(root.path());
    }
}

#[cfg(unix)]
#[test]
fn non_utf8_paths_special_inputs_and_dangling_destination_symlinks_are_rejected() {
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::symlink;
    let files = BTreeMap::from([("app/icons/test.bin".into(), template(2, 0x10))]);
    let root = fixture(&files, &files, &standard_manifest());
    let bad = root
        .path()
        .join(std::ffi::OsString::from_vec(b"theme-\xff".to_vec()));
    let output = Command::new(env!("CARGO_BIN_EXE_conora"))
        .current_dir(root.path())
        .args([
            "import",
            "input.crpack",
            "--firmware",
            "watch.bin",
            "--into",
        ])
        .arg(&bad)
        .arg("--json")
        .output()
        .unwrap();
    result(output, false);
    assert!(!bad.exists());
    symlink("missing", root.path().join("theme")).unwrap();
    result(
        run(
            root.path(),
            &[
                "import",
                "input.crpack",
                "--into",
                "theme",
                "--firmware",
                "watch.bin",
                "--json",
            ],
        ),
        false,
    );
    assert!(
        fs::symlink_metadata(root.path().join("theme"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    fs::remove_file(root.path().join("theme")).unwrap();
    for name in ["watch.bin", "input.crpack"] {
        fs::remove_file(root.path().join(name)).unwrap();
        assert!(
            Command::new("mkfifo")
                .arg(root.path().join(name))
                .status()
                .unwrap()
                .success()
        );
        rejected(root.path()); // must not block opening the FIFO
        fs::remove_file(root.path().join(name)).unwrap();
        if name == "watch.bin" {
            fs::write(root.path().join(name), firmware(&files)).unwrap();
        }
    }
}

#[test]
fn zip_non_utf8_names_and_special_entries_are_rejected_by_strict_parser() {
    let files = BTreeMap::from([("app/icons/test.bin".into(), template(2, 0x10))]);
    for special in [false, true] {
        let root = fixture(&files, &files, &standard_manifest());
        let path = root.path().join("input.crpack");
        let mut bytes = fs::read(&path).unwrap();
        if special {
            let central = bytes.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
            bytes[central + 5] = 3; // Unix creator
            bytes[central + 38..central + 42].copy_from_slice(&(0o010644u32 << 16).to_le_bytes());
        } else {
            let name = b"app/icons/test.bin";
            let locations: Vec<_> = bytes
                .windows(name.len())
                .enumerate()
                .filter_map(|(i, w)| (w == name).then_some(i))
                .collect();
            for location in locations {
                bytes[location] = 0xff;
            }
        }
        fs::write(path, bytes).unwrap();
        rejected(root.path());
    }
}

#[test]
fn imported_target_accepts_explicit_dotted_id_device_and_firmware_pin() {
    let files = BTreeMap::from([("app/icons/test.bin".into(), template(2, 0x10))]);
    let root = fixture(&files, &files, &standard_manifest());
    let report = result(
        run(
            root.path(),
            &[
                "import",
                "input.crpack",
                "--into",
                "nested/theme",
                "--firmware",
                "watch.bin",
                "--target",
                "p67.v1",
                "--device",
                "Fictional Watch",
                "--json",
            ],
        ),
        true,
    );
    let config = read_json(&root.path().join("nested/theme/targets/p67.v1.json"));
    let loaded = conora_core::project::load_firmware(&root.path().join("watch.bin"), None).unwrap();
    assert_eq!(config["firmwareSha256"], loaded.sha256);
    assert_eq!(report["firmwareSha256"], config["firmwareSha256"]);
    assert_eq!(config["device"], "Fictional Watch");
    result(
        run(
            root.path(),
            &[
                "check",
                "--theme",
                "nested/theme",
                "--target",
                "p67.v1",
                "--json",
            ],
        ),
        true,
    );
    let mut modified = firmware(&files);
    modified[28] ^= 1;
    fs::write(root.path().join("watch.bin"), modified).unwrap();
    result(
        run(
            root.path(),
            &[
                "check",
                "--theme",
                "nested/theme",
                "--target",
                "p67.v1",
                "--json",
            ],
        ),
        false,
    );
}

#[test]
fn excessive_image_dimensions_fall_back_to_bounded_raw_asset() {
    let files = BTreeMap::from([("app/icons/test.bin".into(), template(2, 0x10))]);
    let mut oversized = template(2, 0x10);
    oversized[4..6].copy_from_slice(&4097u16.to_le_bytes());
    oversized[6..8].copy_from_slice(&4097u16.to_le_bytes());
    let pack_files = BTreeMap::from([("app/icons/test.bin".into(), oversized)]);
    let root = fixture(&files, &pack_files, &standard_manifest());
    let report = import(root.path(), "theme");
    assert_eq!(report["resources"][0]["mode"], "raw");
    assert!(
        report["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| diagnostic["code"] == "raw_fallback")
    );
}

#[test]
fn malicious_rle_expansion_uses_raw_fallback_without_allocating_declared_size() {
    let files = BTreeMap::from([("app/icons/test.bin".into(), template(2, 0x10))]);
    let mut malicious = template(2, 0x10);
    malicious.resize(26, 0);
    malicious[2..4].copy_from_slice(&8u16.to_le_bytes());
    malicious[16..20].copy_from_slice(&2u32.to_le_bytes());
    malicious[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
    malicious[24..26].copy_from_slice(&[1, 255]);
    let pack_files = BTreeMap::from([("app/icons/test.bin".into(), malicious.clone())]);
    let root = fixture(&files, &pack_files, &standard_manifest());
    let report = import(root.path(), "theme");
    assert_eq!(report["resources"][0]["mode"], "raw");
    assert!(
        report["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| {
                diagnostic["code"] == "raw_fallback"
                    && diagnostic["message"]
                        .as_str()
                        .unwrap()
                        .contains("RLE expanded payload")
            })
    );
    assert_eq!(
        fs::read(root.path().join("theme/source/raw/app/icons/test.bin")).unwrap(),
        malicious
    );
}

#[test]
fn import_progress_is_throttled_and_does_not_corrupt_json_stdout() {
    let files: BTreeMap<_, _> = (0..17)
        .map(|index| (format!("app/icons/icon{index:02}.bin"), template(2, 0x10)))
        .collect();
    let root = fixture(&files, &files, &standard_manifest());
    let output = run(
        root.path(),
        &[
            "import",
            "input.crpack",
            "--into",
            "theme",
            "--firmware",
            "watch.bin",
            "--json",
        ],
    );
    let stderr = String::from_utf8(output.stderr.clone()).unwrap();
    result(output, true);
    assert!(stderr.contains("loading and fingerprinting firmware"));
    assert!(stderr.contains("resolved 17 mapped assets"));
    assert!(stderr.contains("staged 8/17 assets"));
    assert!(stderr.contains("staged 16/17 assets"));
    assert!(stderr.contains("validating 17 staged assets"));
    assert_eq!(
        stderr
            .lines()
            .filter(|line| line.starts_with("Import:"))
            .count(),
        5
    );
}

#[test]
fn native_only_and_mixed_pack_import_preserves_canopus_and_quickapp_bytes() {
    for mixed in [false, true] {
        for normalized in [false, true] {
            let image = template(2, 0x10);
            let firmware_files = BTreeMap::from([("app/icons/stock.bin".into(), image.clone())]);
            let mut replacements = BTreeMap::from([
                ("native/canopus.bin".into(), image.clone()),
                ("native/quickapp.bin".into(), image.clone()),
            ]);
            let mut mappings = vec![
                json!({"source":"/data/canopus/manager_icon.bin", "destination":"native/canopus.bin"}),
            ];
            if mixed {
                replacements.insert("app/icons/stock.bin".into(), image.clone());
                mappings.push(json!({"source":"/resource/app/", "destination":"app/"}));
            }
            if normalized {
                mappings.push(json!({"source":"@quickapp-icon/org.example.app", "destination":"native/quickapp.bin"}));
            }
            let mut original_manifest = manifest(json!(mappings));
            if !normalized {
                original_manifest["quickappIcons"] =
                    json!([{"package":"org.example.app", "destination":"native/quickapp.bin"}]);
            }
            let root = fixture(&firmware_files, &replacements, &original_manifest);
            let imported = import(root.path(), "theme");
            assert_eq!(
                imported["resources"].as_array().unwrap().len(),
                if mixed { 3 } else { 2 }
            );
            assert_eq!(imported["unmapped"], json!([]));
            let theme = read_json(&root.path().join("theme/theme.json"));
            assert_eq!(theme["canopusIcon"]["mode"], "raw");
            assert_eq!(theme["quickappIcons"]["org.example.app"]["mode"], "raw");
            let built = result(
                run(
                    root.path(),
                    &["build", "--theme", "theme", "--target", "default", "--json"],
                ),
                true,
            );
            let pack = crpack::parse_crpack(
                &fs::read(root.path().join(built["outputs"][0].as_str().unwrap())).unwrap(),
            )
            .unwrap();
            assert_eq!(pack.replacements["canopus/manager_icon.bin"], image);
            assert_eq!(
                pack.replacements[&conora_core::app_icons::destination("org.example.app")],
                image
            );
            assert_eq!(pack.quickapp_icons.len(), 1);
            assert_eq!(pack.quickapp_icons[0].package, "org.example.app");
            result(
                run(
                    root.path(),
                    &[
                        "preview", "--theme", "theme", "--target", "default", "--verify", "--json",
                    ],
                ),
                true,
            );
        }
    }
}

#[test]
fn quickapp_declaration_overlapping_a_firmware_mapping_is_not_silently_dropped() {
    let files = BTreeMap::from([("app/icons/stock.bin".into(), template(2, 0x10))]);
    let mut original_manifest = standard_manifest();
    original_manifest["quickappIcons"] =
        json!([{"package":"org.example.app", "destination":"app/icons/stock.bin"}]);
    let root = fixture(&files, &files, &original_manifest);
    let failure = rejected(root.path());
    assert_eq!(failure["errors"][0]["code"], "ambiguous_mapping");
}

#[test]
fn native_consumers_can_share_one_original_bin_without_dropping_declarations() {
    for canopus in [false, true] {
        for normalized in [false, true] {
            let image = template(2, 0x10);
            let firmware_files = BTreeMap::from([("app/icons/stock.bin".into(), image.clone())]);
            let replacements = BTreeMap::from([("native/shared.bin".into(), image.clone())]);
            let mut mappings = Vec::new();
            if canopus {
                mappings.push(json!({"source":"/data/canopus/manager_icon.bin", "destination":"native/shared.bin"}));
            }
            if normalized {
                mappings.push(json!({"source":"@quickapp-icon/org.example.first", "destination":"native/shared.bin"}));
            }
            let mut original_manifest = manifest(json!(mappings));
            let mut declarations =
                vec![json!({"package":"org.example.second", "destination":"native/shared.bin"})];
            if !normalized {
                declarations.push(
                    json!({"package":"org.example.first", "destination":"native/shared.bin"}),
                );
            }
            original_manifest["quickappIcons"] = json!(declarations);
            let root = fixture(&firmware_files, &replacements, &original_manifest);
            let imported = import(root.path(), "theme");
            assert_eq!(
                imported["resources"].as_array().unwrap().len(),
                if canopus { 3 } else { 2 }
            );
            assert_eq!(imported["unmapped"], json!([]));
            let theme = read_json(&root.path().join("theme/theme.json"));
            let first = &theme["quickappIcons"]["org.example.first"];
            assert_eq!(first["mode"], "raw");
            assert_eq!(theme["quickappIcons"]["org.example.second"], *first);
            if canopus {
                assert_eq!(theme["canopusIcon"], *first);
            }
            assert_eq!(
                fs::read(
                    root.path()
                        .join("theme")
                        .join(first["input"].as_str().unwrap())
                )
                .unwrap(),
                image
            );
            let built = result(
                run(
                    root.path(),
                    &["build", "--theme", "theme", "--target", "default", "--json"],
                ),
                true,
            );
            let pack = crpack::parse_crpack(
                &fs::read(root.path().join(built["outputs"][0].as_str().unwrap())).unwrap(),
            )
            .unwrap();
            assert_eq!(pack.quickapp_icons.len(), 2);
            for icon in &pack.quickapp_icons {
                assert_eq!(pack.replacements[&icon.destination], image);
            }
            if canopus {
                assert_eq!(pack.replacements["canopus/manager_icon.bin"], image);
            }
            result(
                run(
                    root.path(),
                    &[
                        "preview", "--theme", "theme", "--target", "default", "--verify", "--json",
                    ],
                ),
                true,
            );
        }
    }
}

#[test]
fn shared_opaque_quickapp_keys_with_trailing_slash_import_build_and_preview() {
    let image = template(2, 0x10);
    let firmware_files = BTreeMap::from([("app/icons/stock.bin".into(), image.clone())]);
    let replacements = BTreeMap::from([("native/shared.bin".into(), image.clone())]);
    let packages = ["", "single", " 快应用 ", "a..b", r"C:\foo:bar", "a\u{0085}"];
    let trailing = "../../快应用/";
    let mut original_manifest = manifest(json!([{
        "source": conora_core::app_icons::source(trailing), "destination":"native/shared.bin"
    }]));
    original_manifest["quickappIcons"] = json!(packages.map(|package| json!({
        "package":package, "destination":"native/shared.bin"
    })));
    let root = fixture(&firmware_files, &replacements, &original_manifest);
    let imported = import(root.path(), "theme");
    assert_eq!(
        imported["resources"].as_array().unwrap().len(),
        packages.len() + 1
    );
    assert_eq!(imported["unmapped"], json!([]));
    let theme = read_json(&root.path().join("theme/theme.json"));
    let shared = &theme["quickappIcons"][trailing];
    for package in packages {
        assert_eq!(theme["quickappIcons"][package], *shared);
    }
    let input = shared["input"].as_str().unwrap();
    assert_eq!(input, "source/raw/native/shared.bin");
    assert_eq!(
        fs::read(root.path().join("theme").join(input)).unwrap(),
        image
    );
    let built = result(
        run(
            root.path(),
            &["build", "--theme", "theme", "--target", "default", "--json"],
        ),
        true,
    );
    let pack = crpack::parse_crpack(
        &fs::read(root.path().join(built["outputs"][0].as_str().unwrap())).unwrap(),
    )
    .unwrap();
    assert_eq!(pack.quickapp_icons.len(), packages.len() + 1);
    assert!(pack.mappings.is_empty());
    for package in packages.into_iter().chain([trailing]) {
        let destination = conora_core::app_icons::destination(package);
        assert!(
            pack.quickapp_icons
                .iter()
                .any(|icon| icon.package == package && icon.destination == destination)
        );
        assert_eq!(pack.replacements[&destination], image);
    }
    result(
        run(
            root.path(),
            &[
                "preview", "--theme", "theme", "--target", "default", "--verify", "--json",
            ],
        ),
        true,
    );
    let index = read_json(
        &root
            .path()
            .join("theme/previews/preview_index-default.json"),
    );
    for entry in index["resources"].as_array().unwrap() {
        let filename = entry["png"].as_str().unwrap();
        assert_eq!(Path::new(filename).components().count(), 1);
        assert!(root.path().join("theme/previews").join(filename).is_file());
    }
}
