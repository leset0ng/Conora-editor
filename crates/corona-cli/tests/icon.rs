use std::fs;
use std::path::Path;
use std::process::Command;

use corona_core::{app_icons, crpack};
use image::{Rgba, RgbaImage};
use serde_json::{Value, json};

fn run(root: &Path, args: &[&str], code: i32) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_corona"))
        .current_dir(root)
        .args(args)
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(code),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["ok"], code == 0);
    value
}

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    // Synthetic one-file ROMFS; native icons must not depend on its contents.
    let mut firmware = vec![0; 68];
    firmware[..8].copy_from_slice(b"-rom1fs-");
    firmware[8..12].copy_from_slice(&68u32.to_be_bytes());
    firmware[16..24].copy_from_slice(b"resource");
    firmware[32..36].copy_from_slice(&2u32.to_be_bytes());
    firmware[40..44].copy_from_slice(&4u32.to_be_bytes());
    firmware[48..56].copy_from_slice(b"test.bin");
    fs::write(dir.path().join("firmware.bin"), firmware).unwrap();
    run(
        dir.path(),
        &[
            "init",
            "theme",
            "--firmware",
            "firmware.bin",
            "--target",
            "A",
        ],
        0,
    );
    RgbaImage::from_pixel(8, 8, Rgba([255, 0, 0, 255]))
        .save(dir.path().join("source.png"))
        .unwrap();
    fs::write(
        dir.path().join("original.bin"),
        app_icons::canopus_template(),
    )
    .unwrap();
    dir
}

fn config(root: &Path) -> Value {
    serde_json::from_slice(&fs::read(root.join("theme/theme.json")).unwrap()).unwrap()
}

