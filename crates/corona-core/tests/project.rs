use std::fs;
use std::io::{Cursor, Write};
use std::path::Path;

use corona_core::crpack::parse_crpack;
use corona_core::lvgl;
use corona_core::project::{load_firmware, load_theme, prepare_target, target_ids};
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use serde_json::{Value, json};
use tempfile::TempDir;

#[test]
fn firmware_limit_rejects_large_sparse_files_before_reading() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("oversized.bin");
    fs::File::create(&path)
        .unwrap()
        .set_len(corona_core::project::MAX_FIRMWARE_BYTES as u64 + 1)
        .unwrap();
    let error = load_firmware(&path, None).err().unwrap();
    assert!(error.contains("input limit"));
}

#[cfg(unix)]
#[test]
fn special_file_inputs_are_rejected_without_blocking() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("fifo");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        corona_core::project::read_limited(&path, 1024)
            .unwrap_err()
            .contains("regular file")
    );
    assert!(
        load_firmware(&path, None)
            .err()
            .unwrap()
            .contains("regular file")
    );
}

#[test]
fn unsupported_templates_are_rejected_before_materialization() {
    let directory = project();
    target(directory.path(), "A", "icons", b"not an image", 1);
    let built = prepare_target(&load_theme(directory.path()).unwrap(), "A");
    assert_eq!(built.report.errors[0].code, "unsupported_template");
}

#[test]
fn oversized_compressed_templates_are_rejected_before_materialization() {
    let directory = project();
    let original = template(2, 2, 0x0a);
    let mut header = romfs("icons", &original, 1);
    let size = corona_core::project::MAX_TEMPLATE_BYTES + 1;
    header[8..12].copy_from_slice(&((128 + size) as u32).to_be_bytes());
    header[104..108].copy_from_slice(&(size as u32).to_be_bytes());
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer
        .start_file(
            "vela_resource.bin",
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated),
        )
        .unwrap();
    writer.write_all(&header).unwrap();
    let zeros = [0; 64 * 1024];
    let mut remaining = size - original.len();
    while remaining > 0 {
        let chunk = remaining.min(zeros.len());
        writer.write_all(&zeros[..chunk]).unwrap();
        remaining -= chunk;
    }
    let bytes = writer.finish().unwrap().into_inner();
    let path = directory.path().join("large.bin");
    fs::write(&path, bytes).unwrap();
    let firmware = load_firmware(&path, None).unwrap();
    write_json(
        &directory.path().join("targets/large.json"),
        &json!({
            "firmware": "../large.bin", "firmwareSha256": firmware.sha256,
            "bindings": {"confirm": "app/icons/test.bin"}
        }),
    );
    let built = prepare_target(&load_theme(directory.path()).unwrap(), "large");
    assert_eq!(built.report.errors[0].code, "template_size");
}

#[test]
fn application_icons_build_without_firmware_bindings_and_support_target_templates() {
    let directory = project();
    target(directory.path(), "A", "icons", &template(2, 2, 0x10), 1);
    let mut target_config: Value =
        serde_json::from_slice(&fs::read(directory.path().join("targets/A.json")).unwrap())
            .unwrap();
    target_config["bindings"] = json!({});
    fs::write(
        directory.path().join("assets/template.bin"),
        template(8, 8, 0x10),
    )
    .unwrap();
    target_config["quickappIcons"] =
        json!({"ng.lst.corona": {"input":"assets/icon.png", "template":"assets/template.bin"}});
    write_json(&directory.path().join("targets/A.json"), &target_config);
    write_json(
        &directory.path().join("theme.json"),
        &json!({
            "themeId":"dark", "name":"App icons", "icons":{},
            "canopusIcon":{"input":"assets/icon.png"},
            "quickappIcons":{"ng.lst.corona":{"input":"assets/icon.png", "template":"assets/template.bin"}}
        }),
    );
    let built = prepare_target(&load_theme(directory.path()).unwrap(), "A");
    assert!(built.report.valid, "{:?}", built.report.errors);
    assert_eq!(built.report.resources.len(), 2);
    let pack = parse_crpack(&built.pack.unwrap()).unwrap();
    assert_eq!(
        pack.mappings[0].source,
        corona_core::app_icons::CANOPUS_SOURCE
    );
    assert_eq!(pack.quickapp_icons[0].package, "ng.lst.corona");
    assert_eq!(
        lvgl::inspect_image(
            &pack.replacements[&corona_core::app_icons::destination("ng.lst.corona")]
        )
        .unwrap()
        .width,
        8
    );
    assert_eq!(
        lvgl::inspect_image(&pack.replacements["canopus/manager_icon.bin"])
            .unwrap()
            .width,
        117
    );
    assert!(
        built
            .report
            .warnings
            .iter()
            .any(|w| w.code == "quickapp_receiver_required")
    );
}

#[test]
fn app_icon_validation_collects_missing_explicit_templates_and_invalid_raw_inputs() {
    let directory = project();
    target(directory.path(), "A", "icons", &template(2, 2, 0x10), 1);
    write_json(
        &directory.path().join("theme.json"),
        &json!({
            "themeId":"dark", "name":"App icons", "icons":{},
            "canopusIcon":{"input":"assets/icon.png", "mode":"raw"},
            "quickappIcons":{"ng.lst.corona":{"input":"assets/icon.png", "template":"assets/missing.bin"}}
        }),
    );
    let mut config: Value =
        serde_json::from_slice(&fs::read(directory.path().join("targets/A.json")).unwrap())
            .unwrap();
    config["bindings"] = json!({});
    write_json(&directory.path().join("targets/A.json"), &config);
    let built = prepare_target(&load_theme(directory.path()).unwrap(), "A");
    assert!(!built.report.valid);
    assert_eq!(
        built
            .report
            .errors
            .iter()
            .filter(|e| e.code == "app_icon")
            .count(),
        2
    );
}

