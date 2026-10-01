use std::fs;
use std::io::{Cursor, Write};
use std::path::Path;

use conora_core::crpack::parse_crpack;
use conora_core::lvgl;
use conora_core::project::{load_firmware, load_theme, prepare_target, target_ids};
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use serde_json::{Value, json};
use tempfile::TempDir;

#[test]
fn firmware_limit_rejects_large_sparse_files_before_reading() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("oversized.bin");
    fs::File::create(&path)
        .unwrap()
        .set_len(conora_core::project::MAX_FIRMWARE_BYTES as u64 + 1)
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
        conora_core::project::read_limited(&path, 1024)
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
    let size = conora_core::project::MAX_TEMPLATE_BYTES + 1;
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