#[test]
fn png_and_raw_native_icons_check_preview_build_and_remove() {
    let dir = fixture();
    let root = dir.path();
    let firmware = fs::read(root.join("firmware.bin")).unwrap();
    run(
        root,
        &["icon", "set", "--canopus", "source.png", "--theme", "theme"],
        0,
    );
    run(
        root,
        &[
            "icon",
            "set",
            "--package",
            "org.example.app",
            "source.png",
            "--template",
            "original.bin",
            "--theme",
            "theme",
        ],
        0,
    );
    let theme = config(root);
    let input = theme["canopusIcon"]["input"].as_str().unwrap();
    assert!(input.starts_with("assets/native-icons/"));
    assert_eq!(
        fs::read(root.join("theme").join(input)).unwrap(),
        fs::read(root.join("source.png")).unwrap()
    );
    let template = theme["quickappIcons"]["org.example.app"]["template"]
        .as_str()
        .unwrap();
    assert_eq!(
        fs::read(root.join("theme").join(template)).unwrap(),
        app_icons::canopus_template()
    );
    // External sources may go away after a successful set.
    fs::remove_file(root.join("source.png")).unwrap();
    fs::remove_file(root.join("original.bin")).unwrap();
    let listed = run(root, &["icon", "ls", "--theme", "theme"], 0);
    assert_eq!(listed["command"], "icon ls");
    assert_eq!(listed["icons"].as_array().unwrap().len(), 2);
    run(root, &["check", "--theme", "theme", "--target", "A"], 0);
    run(
        root,
        &["preview", "--theme", "theme", "--target", "A", "--verify"],
        0,
    );
    let index: Value = serde_json::from_slice(
        &fs::read(root.join("theme/previews/preview_index-A.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(index["verified"], true);
    assert_eq!(index["resources"].as_array().unwrap().len(), 2);
    run(root, &["build", "--theme", "theme", "--target", "A"], 0);
    let built =
        crpack::parse_crpack(&fs::read(root.join("theme/dist/corona-A.crpack")).unwrap()).unwrap();
    assert!(
        built
            .replacements
            .contains_key(app_icons::CANOPUS_DESTINATION)
    );
    let manifest: Value = serde_json::from_slice(&built.manifest_bytes).unwrap();
    assert_eq!(manifest["quickappIcons"][0]["package"], "org.example.app");
    assert_eq!(
        manifest["quickappIcons"][0]["destination"],
        app_icons::destination("org.example.app")
    );
    fs::write(root.join("replacement.bin"), app_icons::canopus_template()).unwrap();
    run(
        root,
        &[
            "icon",
            "set",
            "--package",
            "org.example.app",
            "replacement.bin",
            "--raw",
            "--theme",
            "theme",
        ],
        0,
    );
    assert_eq!(
        config(root)["quickappIcons"]["org.example.app"]["mode"],
        "raw"
    );
    run(
        root,
        &[
            "icon",
            "remove",
            "--package",
            "org.example.app",
            "--theme",
            "theme",
        ],
        0,
    );
    run(
        root,
        &["icon", "remove", "--canopus", "--theme", "theme"],
        0,
    );
    assert!(
        root.join("theme").join(input).exists(),
        "remove must retain owned artwork"
    );
    assert_eq!(fs::read(root.join("firmware.bin")).unwrap(), firmware);
    run(
        root,
        &["icon", "remove", "--canopus", "--theme", "theme"],
        1,
    );
}

#[test]
fn invalid_selectors_templates_inputs_and_packages_do_not_mutate_config() {
    let dir = fixture();
    let root = dir.path();
    let before = fs::read(root.join("theme/theme.json")).unwrap();
    for args in [
        vec!["icon", "set", "source.png", "--theme", "theme"],
        vec![
            "icon",
            "set",
            "--canopus",
            "--package",
            "org.app",
            "source.png",
            "--theme",
            "theme",
        ],
        vec![
            "icon",
            "set",
            "--canopus",
            "original.bin",
            "--raw",
            "--template",
            "original.bin",
            "--theme",
            "theme",
        ],
    ] {
        run(root, &args, 2);
    }
    for args in [
        vec![
            "icon",
            "set",
            "--package",
            "org\tapp",
            "source.png",
            "--template",
            "original.bin",
            "--theme",
            "theme",
        ],
        vec![
            "icon",
            "set",
            "--package",
            "org.app",
            "firmware.bin",
            "--theme",
            "theme",
        ],
        vec![
            "icon",
            "set",
            "--canopus",
            "source.png",
            "--raw",
            "--theme",
            "theme",
        ],
        vec![
            "icon",
            "set",
            "--canopus",
            "firmware.bin",
            "--theme",
            "theme",
        ],
    ] {
        run(root, &args, 1);
    }
    let large = fs::File::create(root.join("oversized.bin")).unwrap();
    large
        .set_len(corona_core::project::MAX_TEMPLATE_BYTES as u64 + 1)
        .unwrap();
    run(
        root,
        &[
            "icon",
            "set",
            "--canopus",
            "oversized.bin",
            "--raw",
            "--theme",
            "theme",
        ],
        1,
    );
    assert_eq!(fs::read(root.join("theme/theme.json")).unwrap(), before);
    assert!(!root.join("theme/assets/native-icons").exists());
}

#[cfg(unix)]
#[test]
fn redirected_asset_directories_and_config_firmware_aliases_are_refused() {
    use std::os::unix::fs::symlink;
    let dir = fixture();
    let root = dir.path();
    fs::create_dir(root.join("foreign")).unwrap();
    symlink(root.join("foreign"), root.join("theme/assets/native-icons")).unwrap();
    run(
        root,
        &["icon", "set", "--canopus", "source.png", "--theme", "theme"],
        1,
    );
    assert_eq!(fs::read_dir(root.join("foreign")).unwrap().count(), 0);
    fs::remove_file(root.join("theme/assets/native-icons")).unwrap();
    let theme_file = root.join("theme/theme.json");
    let before = fs::read(&theme_file).unwrap();
    fs::hard_link(&theme_file, root.join("aliased.bin")).unwrap();
    let target_file = root.join("theme/targets/A.json");
    let mut target: Value = serde_json::from_slice(&fs::read(&target_file).unwrap()).unwrap();
    target["firmware"] = json!("../../aliased.bin");
    fs::write(&target_file, serde_json::to_vec(&target).unwrap()).unwrap();
    run(
        root,
        &["icon", "set", "--canopus", "source.png", "--theme", "theme"],
        1,
    );
    assert_eq!(fs::read(&theme_file).unwrap(), before);
}

#[test]
fn target_native_overrides_and_native_templates_remain_protected_outputs() {
    let dir = fixture();
    let root = dir.path();
    run(
        root,
        &["icon", "set", "--canopus", "source.png", "--theme", "theme"],
        0,
    );
    let target_file = root.join("theme/targets/A.json");
    let mut target: Value = serde_json::from_slice(&fs::read(&target_file).unwrap()).unwrap();
    target["canopusIcon"] = json!({"input":"../original.bin", "mode":"raw"});
    fs::write(&target_file, serde_json::to_vec(&target).unwrap()).unwrap();
    run(
        root,
        &["preview", "--theme", "theme", "--target", "A", "--verify"],
        0,
    );
    let index: Value = serde_json::from_slice(
        &fs::read(root.join("theme/previews/preview_index-A.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(index["resources"][0]["mode"], "raw");
    assert_eq!(
        index["resources"][0]["verification"]["status"],
        "notApplicable"
    );
    assert_eq!(index["verified"], false);
    // An explicitly referenced template is protected even outside assets/, and
    // even when its name collides with an output index and --force is supplied.
    let original = app_icons::canopus_template();
    fs::write(root.join("theme/previews/preview_index-A.json"), &original).unwrap();
    target["canopusIcon"] =
        json!({"input":"../source.png", "mode":"png", "template":"previews/preview_index-A.json"});
    fs::write(&target_file, serde_json::to_vec(&target).unwrap()).unwrap();
    let failure = run(
        root,
        &["preview", "--theme", "theme", "--target", "A", "--force"],
        1,
    );
    assert_eq!(failure["errors"][0]["code"], "protected_input");
    assert_eq!(
        fs::read(root.join("theme/previews/preview_index-A.json")).unwrap(),
        original
    );
}

#[cfg(unix)]
#[test]
fn non_utf8_inputs_and_symlink_theme_files_fail_without_asset_publication() {
    use std::os::unix::{ffi::OsStringExt, fs::symlink};
    let dir = fixture();
    let root = dir.path();
    let before = fs::read(root.join("theme/theme.json")).unwrap();
    let path = std::ffi::OsString::from_vec(b"icon-\xff.png".to_vec());
    let output = Command::new(env!("CARGO_BIN_EXE_corona"))
        .current_dir(root)
        .args(["icon", "set", "--canopus"])
        .arg(path)
        .args(["--theme", "theme", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["errors"][0]["code"], "path");
    symlink(
        root.join("theme/theme.json"),
        root.join("linked-theme.json"),
    )
    .unwrap();
    let failure = run(
        root,
        &[
            "icon",
            "set",
            "--canopus",
            "source.png",
            "--theme",
            "linked-theme.json",
        ],
        1,
    );
    assert_eq!(failure["errors"][0]["code"], "protected_input");
    assert_eq!(fs::read(root.join("theme/theme.json")).unwrap(), before);
    assert!(!root.join("theme/assets/native-icons").exists());
    fs::rename(
        root.join("theme/theme.json"),
        root.join("theme/actual-theme.json"),
    )
    .unwrap();
    symlink("actual-theme.json", root.join("theme/theme.json")).unwrap();
    let failure = run(
        root,
        &["icon", "set", "--canopus", "source.png", "--theme", "theme"],
        1,
    );
    assert_eq!(failure["errors"][0]["code"], "protected_input");
    assert_eq!(
        fs::read(root.join("theme/actual-theme.json")).unwrap(),
        before
    );
    assert!(!root.join("theme/assets/native-icons").exists());
}

#[test]
fn config_paths_declared_as_assets_or_templates_are_not_exempt_from_protection() {
    for native_template in [false, true] {
        let dir = fixture();
        let root = dir.path();
        let theme_file = root.join("theme/theme.json");
        let mut theme = config(root);
        if native_template {
            theme["canopusIcon"] =
                json!({"input":"../source.png", "mode":"png", "template":"theme.json"});
        } else {
            theme["icons"] = json!({"config_as_raw_asset":{"input":"theme.json", "mode":"raw"}});
        }
        fs::write(&theme_file, serde_json::to_vec(&theme).unwrap()).unwrap();
        let before = fs::read(&theme_file).unwrap();
        let failure = run(
            root,
            &["icon", "set", "--canopus", "source.png", "--theme", "theme"],
            1,
        );
        assert_eq!(failure["errors"][0]["code"], "protected_input");
        assert_eq!(fs::read(&theme_file).unwrap(), before);
        assert!(!root.join("theme/assets/native-icons").exists());
    }
}

#[test]
fn removing_shared_native_icons_refuses_to_orphan_any_target_override() {
    let dir = fixture();
    let root = dir.path();
    run(
        root,
        &["icon", "set", "--canopus", "source.png", "--theme", "theme"],
        0,
    );
    run(
        root,
        &[
            "icon",
            "set",
            "--package",
            "org.example.app",
            "original.bin",
            "--raw",
            "--theme",
            "theme",
        ],
        0,
    );
    let target_file = root.join("theme/targets/A.json");
    let mut target: Value = serde_json::from_slice(&fs::read(&target_file).unwrap()).unwrap();
    target["canopusIcon"] = json!({"input":"../original.bin", "mode":"raw"});
    target["quickappIcons"] = json!({"org.example.app":{"input":"../original.bin", "mode":"raw"}});
    fs::write(&target_file, serde_json::to_vec(&target).unwrap()).unwrap();
    let other_target = root.join("theme/targets/B.json");
    fs::write(&other_target, serde_json::to_vec(&target).unwrap()).unwrap();
    let theme_before = fs::read(root.join("theme/theme.json")).unwrap();
    let target_before = fs::read(&target_file).unwrap();
    let other_before = fs::read(&other_target).unwrap();
    for args in [
        vec!["icon", "remove", "--canopus", "--theme", "theme"],
        vec![
            "icon",
            "remove",
            "--package",
            "org.example.app",
            "--theme",
            "theme",
        ],
    ] {
        let failure = run(root, &args, 1);
        assert_eq!(failure["errors"][0]["code"], "icon_overridden");
        assert_eq!(failure["targetOverrides"][0]["target"], "A");
        assert_eq!(failure["targetOverrides"][1]["target"], "B");
        assert!(
            failure["errors"][0]["message"]
                .as_str()
                .unwrap()
                .contains("A, B")
        );
        assert_eq!(
            fs::read(root.join("theme/theme.json")).unwrap(),
            theme_before
        );
        assert_eq!(fs::read(&target_file).unwrap(), target_before);
        assert_eq!(fs::read(&other_target).unwrap(), other_before);
        run(root, &["check", "--theme", "theme", "--target", "A"], 0);
    }
}

#[test]
fn opaque_quickapp_ids_set_list_check_preview_build_and_remove_exactly() {
    let dir = fixture();
    let root = dir.path();
    let budget = 255 - app_icons::QUICKAPP_SOURCE_PREFIX.len();
    let packages = [
        "".into(),
        "single".into(),
        " 快应用 ".into(),
        "../../outside/".into(),
        r"C:\a:b".into(),
        "a..b".into(),
        "a\u{0085}\u{009f}".into(),
        "é".repeat(budget / 2) + &"a".repeat(budget % 2),
    ];
    for package in &packages {
        run(
            root,
            &[
                "icon",
                "set",
                "--package",
                package,
                "original.bin",
                "--raw",
                "--theme",
                "theme",
            ],
            0,
        );
        assert_eq!(config(root)["quickappIcons"][package]["mode"], "raw");
    }
    let listed = run(root, &["icon", "ls", "--theme", "theme"], 0);
    assert_eq!(listed["icons"].as_array().unwrap().len(), packages.len());
    for package in &packages {
        assert!(
            listed["icons"]
                .as_array()
                .unwrap()
                .iter()
                .any(|icon| icon["package"] == *package)
        );
    }
    run(root, &["check", "--theme", "theme", "--target", "A"], 0);
    run(root, &["build", "--theme", "theme", "--target", "A"], 0);
    let built =
        crpack::parse_crpack(&fs::read(root.join("theme/dist/corona-A.crpack")).unwrap()).unwrap();
    assert_eq!(built.quickapp_icons.len(), packages.len());
    for package in &packages {
        let destination = app_icons::destination(package);
        assert_eq!(
            built.replacements[&destination],
            app_icons::canopus_template()
        );
        assert!(
            built
                .quickapp_icons
                .iter()
                .any(|icon| icon.package == *package && icon.destination == destination)
        );
    }
    run(
        root,
        &["preview", "--theme", "theme", "--target", "A", "--verify"],
        0,
    );
    let directory = root.join("theme/previews");
    let index: Value =
        serde_json::from_slice(&fs::read(directory.join("preview_index-A.json")).unwrap()).unwrap();
    for entry in index["resources"].as_array().unwrap() {
        let name = entry["png"].as_str().unwrap();
        assert_eq!(Path::new(name).components().count(), 1);
        assert!(directory.join(name).is_file());
        let identity = entry["resource"].as_str().unwrap();
        assert!(
            packages
                .iter()
                .any(|package| app_icons::source(package) == identity)
        );
    }
    for package in &packages {
        run(
            root,
            &["icon", "remove", "--package", package, "--theme", "theme"],
            0,
        );
        assert!(config(root)["quickappIcons"].get(package).is_none());
    }
    assert_eq!(config(root)["quickappIcons"], json!({}));
}

#[test]
fn unsafe_tsv_bytes_and_overbudget_sources_do_not_publish_assets() {
    let dir = fixture();
    let root = dir.path();
    let before = fs::read(root.join("theme/theme.json")).unwrap();
    let budget = 255 - app_icons::QUICKAPP_SOURCE_PREFIX.len();
    let mut packages: Vec<String> = (1..32)
        .chain([127])
        .map(|byte| format!("a{}b", char::from(byte)))
        .collect();
    packages.extend(["a".repeat(budget + 1), "é".repeat(budget / 2 + 1)]);
    for package in &packages {
        run(
            root,
            &[
                "icon",
                "set",
                "--package",
                package,
                "original.bin",
                "--raw",
                "--theme",
                "theme",
            ],
            1,
        );
        assert_eq!(fs::read(root.join("theme/theme.json")).unwrap(), before);
        assert!(!root.join("theme/assets/native-icons").exists());
    }
    // NUL cannot be passed to a process; core/archive tests cover that byte.
}

#[test]
fn custom_resize_filter_records_in_theme_json() {
    let dir = fixture();
    let root = dir.path();
    run(
        root,
        &[
            "icon",
            "set",
            "--canopus",
            "source.png",
            "--filter",
            "nearest",
            "--theme",
            "theme",
        ],
        0,
    );
    let theme = config(root);
    assert_eq!(theme["canopusIcon"]["filter"], "nearest");
}

#[test]
fn template_free_quickapp_png_set_check_preview_verify_build_preserves_rgba() {
    use corona_core::lvgl;
    let dir = fixture();
    let root = dir.path();
    let image = RgbaImage::from_fn(301, 2, |x, y| {
        Rgba([x as u8, (x / 256) as u8, 73, (x + y) as u8])
    });
    image.save(root.join("source.png")).unwrap();
    run(
        root,
        &[
            "icon",
            "set",
            "--package",
            "org.own",
            "source.png",
            "--filter",
            "nearest",
            "--theme",
            "theme",
        ],
        0,
    );
    let theme = config(root);
    assert!(theme["quickappIcons"]["org.own"].get("template").is_none());
    fs::remove_file(root.join("source.png")).unwrap();
    fs::remove_file(root.join("original.bin")).unwrap();
    run(root, &["check", "--theme", "theme", "--target", "A"], 0);
    run(
        root,
        &["preview", "--theme", "theme", "--target", "A", "--verify"],
        0,
    );
    let previews = root.join("theme/previews");
    let index: Value =
        serde_json::from_slice(&fs::read(previews.join("preview_index-A.json")).unwrap()).unwrap();
    assert_eq!(index["verified"], true);
    let resource = &index["resources"][0];
    assert_eq!(
        resource["verification"]["contract"],
        "sourceDimensionsArgb8888"
    );
    for field in ["sameMetadata", "sameHeader", "samePixels"] {
        assert_eq!(resource["verification"][field], true);
    }
    assert_eq!(
        image::open(previews.join(resource["png"].as_str().unwrap()))
            .unwrap()
            .to_rgba8(),
        image
    );
    run(root, &["build", "--theme", "theme", "--target", "A"], 0);
    let pack =
        crpack::parse_crpack(&fs::read(root.join("theme/dist/corona-A.crpack")).unwrap()).unwrap();
    let encoded = &pack.replacements[&app_icons::destination("org.own")];
    let (info, actual) = lvgl::decode_to_rgba(encoded).unwrap();
    assert_eq!(info.format, lvgl::ImageFormatKind::Lvgl9Argb8888);
    assert_eq!((info.width, info.height, info.stride), (301, 2, 1204));
    assert_eq!(actual, image);
}

#[test]
fn template_free_quickapp_png_size_and_decode_failures_do_not_publish() {
    let dir = fixture();
    let root = dir.path();
    let before = fs::read(root.join("theme/theme.json")).unwrap();
    let png = fs::read(root.join("source.png")).unwrap();
    let mut inputs = vec![b"not a PNG".to_vec(), png[..png.len() / 2].to_vec()];
    for (width, height) in [(16384u32, 1u32), (1, 65536), (4097, 4097), (0, 1)] {
        let mut input = png.clone();
        input[16..20].copy_from_slice(&width.to_be_bytes());
        input[20..24].copy_from_slice(&height.to_be_bytes());
        let mut crc = u32::MAX;
        for &byte in &input[12..29] {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(crc & 1));
            }
        }
        input[29..33].copy_from_slice(&(!crc).to_be_bytes());
        inputs.push(input);
    }
    for input in inputs {
        fs::write(root.join("invalid.png"), input).unwrap();
        run(
            root,
            &[
                "icon",
                "set",
                "--package",
                "org.invalid",
                "invalid.png",
                "--theme",
                "theme",
            ],
            1,
        );
        assert_eq!(fs::read(root.join("theme/theme.json")).unwrap(), before);
        assert!(!root.join("theme/assets/native-icons").exists());
    }
    fs::File::create(root.join("oversized.png"))
        .unwrap()
        .set_len(corona_core::project::MAX_TEMPLATE_BYTES as u64 + 1)
        .unwrap();
    run(
        root,
        &[
            "icon",
            "set",
            "--package",
            "org.invalid",
            "oversized.png",
            "--theme",
            "theme",
        ],
        1,
    );
    assert_eq!(fs::read(root.join("theme/theme.json")).unwrap(), before);
    assert!(!root.join("theme/assets/native-icons").exists());
}
