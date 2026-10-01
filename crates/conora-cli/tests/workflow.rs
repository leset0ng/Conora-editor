use std::fs;
use std::io::Cursor;
use std::path::Path;
use std::process::{Command, Output};

use conora_core::crpack::parse_crpack;
use conora_core::lvgl;
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use serde_json::{Value, json};
use tempfile::TempDir;

fn run(root: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_conora"))
        .current_dir(root)
        .args(arguments)
        .output()
        .unwrap()
}

fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schemaVersion"], 1);
    assert_eq!(value["ok"], true);
    value
}

fn failure(output: Output, status: i32) -> Value {
    assert_eq!(
        output.status.code(),
        Some(status),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schemaVersion"], 1);
    assert_eq!(value["ok"], false);
    value
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

fn firmware(directory: &str, image: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0; 128 + image.len()];
    bytes[..8].copy_from_slice(b"-rom1fs-");
    let length = bytes.len() as u32;
    bytes[8..12].copy_from_slice(&length.to_be_bytes());
    bytes[16..24].copy_from_slice(b"resource");
    bytes[32..36].copy_from_slice(&1u32.to_be_bytes());
    bytes[36..40].copy_from_slice(&64u32.to_be_bytes());
    bytes[48..51].copy_from_slice(b"app");
    bytes[64..68].copy_from_slice(&1u32.to_be_bytes());
    bytes[68..72].copy_from_slice(&96u32.to_be_bytes());
    bytes[80..85].copy_from_slice(directory.as_bytes());
    bytes[96..100].copy_from_slice(&2u32.to_be_bytes());
    bytes[104..108].copy_from_slice(&(image.len() as u32).to_be_bytes());
    bytes[112..120].copy_from_slice(b"test.bin");
    bytes[128..].copy_from_slice(image);
    bytes
}

fn update_json(path: &Path, update: impl FnOnce(&mut Value)) {
    let mut value: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    update(&mut value);
    fs::write(path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
}

fn project() -> TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("A.bin"),
        firmware("icons", &template(2, 0x0a)),
    )
    .unwrap();
    fs::write(
        root.path().join("B.bin"),
        firmware("other", &template(4, 0x10)),
    )
    .unwrap();
    success(run(
        root.path(),
        &[
            "init",
            "my-icons",
            "--theme-id",
            "dark",
            "--name",
            "Dark Icons",
            "--firmware",
            "A.bin",
            "--target",
            "A",
            "--json",
        ],
    ));
    success(run(
        root.path(),
        &[
            "target",
            "add",
            "B",
            "--theme",
            "my-icons",
            "--firmware",
            "B.bin",
            "--json",
        ],
    ));
    let mut png = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(RgbaImage::from_pixel(4, 4, Rgba([255, 0, 0, 255])))
        .write_to(&mut png, ImageFormat::Png)
        .unwrap();
    fs::write(
        root.path().join("my-icons/assets/confirm.png"),
        png.into_inner(),
    )
    .unwrap();
    update_json(&root.path().join("my-icons/theme.json"), |value| {
        value["icons"] = json!({"confirm": "assets/confirm.png"});
    });
    for (id, directory) in [("A", "icons"), ("B", "other")] {
        update_json(
            &root.path().join(format!("my-icons/targets/{id}.json")),
            |value| {
                value["bindings"] = json!({"confirm": format!("app/{directory}/test.bin")});
            },
        );
    }
    root
}

#[test]
fn malformed_unselected_targets_do_not_block_selected_target_workflows() {
    let root = project();
    fs::write(root.path().join("my-icons/targets/B.json"), b"{broken").unwrap();
    success(run(
        root.path(),
        &["ls", "--theme", "my-icons", "--target", "A", "--json"],
    ));
    success(run(
        root.path(),
        &["check", "--theme", "my-icons", "--target", "A", "--json"],
    ));
    success(run(
        root.path(),
        &["build", "--theme", "my-icons", "--target", "A", "--json"],
    ));
    failure(
        run(
            root.path(),
            &["check", "--theme", "my-icons", "--all-targets", "--json"],
        ),
        1,
    );
    let path = root.path().join("my-icons/targets/B.json");
    failure(
        run(
            root.path(),
            &[
                "extract",
                "--theme",
                "my-icons",
                "--target",
                "A",
                "--resource",
                "app/icons/test.bin",
                "--output",
                "my-icons/targets/B.json",
                "--force",
                "--json",
            ],
        ),
        1,
    );
    assert_eq!(fs::read(path).unwrap(), b"{broken");
}

