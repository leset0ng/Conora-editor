use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use conora_core::{crpack, lvgl};
use image::{Rgba, RgbaImage};
use serde_json::{Value, json};
use tempfile::TempDir;

fn run(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_conora"))
        .current_dir(root)
        .args(args)
        .output()
        .unwrap()
}

fn result(output: Output, code: i32) -> Value {
    assert_eq!(
        output.status.code(),
        Some(code),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schemaVersion"], 1);
    assert_eq!(value["ok"], code == 0);
    value
}

fn update(path: &Path, f: impl FnOnce(&mut Value)) {
    let mut value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    f(&mut value);
    fs::write(path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
}

fn template(cf: u8) -> Vec<u8> {
    let stride = if cf == 0x10 { 8u16 } else { 2u16 };
    let palette = if cf == 0x0a { 1024 } else { 0 };
    let mut bytes = vec![0; 12 + palette + usize::from(stride) * 2];
    bytes[0] = 0x19;
    bytes[1] = cf;
    bytes[4..6].copy_from_slice(&2u16.to_le_bytes());
    bytes[6..8].copy_from_slice(&2u16.to_le_bytes());
    bytes[8..10].copy_from_slice(&stride.to_le_bytes());
    if palette != 0 {
        bytes[12..16].copy_from_slice(&[0, 0, 255, 255]);
    }
    bytes
}

fn firmware(count: usize, image: &[u8]) -> Vec<u8> {
    let node_size = (32 + image.len()).next_multiple_of(16);
    let mut bytes = vec![0; 96 + node_size * count];
    bytes[..8].copy_from_slice(b"-rom1fs-");
    let len = bytes.len() as u32;
    bytes[8..12].copy_from_slice(&len.to_be_bytes());
    bytes[16..24].copy_from_slice(b"resource");
    for (node, child, name) in [(32, 64u32, "app"), (64, 96u32, "icons")] {
        bytes[node..node + 4].copy_from_slice(&1u32.to_be_bytes());
        bytes[node + 4..node + 8].copy_from_slice(&child.to_be_bytes());
        bytes[node + 16..node + 16 + name.len()].copy_from_slice(name.as_bytes());
    }
    for i in 0..count {
        let offset = 96 + i * node_size;
        let next = if i + 1 == count {
            2
        } else {
            (offset + node_size) as u32 | 2
        };
        bytes[offset..offset + 4].copy_from_slice(&next.to_be_bytes());
        bytes[offset + 8..offset + 12].copy_from_slice(&(image.len() as u32).to_be_bytes());
        let name = format!("test{i:05}.bin");
        bytes[offset + 16..offset + 16 + name.len()].copy_from_slice(name.as_bytes());
        bytes[offset + 32..offset + 32 + image.len()].copy_from_slice(image);
    }
    bytes
}

fn fixture(count: usize, cf: u8) -> TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("firmware.bin"),
        firmware(count, &template(cf)),
    )
    .unwrap();
    result(
        run(
            root.path(),
            &[
                "init",
                "theme",
                "--firmware",
                "firmware.bin",
                "--target",
                "A",
                "--json",
            ],
        ),
        0,
    );
    RgbaImage::from_pixel(4, 4, Rgba([255, 0, 0, 255]))
        .save(root.path().join("theme/assets/source.png"))
        .unwrap();
    update(&root.path().join("theme/theme.json"), |v| {
        v["icons"] = Value::Object(
            (0..count)
                .map(|i| (format!("role/{i:05}"), json!("assets/source.png")))
                .collect(),
        );
    });
    update(&root.path().join("theme/targets/A.json"), |v| {
        v["bindings"] = Value::Object(
            (0..count)
                .map(|i| {
                    (
                        format!("role/{i:05}"),
                        json!(format!("app/icons/test{i:05}.bin")),
                    )
                })
                .collect(),
        );
    });
    root
}

fn preview(root: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["preview", "--theme", "theme", "--target", "A", "--json"];
    args.extend_from_slice(extra);
    run(root, &args)
}

fn index(root: &Path) -> Value {
    serde_json::from_slice(&fs::read(root.join("theme/previews/preview_index-A.json")).unwrap())
        .unwrap()
}