fn write_json(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

fn png(width: u32, height: u32, varied: bool) -> Vec<u8> {
    let mut image = RgbaImage::new(width, height);
    for (index, pixel) in image.pixels_mut().enumerate() {
        *pixel = if varied {
            Rgba([index as u8, (index / 256) as u8, 100, 255])
        } else {
            Rgba([255, 0, 0, 255])
        };
    }
    let mut output = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(image)
        .write_to(&mut output, ImageFormat::Png)
        .unwrap();
    output.into_inner()
}

fn template(width: u16, height: u16, cf: u8) -> Vec<u8> {
    let stride = if cf == 0x10 { width * 4 } else { width };
    let palette = if cf == 0x0a { 1024 } else { 0 };
    let mut data = vec![0; 12 + palette + usize::from(stride) * usize::from(height)];
    data[0] = 0x19;
    data[1] = cf;
    data[4..6].copy_from_slice(&width.to_le_bytes());
    data[6..8].copy_from_slice(&height.to_le_bytes());
    data[8..10].copy_from_slice(&stride.to_le_bytes());
    if palette > 0 {
        data[12..16].copy_from_slice(&[0, 0, 255, 255]);
    }
    data
}

fn romfs(directory: &str, contents: &[u8], copies: usize) -> Vec<u8> {
    assert_eq!(directory.len(), 5);
    let mut data = vec![0u8; 96];
    data[..8].copy_from_slice(b"-rom1fs-");
    data[16..24].copy_from_slice(b"resource");
    data[32..36].copy_from_slice(&1u32.to_be_bytes());
    data[36..40].copy_from_slice(&64u32.to_be_bytes());
    data[48..51].copy_from_slice(b"app");
    data[64..68].copy_from_slice(&1u32.to_be_bytes());
    data[68..72].copy_from_slice(&96u32.to_be_bytes());
    data[80..85].copy_from_slice(directory.as_bytes());
    for index in 0..copies {
        let offset = data.len();
        let name = if index == 0 { "test.bin" } else { "copy.bin" };
        data.resize(offset + 32 + contents.len(), 0);
        let next = (data.len() + 15) & !15;
        let next_field = if index + 1 == copies {
            2
        } else {
            next as u32 | 2
        };
        data[offset..offset + 4].copy_from_slice(&next_field.to_be_bytes());
        data[offset + 8..offset + 12].copy_from_slice(&(contents.len() as u32).to_be_bytes());
        data[offset + 16..offset + 16 + name.len()].copy_from_slice(name.as_bytes());
        data[offset + 32..offset + 32 + contents.len()].copy_from_slice(contents);
        if index + 1 != copies {
            data.resize(next, 0);
        }
    }
    let length = data.len() as u32;
    data[8..12].copy_from_slice(&length.to_be_bytes());
    data
}

fn project() -> TempDir {
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join("assets")).unwrap();
    fs::create_dir(directory.path().join("targets")).unwrap();
    fs::write(directory.path().join("assets/icon.png"), png(4, 4, false)).unwrap();
    write_json(
        &directory.path().join("theme.json"),
        &json!({
            "schemaVersion": 1, "themeId": "dark", "name": "Dark Icons",
            "icons": {"confirm": "assets/icon.png"}
        }),
    );
    directory
}

fn target(root: &Path, id: &str, directory: &str, image: &[u8], copies: usize) {
    let firmware_path = root.join(format!("{id}.bin"));
    fs::write(&firmware_path, romfs(directory, image, copies)).unwrap();
    let firmware = load_firmware(&firmware_path, None).unwrap();
    write_json(
        &root.join(format!("targets/{id}.json")),
        &json!({
            "schemaVersion": 1,
            "firmware": format!("../{id}.bin"),
            "firmwareSha256": firmware.sha256,
            "device": format!("test-{id}"),
            "bindings": {"confirm": format!("app/{directory}/test.bin")}
        }),
    );
}

fn runtime_project(files: Value, mappings: Value) -> TempDir {
    let directory = project();
    target(directory.path(), "A", "icons", &template(2, 2, 0x10), 1);
    write_json(
        &directory.path().join("theme.json"),
        &json!({"themeId":"dark", "name":"Runtime"}),
    );
    let path = directory.path().join("targets/A.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    config["bindings"] = json!({});
    config["runtimeFiles"] = files;
    config["runtimeMappings"] = mappings;
    write_json(&path, &config);
    directory
}

#[test]
fn runtime_only_build_preserves_nonexistent_sources_aliases_overlaps_and_unmapped_files() {
    let rules = json!([
        {"source":"/data/absent/tree/", "destination":"custom/tree/"},
        {"source":"/data/absent/tree/icon.bin", "destination":"custom/tree/icon.bin"},
        {"source":"/elsewhere/alias.bin", "destination":"custom/tree/icon.bin"}
    ]);
    let directory = runtime_project(
        json!({
            "custom/tree/icon.bin":{"input":"assets/icon.png"},
            "unused.bin":{"input":"assets/raw.dat", "mode":"raw"}
        }),
        rules.clone(),
    );
    let raw = b"arbitrary\0\xff bytes, not an image";
    fs::write(directory.path().join("assets/raw.dat"), raw).unwrap();
    let built = prepare_target(&load_theme(directory.path()).unwrap(), "A");
    assert!(built.report.valid, "{:?}", built.report.errors);
    let pack = parse_crpack(&built.pack.unwrap()).unwrap();
    assert_eq!(serde_json::to_value(&pack.mappings).unwrap(), rules);
    assert_eq!(pack.replacements["unused.bin"], raw);
    assert_eq!(pack.replacements.len(), 2);
    let png_report = built
        .report
        .resources
        .iter()
        .find(|r| r.archive_path.as_deref() == Some("custom/tree/icon.bin"))
        .unwrap();
    assert_eq!(png_report.resource, "runtime:custom/tree/icon.bin");
    assert!(
        built
            .report
            .warnings
            .iter()
            .any(|warning| warning.code == "runtime_unverified"
                && warning.resource.as_deref() == Some("/data/absent/tree/icon.bin"))
    );
    assert_eq!(png_report.origin.as_deref(), Some("runtime"));
    assert_eq!((png_report.width, png_report.height), (Some(4), Some(4)));
    let raw_report = built
        .report
        .resources
        .iter()
        .find(|r| r.resource == "runtime:unused.bin")
        .unwrap();
    assert!(raw_report.format.is_none());
    assert_eq!(
        built
            .report
            .warnings
            .iter()
            .filter(|w| w.code == "runtime_unverified")
            .count(),
        2
    );
    assert_eq!(
        built
            .report
            .warnings
            .iter()
            .filter(|w| w.code == "raw_unverified")
            .count(),
        1
    );
}

#[test]
fn runtime_import_preserves_quickapp_custom_shared_destinations() {
    let directory = runtime_project(
        json!({
            "custom/shared.bin":{"input":"assets/raw.dat", "mode":"raw"},
            "unused.bin":{"input":"assets/raw.dat", "mode":"raw"}
        }),
        json!([{"source":"/data/canopus/manager_icon.bin", "destination":"custom/shared.bin"}]),
    );
    fs::write(
        directory.path().join("assets/raw.dat"),
        b"opaque imported BIN",
    )
    .unwrap();
    let path = directory.path().join("targets/A.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let declarations = json!([
        {"package":"ng.lst.corona", "destination":"custom/shared.bin"},
        {"package":"other.package", "destination":"custom/shared.bin"}
    ]);
    config["runtimeQuickappIcons"] = declarations.clone();
    write_json(&path, &config);
    let built = prepare_target(&load_theme(directory.path()).unwrap(), "A");
    assert!(built.report.valid, "{:?}", built.report.errors);
    let pack = parse_crpack(&built.pack.unwrap()).unwrap();
    assert_eq!(
        serde_json::to_value(pack.quickapp_icons).unwrap(),
        declarations
    );
    assert_eq!(pack.mappings.len(), 1);
    assert_eq!(pack.mappings[0].destination, "custom/shared.bin");
    assert!(pack.replacements.contains_key("unused.bin"));
}