#[cfg(unix)]
#[test]
fn generated_firmware_paths_preserve_literal_unix_backslashes() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("A\\B.bin"),
        firmware("icons", &template(2, 0x0a)),
    )
    .unwrap();
    success(run(
        root.path(),
        &[
            "init",
            "theme",
            "--firmware",
            "A\\B.bin",
            "--target",
            "A",
            "--json",
        ],
    ));
    let path = root.path().join("theme/targets/A.json");
    let config: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert!(config["firmware"].as_str().unwrap().contains("A\\B.bin"));
    success(run(
        root.path(),
        &["ls", "--theme", "theme", "--target", "A", "--json"],
    ));
}

#[cfg(unix)]
#[test]
fn non_utf8_output_paths_fail_before_mutation_with_a_json_error() {
    use std::os::unix::ffi::OsStringExt;
    let root = tempfile::tempdir().unwrap();
    let path = root
        .path()
        .join(std::ffi::OsString::from_vec(b"nonutf8-\xff".to_vec()));
    let output = Command::new(env!("CARGO_BIN_EXE_conora"))
        .arg("init")
        .arg(&path)
        .arg("--json")
        .output()
        .unwrap();
    failure(output, 1);
    assert!(!path.exists());
}

#[test]
fn ai_can_initialize_inspect_extract_check_and_build_multiple_firmwares() {
    let root = project();
    let listing = success(run(
        root.path(),
        &[
            "ls", "--theme", "my-icons", "--target", "A", "--images", "--json",
        ],
    ));
    assert!(listing.to_string().contains("app/icons/test.bin"));
    success(run(
        root.path(),
        &[
            "extract",
            "--theme",
            "my-icons",
            "--target",
            "A",
            "--resource",
            "app/icons/test.bin",
            "--as",
            "png",
            "--output",
            "my-icons/previews/confirm.png",
            "--json",
        ],
    ));
    let preview = fs::read(root.path().join("my-icons/previews/confirm.png")).unwrap();
    assert!(preview.starts_with(b"\x89PNG\r\n\x1a\n"));
    success(run(
        root.path(),
        &["check", "--theme", "my-icons", "--all-targets", "--json"],
    ));
    assert!(!root.path().join("my-icons/dist").exists());
    success(run(
        root.path(),
        &["build", "--theme", "my-icons", "--all-targets", "--json"],
    ));
    for (id, directory, size) in [("A", "icons", 2), ("B", "other", 4)] {
        let path = root.path().join(format!("my-icons/dist/dark-{id}.crpack"));
        let pack = parse_crpack(&fs::read(path).unwrap()).unwrap();
        assert_eq!(pack.replacements.len(), 1);
        let (info, _) =
            lvgl::decode_image_png(&pack.replacements[&format!("app/{directory}/test.bin")])
                .unwrap();
        assert_eq!((info.width, info.height), (size, size));
        let inspected = success(run(
            root.path(),
            &[
                "inspect",
                &format!("my-icons/dist/dark-{id}.crpack"),
                "--json",
            ],
        ));
        assert_eq!(inspected["manifest"]["format"], "canopus-resource-pack");
        assert_eq!(inspected["mappings"].as_array().unwrap().len(), 1);
        assert_eq!(inspected["files"].as_array().unwrap().len(), 2);
    }
}

#[test]
fn no_target_is_published_when_another_target_fails() {
    let root = project();
    update_json(&root.path().join("my-icons/targets/B.json"), |value| {
        value["bindings"]["confirm"] = json!("absent.bin");
    });
    let report = failure(
        run(
            root.path(),
            &["build", "--theme", "my-icons", "--all-targets", "--json"],
        ),
        1,
    );
    assert!(report.to_string().contains("missing_resource"));
    assert!(!root.path().join("my-icons/dist/dark-A.crpack").exists());
    assert!(!root.path().join("my-icons/dist/dark-B.crpack").exists());
}

#[test]
fn existing_outputs_require_force_and_survive_failed_rebuilds() {
    let root = project();
    let arguments = ["build", "--theme", "my-icons", "--all-targets", "--json"];
    success(run(root.path(), &arguments));
    let path = root.path().join("my-icons/dist/dark-A.crpack");
    let original = fs::read(&path).unwrap();
    failure(run(root.path(), &arguments), 1);
    assert_eq!(fs::read(&path).unwrap(), original);
    success(run(
        root.path(),
        &[
            "build",
            "--theme",
            "my-icons",
            "--all-targets",
            "--force",
            "--json",
        ],
    ));
    update_json(&root.path().join("my-icons/targets/B.json"), |value| {
        value["firmwareSha256"] = json!("0".repeat(64));
    });
    failure(
        run(
            root.path(),
            &[
                "build",
                "--theme",
                "my-icons",
                "--all-targets",
                "--force",
                "--json",
            ],
        ),
        1,
    );
    assert_eq!(fs::read(&path).unwrap(), original);
}