#[test]
fn actual_pack_pixels_dimensions_manifest_and_verification_are_indexed() {
    let root = fixture(1, 0x0a);
    let output = result(preview(root.path(), &["--verify"]), 0);
    assert_eq!(output["command"], "preview");
    assert_eq!(output["targets"][0]["preview"]["images"], 1);
    assert!(!root.path().join("theme/dist").exists());
    let index = index(root.path());
    assert_eq!(index["manifest"]["format"], "canopus-resource-pack");
    let entry = &index["resources"][0];
    assert_eq!(entry["role"], "role/00000");
    assert_eq!(entry["resource"], "app/icons/test00000.bin");
    assert_eq!(entry["verification"]["samePixels"], true);
    assert_eq!(entry["verification"]["sameMetadata"], true);
    assert_eq!(entry["verification"]["sameHeader"], true);
    let png = image::open(
        root.path()
            .join("theme/previews")
            .join(entry["png"].as_str().unwrap()),
    )
    .unwrap()
    .to_rgba8();
    assert_eq!(png.dimensions(), (2, 2)); // Not the 4x4 source PNG.
    result(
        run(
            root.path(),
            &["build", "--theme", "theme", "--target", "A", "--json"],
        ),
        0,
    );
    let pack =
        crpack::parse_crpack(&fs::read(root.path().join("theme/dist/conora-A.crpack")).unwrap())
            .unwrap();
    let (_, actual) = lvgl::decode_to_rgba(&pack.replacements["app/icons/test00000.bin"]).unwrap();
    assert_eq!(png, actual);
    let sheet = image::open(
        root.path()
            .join("theme/previews")
            .join(entry["tile"]["sheet"].as_str().unwrap()),
    )
    .unwrap()
    .to_rgba8();
    let x = entry["tile"]["x"].as_u64().unwrap() as u32;
    let y = entry["tile"]["y"].as_u64().unwrap() as u32;
    assert_eq!(sheet.get_pixel(x, y), actual.get_pixel(0, 0));
}

#[test]
fn raw_nonimages_are_explicitly_skipped_and_lossy_pixels_do_not_fail_verify() {
    let root = fixture(1, 0x0a);
    fs::write(root.path().join("theme/assets/raw.bin"), b"not an image").unwrap();
    update(&root.path().join("theme/theme.json"), |v| {
        v["icons"]["role/00000"] = json!({"input": "assets/raw.bin", "mode": "raw"})
    });
    result(preview(root.path(), &["--verify"]), 0);
    let raw = index(root.path());
    assert_eq!(raw["resources"][0]["status"], "skipped");
    assert!(!raw["resources"][0]["reason"].as_str().unwrap().is_empty());
    assert_eq!(raw["contactSheets"], json!([]));
    // More than 256 colors at the actual target size requires lossy I8 quantization.
    let mut original = template(0x0a);
    original.resize(12 + 1024 + 17 * 17, 0);
    for offset in [4, 6, 8] {
        original[offset..offset + 2].copy_from_slice(&17u16.to_le_bytes());
    }
    fs::write(root.path().join("firmware.bin"), firmware(1, &original)).unwrap();
    result(
        run(
            root.path(),
            &[
                "target",
                "add",
                "A",
                "--theme",
                "theme",
                "--firmware",
                "firmware.bin",
                "--force",
                "--json",
            ],
        ),
        0,
    );
    update(&root.path().join("theme/targets/A.json"), |v| {
        v["bindings"] = json!({"role/00000": "app/icons/test00000.bin"})
    });
    RgbaImage::from_fn(17, 17, |x, y| {
        Rgba([(x * 13) as u8, (y * 13) as u8, 0, 255])
    })
    .save(root.path().join("theme/assets/source.png"))
    .unwrap();
    update(&root.path().join("theme/theme.json"), |v| {
        v["icons"]["role/00000"] = json!({"input": "assets/source.png", "allowQuantize": true})
    });
    result(preview(root.path(), &["--verify", "--force"]), 0);
    let lossy = index(root.path());
    assert_eq!(lossy["resources"][0]["lossy"], true);
    assert_eq!(lossy["resources"][0]["verification"]["samePixels"], false);
}

#[test]
fn pages_are_bounded_and_every_tile_has_a_mapping() {
    let root = fixture(257, 0x10);
    result(preview(root.path(), &[]), 0);
    let index = index(root.path());
    assert_eq!(index["contactSheets"].as_array().unwrap().len(), 2);
    assert_eq!(index["resources"].as_array().unwrap().len(), 257);
    for name in index["contactSheets"].as_array().unwrap() {
        let sheet = image::open(
            root.path()
                .join("theme/previews")
                .join(name.as_str().unwrap()),
        )
        .unwrap();
        assert!(sheet.width() <= 4096 && sheet.height() <= 4096);
        assert!(u64::from(sheet.width()) * u64::from(sheet.height()) * 4 <= 16 * 1024 * 1024);
    }
    let last = &index["resources"][256];
    assert_eq!(last["tile"]["index"], 0);
    assert_eq!(last["tile"]["sheet"], "contact_sheet-A-0002.png");
}