#[test]
fn runtime_files_never_add_inferred_firmware_groups_and_rules_keep_order() {
    let directory = project();
    target(directory.path(), "A", "icons", &template(2, 2, 0x10), 1);
    let path = directory.path().join("targets/A.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    config["runtimeFiles"] = json!({"custom/icon.bin":{"input":"assets/icon.png"}, "orphan.bin":{"input":"assets/icon.png"}});
    config["runtimeMappings"] = json!([
        {"source":"/z/icon.bin", "destination":"custom/icon.bin"},
        {"source":"/a/icon.bin", "destination":"custom/icon.bin"}
    ]);
    write_json(&path, &config);
    let built = prepare_target(&load_theme(directory.path()).unwrap(), "A");
    assert!(built.report.valid, "{:?}", built.report.errors);
    let pack = parse_crpack(&built.pack.unwrap()).unwrap();
    assert_eq!(
        pack.mappings
            .iter()
            .map(|m| m.source.as_str())
            .collect::<Vec<_>>(),
        vec!["/resource/app/icons/test.bin", "/z/icon.bin", "/a/icon.bin"]
    );
}

#[test]
fn unmapped_runtime_sibling_of_firmware_file_stays_unmapped() {
    let directory = project();
    target(directory.path(), "A", "icons", &template(2, 2, 0x10), 1);
    let path = directory.path().join("targets/A.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    config["runtimeFiles"] = json!({"app/icons/unmapped.bin":{"input":"assets/icon.png"}});
    write_json(&path, &config);
    let built = prepare_target(&load_theme(directory.path()).unwrap(), "A");
    assert!(built.report.valid, "{:?}", built.report.errors);
    let pack = parse_crpack(&built.pack.unwrap()).unwrap();
    assert_eq!(
        pack.mappings,
        vec![corona_core::crpack::Mapping {
            source: "/resource/app/icons/test.bin".into(),
            destination: "app/icons/test.bin".into(),
        }]
    );
    assert!(pack.replacements.contains_key("app/icons/unmapped.bin"));
    assert!(
        !pack
            .mappings
            .iter()
            .any(|mapping| mapping.destination == "app/icons/unmapped.bin"
                || (mapping.destination.ends_with('/')
                    && "app/icons/unmapped.bin".starts_with(&mapping.destination)))
    );
}

#[test]
fn preserved_runtime_directory_rule_coexists_with_exact_firmware_exception() {
    let directory = project();
    target(directory.path(), "A", "icons", &template(2, 2, 0x10), 1);
    let path = directory.path().join("targets/A.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    config["runtimeFiles"] = json!({"imported/original.bin":{"input":"assets/icon.png"}});
    let rules = json!([
        {"source":"/resource/app/", "destination":"imported/"},
        {"source":"/data/alias.bin", "destination":"imported/original.bin"}
    ]);
    config["runtimeMappings"] = rules.clone();
    write_json(&path, &config);
    let built = prepare_target(&load_theme(directory.path()).unwrap(), "A");
    assert!(built.report.valid, "{:?}", built.report.errors);
    let pack = parse_crpack(&built.pack.unwrap()).unwrap();
    assert_eq!(
        pack.mappings[0],
        corona_core::crpack::Mapping {
            source: "/resource/app/icons/test.bin".into(),
            destination: "app/icons/test.bin".into(),
        }
    );
    assert_eq!(serde_json::to_value(&pack.mappings[1..]).unwrap(), rules);
}

#[test]
fn runtime_destination_collisions_allow_identical_bytes_but_reject_different_bytes() {
    let directory = project();
    target(directory.path(), "A", "icons", &template(2, 2, 0x10), 1);
    let path = directory.path().join("targets/A.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    config["overrides"] = json!({"confirm":{"input":"assets/raw.dat", "mode":"raw"}});
    config["runtimeFiles"] =
        json!({"app/icons/test.bin":{"input":"assets/second.dat", "mode":"raw"}});
    fs::write(directory.path().join("assets/raw.dat"), b"same bytes").unwrap();
    fs::write(directory.path().join("assets/second.dat"), b"same bytes").unwrap();
    write_json(&path, &config);
    let theme = load_theme(directory.path()).unwrap();
    let built = prepare_target(&theme, "A");
    assert!(built.report.valid, "{:?}", built.report.errors);
    assert_eq!(
        parse_crpack(&built.pack.unwrap())
            .unwrap()
            .replacements
            .len(),
        1
    );
    assert_eq!(built.report.resources.len(), 2);
    assert_ne!(
        built.report.resources[0].resource,
        built.report.resources[1].resource
    );
    assert!(
        built
            .report
            .resources
            .iter()
            .all(|resource| resource.archive_path.as_deref() == Some("app/icons/test.bin"))
    );
    fs::write(
        directory.path().join("assets/second.dat"),
        b"different bytes",
    )
    .unwrap();
    let built = prepare_target(&theme, "A");
    assert!(built.pack.is_none());
    assert!(built.report.errors.iter().any(|e| e.code == "runtime_asset" && e.message.contains("different replacement bytes")));
}

#[test]
fn runtime_validation_collects_per_asset_errors_and_enforces_protocol_rules() {
    let directory = runtime_project(
        json!({
            "a.bin":{"input":"assets/missing"},
            "b.bin":{"input":"assets"},
            "c.bin":{"input":"assets/icon.png", "template":"assets/missing"},
            "../unsafe.bin":{"input":"assets/icon.png"}
        }),
        json!([]),
    );
    let built = prepare_target(&load_theme(directory.path()).unwrap(), "A");
    assert_eq!(
        built
            .report
            .errors
            .iter()
            .filter(|e| e.code == "runtime_asset")
            .count(),
        4
    );
    assert!(built.pack.is_none());
    for rules in [
        json!([{"source":"/a", "destination":"custom/"}]),
        json!([{"source":"/a", "destination":"ok.bin"}, {"source":"/a", "destination":"ok.bin"}]),
        json!([{"source":"/a/", "destination":"missing/"}]),
        json!([{"source":"relative", "destination":"ok.bin"}]),
    ] {
        let directory = runtime_project(json!({"ok.bin":{"input":"assets/icon.png"}}), rules);
        let built = prepare_target(&load_theme(directory.path()).unwrap(), "A");
        assert!(built.pack.is_none());
        assert!(
            built
                .report
                .errors
                .iter()
                .any(|e| e.code == "pack_validation")
        );
    }
}

#[test]
fn runtime_input_and_template_reads_are_bounded_and_firmware_is_still_required() {
    let directory = runtime_project(
        json!({
            "large.bin":{"input":"assets/large.dat", "mode":"raw"},
            "template.bin":{"input":"assets/icon.png", "template":"assets/large.dat"}
        }),
        json!([]),
    );
    fs::File::create(directory.path().join("assets/large.dat"))
        .unwrap()
        .set_len(corona_core::project::MAX_TEMPLATE_BYTES as u64 + 1)
        .unwrap();
    let theme = load_theme(directory.path()).unwrap();
    let built = prepare_target(&theme, "A");
    assert_eq!(
        built
            .report
            .errors
            .iter()
            .filter(|e| e.code == "runtime_asset" && e.message.contains("input limit"))
            .count(),
        2
    );
    fs::remove_file(directory.path().join("A.bin")).unwrap();
    let built = prepare_target(&theme, "A");
    assert_eq!(built.report.errors[0].code, "firmware");
}

#[test]
fn runtime_files_share_aggregate_source_and_template_budgets() {
    for template_mode in [false, true] {
        let asset = if template_mode {
            json!({"input":"assets/icon.png", "template":"assets/large.dat"})
        } else {
            json!({"input":"assets/large.dat", "mode":"raw"})
        };
        let directory = runtime_project(json!({"a.bin":asset.clone(), "b.bin":asset}), json!([]));
        fs::File::create(directory.path().join("assets/large.dat"))
            .unwrap()
            .set_len((corona_core::project::MAX_TEMPLATE_BYTES / 2 + 1) as u64)
            .unwrap();
        let built = prepare_target(&load_theme(directory.path()).unwrap(), "A");
        assert!(built.pack.is_none());
        assert!(built.report.errors.iter().any(
            |e| e.role.as_deref() == Some("runtime:b.bin") && e.message.contains("input limit")
        ));
    }
}

#[test]
fn runtime_raw_files_share_the_manifest_inclusive_pack_budget() {
    let directory = runtime_project(
        json!({"full.bin":{"input":"assets/full.dat", "mode":"raw"}}),
        json!([]),
    );
    fs::File::create(directory.path().join("assets/full.dat"))
        .unwrap()
        .set_len(corona_core::project::MAX_TEMPLATE_BYTES as u64)
        .unwrap();
    let built = prepare_target(&load_theme(directory.path()).unwrap(), "A");
    assert!(built.pack.is_none());
    assert!(
        built
            .report
            .errors
            .iter()
            .any(|e| e.code == "pack_validation" && e.message.contains("plus corona.json"))
    );
}

#[test]
fn runtime_rules_share_native_rule_counts_and_duplicate_source_validation() {
    let directory = runtime_project(json!({"icon.bin":{"input":"assets/icon.png"}}), json!([]));
    let path = directory.path().join("targets/A.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    config["runtimeMappings"] = json!([{"source":"@quickapp-icon/pkg", "destination":"icon.bin"}]);
    config["runtimeQuickappIcons"] = json!([{"package":"pkg", "destination":"icon.bin"}]);
    write_json(&path, &config);
    let built = prepare_target(&load_theme(directory.path()).unwrap(), "A");
    assert!(
        built
            .report
            .errors
            .iter()
            .any(|e| e.message.contains("duplicate mapping source"))
    );
    config["runtimeMappings"] = json!(
        (0..256)
            .map(|i| json!({"source":format!("/data/{i}.bin"), "destination":"icon.bin"}))
            .collect::<Vec<_>>()
    );
    write_json(&path, &config);
    let built = prepare_target(&load_theme(directory.path()).unwrap(), "A");
    assert!(
        built
            .report
            .errors
            .iter()
            .any(|e| e.message.contains("256"))
    );
}

#[test]
fn proposed_target_config_uses_external_assets_without_project_writes_or_repinning() {
    fn snapshot(root: &Path) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
        fn visit(
            root: &Path,
            path: &Path,
            files: &mut std::collections::BTreeMap<std::path::PathBuf, Vec<u8>>,
        ) {
            for entry in fs::read_dir(path).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    visit(root, &path, files);
                } else {
                    files.insert(
                        path.strip_prefix(root).unwrap().to_owned(),
                        fs::read(&path).unwrap(),
                    );
                }
            }
        }
        let mut files = std::collections::BTreeMap::new();
        visit(root, root, &mut files);
        files
    }
    let directory = project();
    target(directory.path(), "A", "icons", &template(2, 2, 0x10), 1);
    let theme = load_theme(directory.path()).unwrap();
    let saved = corona_core::project::load_target(&theme, "A").unwrap();
    let pinned = saved.firmware_sha256.clone();
    let before = snapshot(directory.path());
    let external = tempfile::tempdir().unwrap();
    let input = external.path().join("pending.dat");
    fs::write(&input, b"pending opaque runtime bytes").unwrap();
    let mut proposed = saved.clone();
    proposed.runtime_files.insert(
        "custom/pending.bin".into(),
        serde_json::from_value(json!({"input":input, "mode":"raw"})).unwrap(),
    );
    proposed
        .runtime_mappings
        .push(corona_core::crpack::Mapping {
            source: "/data/new/runtime.bin".into(),
            destination: "custom/pending.bin".into(),
        });
    // This id has no config file; the relative firmware path is still targets/../A.bin.
    let built = corona_core::project::prepare_target_config(&theme, "pending", &proposed);
    assert!(built.report.valid, "{:?}", built.report.errors);
    assert_eq!(
        built.report.firmware_sha256.as_deref(),
        Some(pinned.as_str())
    );
    let pack = parse_crpack(&built.pack.unwrap()).unwrap();
    assert_eq!(
        pack.replacements["custom/pending.bin"],
        b"pending opaque runtime bytes"
    );
    assert_eq!(proposed.firmware_sha256, pinned);
    assert_eq!(snapshot(directory.path()), before);
    assert_eq!(
        corona_core::project::load_target(&theme, "A")
            .unwrap()
            .firmware_sha256,
        pinned
    );
    assert!(!directory.path().join("targets/pending.json").exists());

    proposed.firmware_sha256 = "0".repeat(64);
    let failed = corona_core::project::prepare_target_config(&theme, "pending", &proposed);
    assert!(failed.pack.is_none());
    assert_eq!(failed.report.errors[0].code, "firmware");
    assert!(failed.report.errors[0].message.contains("SHA-256 mismatch"));
    assert_eq!(snapshot(directory.path()), before);

    proposed.schema_version = 2;
    let failed = corona_core::project::prepare_target_config(&theme, "pending", &proposed);
    assert_eq!(failed.report.errors[0].code, "target_config");
    assert!(failed.report.errors[0].message.contains("schemaVersion"));
}

#[test]
fn legacy_target_runtime_fields_default_and_serialize_only_when_nonempty() {
    let directory = project();
    target(directory.path(), "A", "icons", &template(2, 2, 0x10), 1);
    let loaded =
        corona_core::project::load_target(&load_theme(directory.path()).unwrap(), "A").unwrap();
    assert!(loaded.runtime_files.is_empty());
    assert!(loaded.runtime_mappings.is_empty());
    assert!(loaded.runtime_quickapp_icons.is_empty());
    let serialized = serde_json::to_value(loaded).unwrap();
    for key in ["runtimeFiles", "runtimeMappings", "runtimeQuickappIcons"] {
        assert!(serialized.get(key).is_none());
    }
}

#[test]
fn one_theme_builds_distinct_paths_sizes_and_formats_for_two_firmwares() {
    let directory = project();
    target(directory.path(), "A", "icons", &template(2, 2, 0x0a), 1);
    target(directory.path(), "B", "other", &template(4, 4, 0x10), 1);
    let theme = load_theme(directory.path()).unwrap();
    assert_eq!(target_ids(&theme).unwrap(), vec!["A", "B"]);
    for (id, path, size, format) in [
        ("A", "app/icons/test.bin", 2, "LVGL v9 I8"),
        ("B", "app/other/test.bin", 4, "LVGL v9 ARGB8888"),
    ] {
        let built = prepare_target(&theme, id);
        assert!(built.report.valid, "{:?}", built.report.errors);
        assert_eq!(built.report.resources[0].width, Some(size));
        assert_eq!(built.report.resources[0].format.as_deref(), Some(format));
        let pack = parse_crpack(&built.pack.unwrap()).unwrap();
        assert_eq!(pack.replacements.len(), 1);
        assert_eq!(pack.target.as_deref(), Some(format!("test-{id}").as_str()));
        let (info, _) = lvgl::decode_image_png(&pack.replacements[path]).unwrap();
        assert_eq!((info.width, info.height), (size, size));
    }
}

#[test]
fn one_role_can_replace_multiple_resources() {
    let directory = project();
    target(directory.path(), "A", "icons", &template(2, 2, 0x0a), 2);
    let path = directory.path().join("targets/A.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    config["bindings"]["confirm"] = json!(["app/icons/test.bin", "app/icons/copy.bin"]);
    write_json(&path, &config);
    let built = prepare_target(&load_theme(directory.path()).unwrap(), "A");
    assert!(built.report.valid, "{:?}", built.report.errors);
    let pack = parse_crpack(&built.pack.unwrap()).unwrap();
    assert_eq!(pack.replacements.len(), 2);
    assert_eq!(
        pack.replacements["app/icons/test.bin"],
        pack.replacements["app/icons/copy.bin"]
    );
}

#[test]
fn firmware_fingerprint_mismatch_stops_building() {
    let directory = project();
    target(directory.path(), "A", "icons", &template(2, 2, 0x0a), 1);
    let path = directory.path().join("A.bin");
    let mut bytes = fs::read(&path).unwrap();
    bytes.push(0);
    fs::write(&path, bytes).unwrap();
    let built = prepare_target(&load_theme(directory.path()).unwrap(), "A");
    assert!(built.pack.is_none());
    assert!(built.report.errors[0].message.contains("SHA-256 mismatch"));
}

#[test]
fn target_override_handles_a_different_aspect_ratio() {
    let directory = project();
    target(directory.path(), "wide", "other", &template(4, 2, 0x10), 1);
    let theme = load_theme(directory.path()).unwrap();
    let failed = prepare_target(&theme, "wide");
    assert_eq!(failed.report.errors[0].code, "image_conversion");
    fs::write(directory.path().join("assets/wide.png"), png(8, 4, false)).unwrap();
    let path = directory.path().join("targets/wide.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    config["overrides"] = json!({"confirm": "assets/wide.png"});
    write_json(&path, &config);
    assert!(prepare_target(&theme, "wide").report.valid);
}

#[test]
fn diagnostics_collect_independent_binding_and_asset_errors() {
    let directory = project();
    target(directory.path(), "A", "icons", &template(2, 2, 0x0a), 1);
    write_json(
        &directory.path().join("theme.json"),
        &json!({
            "themeId": "dark", "name": "Dark Icons",
            "icons": {"confirm": "missing.png", "settings": "assets/icon.png"}
        }),
    );
    let path = directory.path().join("targets/A.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    config["bindings"]["confirm"] = json!(["../escape.bin", "absent.bin"]);
    write_json(&path, &config);
    let built = prepare_target(&load_theme(directory.path()).unwrap(), "A");
    let codes: Vec<_> = built
        .report
        .errors
        .iter()
        .map(|error| error.code.as_str())
        .collect();
    assert!(codes.contains(&"asset_input"));
    assert!(codes.contains(&"resource_path"));
    assert!(codes.contains(&"missing_resource"));
    assert!(codes.contains(&"missing_binding"));
    assert!(built.pack.is_none());
}

#[test]
fn rejects_multiple_owners_for_a_resource_and_unknown_roles() {
    let directory = project();
    target(directory.path(), "A", "icons", &template(2, 2, 0x0a), 1);
    write_json(
        &directory.path().join("theme.json"),
        &json!({
            "themeId": "dark", "name": "Dark",
            "icons": {"confirm": "assets/icon.png", "duplicate": "assets/icon.png"}
        }),
    );
    let path = directory.path().join("targets/A.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    config["bindings"]["duplicate"] = json!("app/icons/test.bin");
    config["bindings"]["typo"] = json!("app/icons/test.bin");
    write_json(&path, &config);
    let built = prepare_target(&load_theme(directory.path()).unwrap(), "A");
    assert!(
        built
            .report
            .errors
            .iter()
            .any(|error| error.code == "duplicate_binding")
    );
    assert!(
        built
            .report
            .errors
            .iter()
            .any(|error| error.code == "unknown_role")
    );
}

#[test]
fn palette_quantization_requires_explicit_opt_in() {
    let directory = project();
    fs::write(directory.path().join("assets/icon.png"), png(17, 17, true)).unwrap();
    target(directory.path(), "A", "icons", &template(17, 17, 0x0a), 1);
    let failed = prepare_target(&load_theme(directory.path()).unwrap(), "A");
    assert!(
        failed
            .report
            .errors
            .iter()
            .any(|error| error.code == "image_conversion")
    );
    write_json(
        &directory.path().join("theme.json"),
        &json!({
            "themeId": "dark", "name": "Dark",
            "icons": {"confirm": {"input": "assets/icon.png", "allowQuantize": true}}
        }),
    );
    let built = prepare_target(&load_theme(directory.path()).unwrap(), "A");
    assert!(built.report.valid, "{:?}", built.report.errors);
    assert!(
        built
            .report
            .warnings
            .iter()
            .any(|warning| warning.code == "lossy_conversion")
    );
}

#[test]
fn raw_replacements_are_explicit_and_reported_as_unverified() {
    let directory = project();
    target(directory.path(), "A", "icons", &template(2, 2, 0x0a), 1);
    fs::write(directory.path().join("assets/raw.bin"), b"raw bytes").unwrap();
    write_json(
        &directory.path().join("theme.json"),
        &json!({
            "themeId": "dark", "name": "Dark",
            "icons": {"confirm": {"input": "assets/raw.bin", "mode": "raw"}}
        }),
    );
    let built = prepare_target(&load_theme(directory.path()).unwrap(), "A");
    assert!(built.report.valid);
    assert_eq!(built.report.warnings[0].code, "raw_unverified");
    assert_eq!(
        parse_crpack(&built.pack.unwrap()).unwrap().replacements["app/icons/test.bin"],
        b"raw bytes"
    );
}

#[test]
fn relative_paths_survive_moving_the_entire_project() {
    let directory = project();
    target(directory.path(), "A", "icons", &template(2, 2, 0x0a), 1);
    let parent = tempfile::tempdir().unwrap();
    let moved = parent.path().join("moved");
    fs::rename(directory.path(), &moved).unwrap();
    let built = prepare_target(&load_theme(&moved.join("theme.json")).unwrap(), "A");
    assert!(built.report.valid, "{:?}", built.report.errors);
}

#[test]
fn rejects_unknown_config_fields_versions_and_unsafe_target_names() {
    let directory = project();
    target(directory.path(), "A", "icons", &template(2, 2, 0x0a), 1);
    let theme = load_theme(directory.path()).unwrap();
    assert!(prepare_target(&theme, "../A").pack.is_none());
    let path = directory.path().join("targets/A.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    config["schemaVersion"] = json!(2);
    write_json(&path, &config);
    assert_eq!(
        prepare_target(&theme, "A").report.errors[0].code,
        "target_config"
    );
    config["schemaVersion"] = json!(1);
    config["firmwareSha256"] = json!("");
    write_json(&path, &config);
    assert!(
        prepare_target(&theme, "A").report.errors[0]
            .message
            .contains("firmwareSha256")
    );
    write_json(
        &directory.path().join("theme.json"),
        &json!({
            "themeId": "dark", "name": "Dark", "icons": {}, "typo": true
        }),
    );
    assert!(load_theme(directory.path()).is_err());
}

#[test]
fn explicit_exclusions_are_reported_without_reading_their_assets() {
    let directory = project();
    target(directory.path(), "A", "icons", &template(2, 2, 0x0a), 1);
    write_json(
        &directory.path().join("theme.json"),
        &json!({
            "themeId": "dark", "name": "Dark",
            "icons": {"confirm": "assets/icon.png", "optional": "missing.png"}
        }),
    );
    let path = directory.path().join("targets/A.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    config["excluded"] = json!({"optional": "Not present on this firmware"});
    write_json(&path, &config);
    let theme = load_theme(directory.path()).unwrap();
    let built = prepare_target(&theme, "A");
    assert!(built.report.valid, "{:?}", built.report.errors);
    assert_eq!(built.report.resources.len(), 1);
    assert_eq!(
        built.report.excluded["optional"],
        "Not present on this firmware"
    );
    assert_eq!(
        serde_json::to_value(&built.report).unwrap()["excluded"],
        config["excluded"]
    );

    config.as_object_mut().unwrap().remove("excluded");
    write_json(&path, &config);
    let built = prepare_target(&theme, "A");
    assert!(
        built
            .report
            .errors
            .iter()
            .any(|error| error.code == "missing_binding")
    );
    let loaded = corona_core::project::load_target(&theme, "A").unwrap();
    assert!(loaded.excluded.is_empty());
    assert!(
        serde_json::to_value(loaded)
            .unwrap()
            .get("excluded")
            .is_none()
    );
}

#[test]
fn invalid_exclusions_and_conflicts_are_independent_errors() {
    let directory = project();
    target(directory.path(), "A", "icons", &template(2, 2, 0x0a), 1);
    let path = directory.path().join("targets/A.json");
    let original: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let theme = load_theme(directory.path()).unwrap();
    for reason in [
        "".to_string(),
        "   ".into(),
        "reason\n".into(),
        "x".repeat(1025),
    ] {
        let mut config = original.clone();
        config["excluded"] = json!({"confirm": reason, "unknown": "Not supported"});
        write_json(&path, &config);
        let built = prepare_target(&theme, "A");
        for code in ["exclusion_reason", "excluded_conflict", "unknown_role"] {
            assert!(
                built.report.errors.iter().any(|error| error.code == code),
                "{code}: {:?}",
                built.report.errors
            );
        }
        assert!(built.pack.is_none());
        assert!(
            !built
                .report
                .errors
                .iter()
                .any(|error| error.code == "missing_binding")
        );
    }
    let mut config = original;
    config["bindings"] = json!({});
    config["overrides"] = json!({"confirm": "assets/icon.png"});
    config["excluded"] = json!({"confirm": "No firmware equivalent"});
    write_json(&path, &config);
    assert!(
        prepare_target(&theme, "A")
            .report
            .errors
            .iter()
            .any(|error| error.code == "excluded_conflict")
    );
}

#[test]
fn dotted_firmware_version_target_ids_are_safe_and_discoverable() {
    use corona_core::project::validate_target_id;
    for id in ["p67-3.101.043", "A.B", "a."] {
        assert!(validate_target_id(id).is_ok());
    }
    for id in [
        "",
        ".",
        "..",
        ".hidden",
        "../A",
        "a/../b",
        "a\\b",
        "/absolute",
        "non ascii é",
    ] {
        assert!(validate_target_id(id).is_err(), "{id}");
    }
    assert!(validate_target_id(&"a".repeat(64)).is_ok());
    assert!(validate_target_id(&"a".repeat(65)).is_err());
    let directory = project();
    target(
        directory.path(),
        "p67-3.101.043",
        "icons",
        &template(2, 2, 0x0a),
        1,
    );
    let theme = load_theme(directory.path()).unwrap();
    assert_eq!(target_ids(&theme).unwrap(), ["p67-3.101.043"]);
    assert!(prepare_target(&theme, "p67-3.101.043").report.valid);
}

fn batch_indexes() -> Vec<corona_core::firmware::FirmwareIndex> {
    use corona_core::firmware::FirmwareIndex;
    let data = romfs("icons", b"test", 2);
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer
        .start_file(
            "vela_resource.bin",
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated),
        )
        .unwrap();
    writer.write_all(&data).unwrap();
    vec![
        FirmwareIndex::from_romfs(data).unwrap(),
        FirmwareIndex::from_firmware(writer.finish().unwrap().into_inner()).unwrap(),
    ]
}

#[test]
fn batch_resources_are_deduplicated_and_visited_in_physical_order() {
    for index in batch_indexes() {
        let paths = vec![
            "app/icons/copy.bin".into(),
            "app/icons/test.bin".into(),
            "app/icons/copy.bin".into(),
        ];
        let mut visited = Vec::new();
        index
            .visit_file_bytes(&paths, 4, 8, |path, bytes| {
                visited.push(path.to_owned());
                assert_eq!(bytes, b"test");
                Ok(())
            })
            .unwrap();
        assert_eq!(visited, ["app/icons/test.bin", "app/icons/copy.bin"]);
        // Visiting does not break the independent lazy browser API.
        assert_eq!(
            index
                .file_bytes("app/icons/copy.bin")
                .unwrap()
                .unwrap()
                .as_slice(),
            b"test"
        );
    }
}

#[test]
fn batch_preflights_all_paths_and_budgets_before_any_callback() {
    for index in batch_indexes() {
        let paths = vec!["app/icons/test.bin".into(), "app/icons/copy.bin".into()];
        let mut calls = 0;
        let mut callback = |_: &str, _: &[u8]| {
            calls += 1;
            Ok(())
        };
        assert!(
            index
                .visit_file_bytes(&paths, 3, 8, &mut callback)
                .unwrap_err()
                .contains("per-file limit")
        );
        assert!(
            index
                .visit_file_bytes(&paths, 4, 7, &mut callback)
                .unwrap_err()
                .contains("aggregate limit")
        );
        let missing = vec![paths[0].clone(), "missing.bin".into(), "another.bin".into()];
        let error = index
            .visit_file_bytes(&missing, 3, 3, &mut callback)
            .unwrap_err();
        for text in [
            "missing.bin",
            "another.bin",
            "per-file limit",
            "aggregate limit",
        ] {
            assert!(error.contains(text), "{error}");
        }
        assert_eq!(calls, 0);
        index
            .visit_file_bytes(&[], 0, 0, |_, _| panic!("empty request"))
            .unwrap();
    }
}

#[test]
fn batch_callback_error_stops_before_the_next_resource() {
    for index in batch_indexes() {
        let paths = vec!["app/icons/copy.bin".into(), "app/icons/test.bin".into()];
        let mut calls = 0;
        let error = index
            .visit_file_bytes(&paths, 4, 8, |_, _| {
                calls += 1;
                Err("callback stopped".into())
            })
            .unwrap_err();
        assert_eq!(error, "callback stopped");
        assert_eq!(calls, 1);
    }
}

#[test]
fn preparation_progress_reports_loading_and_conversion_without_rereading_assets() {
    let directory = project();
    target(directory.path(), "A", "icons", &template(2, 2, 0x0a), 2);
    let path = directory.path().join("targets/A.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    config["bindings"]["confirm"] = json!(["app/icons/copy.bin", "app/icons/test.bin"]);
    write_json(&path, &config);
    let theme = load_theme(directory.path()).unwrap();
    let mut events = Vec::new();
    let built = corona_core::project::prepare_target_with_progress(&theme, "A", |event| {
        events.push(event.to_owned());
        if event == "Converting role confirm" {
            fs::remove_file(directory.path().join("assets/icon.png")).unwrap();
        }
    });
    assert!(built.report.valid, "{:?}", built.report.errors);
    assert_eq!(built.report.resources.len(), 2);
    assert_eq!(
        events,
        [
            "Loading firmware for target A",
            "Loading asset for role confirm",
            "Converting role confirm"
        ]
    );
}

#[test]
fn aggregate_template_limit_rejects_conversion_before_materializing_any_template() {
    let directory = project();
    let mut original = template(2, 2, 0x0a);
    original.resize(corona_core::project::MAX_TEMPLATE_BYTES / 2 + 1, 0);
    let data = romfs("icons", &original, 2);
    drop(original);
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer
        .start_file(
            "vela_resource.bin",
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated),
        )
        .unwrap();
    writer.write_all(&data).unwrap();
    drop(data);
    let firmware_path = directory.path().join("aggregate.bin");
    fs::write(&firmware_path, writer.finish().unwrap().into_inner()).unwrap();
    let firmware = load_firmware(&firmware_path, None).unwrap();
    write_json(
        &directory.path().join("targets/aggregate.json"),
        &json!({
            "firmware": "../aggregate.bin", "firmwareSha256": firmware.sha256,
            "bindings": {"confirm": ["app/icons/copy.bin", "app/icons/test.bin"]}
        }),
    );
    let mut conversion_started = false;
    let built = corona_core::project::prepare_target_with_progress(
        &load_theme(directory.path()).unwrap(),
        "aggregate",
        |event| {
            conversion_started |= event.starts_with("Converting");
        },
    );
    assert!(!conversion_started);
    assert!(built.report.resources.is_empty());
    assert!(
        built
            .report
            .errors
            .iter()
            .any(|error| error.code == "template_size" && error.message.contains("aggregate"))
    );
    assert!(built.pack.is_none());
}

#[test]
fn aggregate_source_assets_are_bounded_even_when_each_file_is_within_its_limit() {
    let directory = project();
    target(directory.path(), "A", "icons", &template(2, 2, 0x0a), 2);
    for name in ["first.bin", "second.bin"] {
        fs::File::create(directory.path().join("assets").join(name))
            .unwrap()
            .set_len((corona_core::project::MAX_TEMPLATE_BYTES / 2 + 1) as u64)
            .unwrap();
    }
    write_json(
        &directory.path().join("theme.json"),
        &json!({
            "themeId": "dark", "name": "Dark",
            "icons": {
                "confirm": {"input": "assets/first.bin", "mode": "raw"},
                "second": {"input": "assets/second.bin", "mode": "raw"}
            }
        }),
    );
    let path = directory.path().join("targets/A.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    config["bindings"]["second"] = json!("app/icons/copy.bin");
    write_json(&path, &config);
    let built = prepare_target(&load_theme(directory.path()).unwrap(), "A");
    assert!(
        built
            .report
            .errors
            .iter()
            .any(|error| error.code == "asset_input" && error.role.as_deref() == Some("second"))
    );
    assert_eq!(built.report.resources.len(), 1);
    assert!(built.pack.is_none());
}

#[test]
fn template_free_quickapp_project_build_retains_source_dimensions_and_alpha() {
    let directory = project();
    target(
        directory.path(),
        "A",
        "icons",
        b"unrelated firmware resource",
        1,
    );
    let mut target_config: Value =
        serde_json::from_slice(&fs::read(directory.path().join("targets/A.json")).unwrap())
            .unwrap();
    target_config["bindings"] = json!({});
    // Target overrides can also omit a template.
    target_config["quickappIcons"] =
        json!({"org.app": {"input":"assets/own.png", "filter":"nearest"}});
    write_json(&directory.path().join("targets/A.json"), &target_config);
    let image = RgbaImage::from_fn(301, 2, |x, y| {
        Rgba([x as u8, (x / 256) as u8, 39, (x + y) as u8])
    });
    image.save(directory.path().join("assets/own.png")).unwrap();
    write_json(
        &directory.path().join("theme.json"),
        &json!({
            "themeId":"dark", "name":"Own dimensions", "icons":{},
            "quickappIcons":{
                "org.app":{"input":"assets/icon.png", "template":"assets/missing.bin"},
                "org.shared":{"input":"assets/own.png"}
            }
        }),
    );
    let built = prepare_target(&load_theme(directory.path()).unwrap(), "A");
    assert!(built.report.valid, "{:?}", built.report.errors);
    assert_eq!(built.report.resources.len(), 2);
    for summary in &built.report.resources {
        assert_eq!(summary.format.as_deref(), Some("LVGL v9 ARGB8888"));
        assert_eq!((summary.width, summary.height), (Some(301), Some(2)));
        assert!(!summary.lossy);
    }
    let pack = parse_crpack(&built.pack.unwrap()).unwrap();
    assert!(pack.mappings.is_empty());
    for package in ["org.app", "org.shared"] {
        let bytes = &pack.replacements[&corona_core::app_icons::destination(package)];
        assert_eq!(lvgl::decode_to_rgba(bytes).unwrap().1, image);
    }
}