#[test]
fn protected_sources_cannot_be_overwritten_even_with_force() {
    let root = project();
    for path in [
        "A.bin",
        "my-icons/theme.json",
        "my-icons/targets/B.json",
        "my-icons/assets/confirm.png",
    ] {
        let original = fs::read(root.path().join(path)).unwrap();
        failure(
            run(
                root.path(),
                &[
                    "extract",
                    "--theme",
                    "my-icons",
                    "--target",
                    "A",
                    "--resource",
                    "app/icons/test.bin",
                    "--output",
                    path,
                    "--force",
                    "--json",
                ],
            ),
            1,
        );
        assert_eq!(fs::read(root.path().join(path)).unwrap(), original);
    }
    fs::write(
        root.path().join("my-icons/assets/dark-A.crpack"),
        b"raw replacement",
    )
    .unwrap();
    update_json(&root.path().join("my-icons/theme.json"), |value| {
        value["icons"]["confirm"] = json!({"input": "assets/dark-A.crpack", "mode": "raw"});
    });
    failure(
        run(
            root.path(),
            &[
                "build",
                "--theme",
                "my-icons",
                "--target",
                "A",
                "--output",
                "my-icons/assets",
                "--force",
                "--json",
            ],
        ),
        1,
    );
    assert_eq!(
        fs::read(root.path().join("my-icons/assets/dark-A.crpack")).unwrap(),
        b"raw replacement"
    );
}

#[test]
fn pinning_records_only_whole_firmware_sha256_and_refuses_accidental_repinning() {
    let root = project();
    let path = root.path().join("my-icons/targets/A.json");
    let original = fs::read(&path).unwrap();
    let config: Value = serde_json::from_slice(&original).unwrap();
    assert_eq!(config["firmwareSha256"].as_str().unwrap().len(), 64);
    assert_eq!(config["bindings"]["confirm"], "app/icons/test.bin");
    failure(
        run(
            root.path(),
            &[
                "target",
                "add",
                "A",
                "--theme",
                "my-icons",
                "--firmware",
                "B.bin",
                "--json",
            ],
        ),
        1,
    );
    assert_eq!(fs::read(path).unwrap(), original);
    failure(run(root.path(), &["init", "my-icons", "--json"]), 1);
}

#[test]
fn json_usage_errors_and_target_selection_are_machine_readable() {
    let root = project();
    failure(
        run(root.path(), &["check", "--theme", "my-icons", "--json"]),
        2,
    );
    failure(
        run(
            root.path(),
            &[
                "check",
                "--theme",
                "my-icons",
                "--target",
                "A",
                "--all-targets",
                "--json",
            ],
        ),
        2,
    );
    failure(run(root.path(), &["--json", "does-not-exist"]), 2);
    failure(
        run(
            root.path(),
            &[
                "ls",
                "--firmware",
                "A.bin",
                "--theme",
                "my-icons",
                "--target",
                "A",
                "--json",
            ],
        ),
        2,
    );
    let help = run(root.path(), &["--help"]);
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("build"));
}

#[test]
fn extraction_without_a_project_is_read_only_and_refuses_overwrites() {
    let root = project();
    success(run(
        root.path(),
        &[
            "extract",
            "--firmware",
            "A.bin",
            "--resource",
            "app/icons/test.bin",
            "--output",
            "original.bin",
            "--json",
        ],
    ));
    assert_eq!(
        fs::read(root.path().join("original.bin")).unwrap(),
        template(2, 0x0a)
    );
    failure(
        run(
            root.path(),
            &[
                "extract",
                "--firmware",
                "A.bin",
                "--resource",
                "app/icons/test.bin",
                "--output",
                "original.bin",
                "--json",
            ],
        ),
        1,
    );
    failure(
        run(
            root.path(),
            &[
                "extract",
                "--firmware",
                "A.bin",
                "--resource",
                "app/icons/test.bin",
                "--output",
                "A.bin",
                "--force",
                "--json",
            ],
        ),
        1,
    );
}