#[test]
fn no_clobber_and_validation_failure_publish_nothing() {
    let root = fixture(1, 0x10);
    result(preview(root.path(), &[]), 0);
    let path = root.path().join("theme/previews/preview_index-A.json");
    let original = fs::read(&path).unwrap();
    result(preview(root.path(), &[]), 1);
    assert_eq!(fs::read(&path).unwrap(), original);
    result(preview(root.path(), &["--force"]), 0);
    assert_eq!(fs::read(&path).unwrap(), original);
    result(
        run(
            root.path(),
            &[
                "target",
                "add",
                "B",
                "--theme",
                "theme",
                "--firmware",
                "firmware.bin",
                "--json",
            ],
        ),
        0,
    );
    let failed = result(
        run(
            root.path(),
            &[
                "preview",
                "--theme",
                "theme",
                "--all-targets",
                "--output",
                "new-output",
                "--json",
            ],
        ),
        1,
    );
    assert_eq!(failed["outputs"], json!([]));
    assert!(!root.path().join("new-output/preview_index-A.json").exists());
    assert!(
        fs::read_dir(root.path().join("new-output"))
            .unwrap()
            .next()
            .is_none()
    );
}

#[cfg(unix)]
#[test]
fn force_cannot_overwrite_any_project_input_alias() {
    let root = fixture(1, 0x10);
    result(preview(root.path(), &[]), 0);
    let index = index(root.path());
    let name = index["resources"][0]["png"].as_str().unwrap();
    fs::create_dir(root.path().join("aliases")).unwrap();
    for (i, input) in [
        "firmware.bin",
        "theme/theme.json",
        "theme/targets/A.json",
        "theme/assets/source.png",
    ]
    .iter()
    .enumerate()
    {
        let output = root.path().join("aliases").join(name);
        let input = root.path().join(input);
        let original = fs::read(&input).unwrap();
        if i % 2 == 0 {
            fs::hard_link(&input, &output).unwrap();
        } else {
            std::os::unix::fs::symlink(&input, &output).unwrap();
        }
        let failed = result(preview(root.path(), &["--output", "aliases", "--force"]), 1);
        assert_eq!(failed["errors"][0]["code"], "protected_input");
        assert_eq!(fs::read(&input).unwrap(), original);
        fs::remove_file(output).unwrap();
    }
}

#[test]
fn selectors_are_required_and_exclusive() {
    let root = fixture(1, 0x10);
    result(
        run(root.path(), &["preview", "--theme", "theme", "--json"]),
        2,
    );
    result(preview(root.path(), &["--all-targets"]), 2);
}

#[test]
fn malicious_raw_png_dimensions_fail_without_publication() {
    let root = fixture(1, 0x10);
    // A real PNG signature with an enormous IHDR, deliberately lacking a valid CRC.
    // The dimensions probe must reject this before bitmap allocation.
    let mut png = vec![0; 33];
    png[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    png[8..12].copy_from_slice(&13u32.to_be_bytes());
    png[12..16].copy_from_slice(b"IHDR");
    png[16..20].copy_from_slice(&100_000u32.to_be_bytes());
    png[20..24].copy_from_slice(&100_000u32.to_be_bytes());
    png[24] = 8;
    png[25] = 6;
    fs::write(root.path().join("theme/assets/bomb.png"), png).unwrap();
    update(&root.path().join("theme/theme.json"), |v| {
        v["icons"]["role/00000"] = json!({"input": "assets/bomb.png", "mode": "raw"})
    });
    let failed = result(preview(root.path(), &[]), 1);
    assert_eq!(failed["errors"][0]["code"], "preview_decode");
    assert_eq!(failed["outputs"], json!([]));
    assert!(
        !root
            .path()
            .join("theme/previews/preview_index-A.json")
            .exists()
    );
}

#[test]
fn dotted_target_ids_produce_safe_contact_sheet_names() {
    let root = fixture(1, 0x10);
    fs::rename(
        root.path().join("theme/targets/A.json"),
        root.path().join("theme/targets/band.11.json"),
    )
    .unwrap();
    result(
        run(
            root.path(),
            &[
                "preview", "--theme", "theme", "--target", "band.11", "--verify", "--json",
            ],
        ),
        0,
    );
    let path = root
        .path()
        .join("theme/previews/contact_sheet-band.11-0001.png");
    assert!(path.is_file());
    assert!(
        root.path()
            .join("theme/previews/preview_index-band.11.json")
            .is_file()
    );
}

#[test]
fn malicious_raw_rle_expanded_length_is_rejected_before_allocation() {
    let root = fixture(1, 0x10);
    let mut image = template(0x10);
    image.truncate(12);
    image[2..4].copy_from_slice(&8u16.to_le_bytes());
    image.extend_from_slice(&0u32.to_le_bytes());
    image.extend_from_slice(&2u32.to_le_bytes());
    image.extend_from_slice(&u32::MAX.to_le_bytes());
    image.extend_from_slice(&[16, 0]);
    fs::write(root.path().join("theme/assets/bomb.bin"), image).unwrap();
    update(&root.path().join("theme/theme.json"), |v| {
        v["icons"]["role/00000"] = json!({"input": "assets/bomb.bin", "mode": "raw"})
    });
    let failed = result(preview(root.path(), &[]), 1);
    assert_eq!(failed["errors"][0]["code"], "preview_decode");
    assert!(
        failed["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("RLE")
    );
    assert_eq!(failed["outputs"], json!([]));
}
