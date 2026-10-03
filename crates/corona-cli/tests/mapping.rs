use std::fs;
use std::path::Path;
use std::process::Command;

use corona_core::{app_icons, crpack, lvgl, runtime};
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
    fs::write(dir.path().join("source.dat"), b"opaque\0bytes").unwrap();
    RgbaImage::from_pixel(7, 5, Rgba([13, 29, 47, 123]))
        .save(dir.path().join("source.png"))
        .unwrap();
    dir
}

fn config(root: &Path) -> Value {
    serde_json::from_slice(&fs::read(root.join("theme/targets/A.json")).unwrap()).unwrap()
}

fn save_config(root: &Path, value: &Value) {
    fs::write(
        root.join("theme/targets/A.json"),
        serde_json::to_vec_pretty(value).unwrap(),
    )
    .unwrap();
}

fn add(root: &Path, source: &str, extra: &[&str], code: i32) -> Value {
    let mut args = vec![
        "mapping", "add", "--theme", "theme", "--target", "A", "--source", source,
    ];
    args.extend_from_slice(extra);
    run(root, &args, code)
}

fn snapshot(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn visit(root: &Path, path: &Path, entries: &mut Vec<(String, Vec<u8>)>) {
        let relative = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        if fs::symlink_metadata(path).unwrap().file_type().is_symlink() {
            entries.push((
                relative,
                fs::read_link(path)
                    .unwrap()
                    .to_string_lossy()
                    .as_bytes()
                    .to_vec(),
            ));
        } else if path.is_dir() {
            entries.push((relative, vec![]));
            for entry in fs::read_dir(path).unwrap() {
                visit(root, &entry.unwrap().path(), entries);
            }
        } else {
            entries.push((relative, fs::read(path).unwrap()));
        }
    }
    let mut entries = vec![];
    visit(root, root, &mut entries);
    entries.sort();
    entries
}

#[test]
fn raw_and_png_are_copied_built_listed_and_removed_without_deleting_shared_bytes() {
    let dir = fixture();
    let root = dir.path();
    let fingerprint = config(root)["firmwareSha256"].clone();
    let raw_dest = runtime::destination("/data/custom/settings.dat");
    let png_dest = runtime::destination("/data/custom/image.bin");
    let raw = add(
        root,
        "/data/custom/settings.dat",
        &["--input", "source.dat"],
        0,
    );
    assert_eq!(raw["asset"]["mode"], "raw");
    let png = add(
        root,
        "/data/custom/image.bin",
        &["--input", "source.png"],
        0,
    );
    assert_eq!(png["asset"]["mode"], "png");
    assert!(png["asset"]["template"].is_null());
    for (result, source) in [(&raw, "source.dat"), (&png, "source.png")] {
        let input = result["asset"]["input"].as_str().unwrap();
        assert!(input.starts_with("assets/runtime-files/"));
        assert_eq!(
            fs::read(root.join("theme").join(input)).unwrap(),
            fs::read(root.join(source)).unwrap()
        );
    }
    fs::remove_file(root.join("source.dat")).unwrap();
    fs::remove_file(root.join("source.png")).unwrap();
    add(
        root,
        "/data/custom/alias.dat",
        &["--destination", &raw_dest],
        0,
    );
    let list = run(
        root,
        &["mapping", "list", "--theme", "theme", "--target", "A"],
        0,
    );
    assert_eq!(list["command"], "mapping list");
    assert_eq!(list["runtimeMappingCount"], 3);
    assert_eq!(list["runtimeFileCount"], 2);
    assert_eq!(list["mappings"][0]["source"], "/data/custom/settings.dat");
    assert_eq!(list["mappings"][2]["source"], "/data/custom/alias.dat");
    let checked = run(root, &["check", "--theme", "theme", "--target", "A"], 0);
    assert_eq!(checked["targets"][0]["runtimeFileCount"], 2);
    assert_eq!(checked["targets"][0]["runtimeMappingCount"], 3);
    run(root, &["build", "--theme", "theme", "--target", "A"], 0);
    let pack =
        crpack::parse_crpack(&fs::read(root.join("theme/dist/corona-A.crpack")).unwrap()).unwrap();
    assert_eq!(pack.replacements[&raw_dest], b"opaque\0bytes");
    let encoded = &pack.replacements[&png_dest];
    let info = lvgl::inspect_image(encoded).unwrap();
    assert_eq!((info.width, info.height), (7, 5));
    assert_eq!(
        lvgl::decode_to_rgba(encoded).unwrap().1.get_pixel(0, 0).0,
        [13, 29, 47, 123]
    );
    assert_eq!(
        pack.mappings,
        serde_json::from_value::<Vec<crpack::Mapping>>(list["mappings"].clone()).unwrap()
    );
    let before_files = config(root)["runtimeFiles"].clone();
    let owned_input = root
        .join("theme")
        .join(raw["asset"]["input"].as_str().unwrap());
    run(
        root,
        &[
            "mapping",
            "remove",
            "--theme",
            "theme",
            "--target",
            "A",
            "--source",
            "/data/custom/settings.dat",
        ],
        0,
    );
    assert_eq!(config(root)["runtimeFiles"], before_files);
    assert_eq!(fs::read(owned_input).unwrap(), b"opaque\0bytes");
    assert_eq!(
        config(root)["runtimeMappings"][0]["source"],
        "/data/custom/image.bin"
    );
    assert_eq!(config(root)["firmwareSha256"], fingerprint);
    run(root, &["check", "--theme", "theme", "--target", "A"], 0);
}

#[test]
fn explicit_raw_overrides_png_and_templates_are_copied() {
    let dir = fixture();
    let root = dir.path();
    let bytes = fs::read(root.join("source.png")).unwrap();
    let raw = add(
        root,
        "/data/png-opaque",
        &[
            "--input",
            "source.png",
            "--raw",
            "--destination",
            "custom/png.dat",
        ],
        0,
    );
    assert_eq!(raw["asset"]["mode"], "raw");
    fs::write(root.join("template.bin"), app_icons::canopus_template()).unwrap();
    RgbaImage::from_pixel(7, 7, Rgba([13, 29, 47, 123]))
        .save(root.join("square.png"))
        .unwrap();
    let png = add(
        root,
        "/data/template-image",
        &[
            "--input",
            "square.png",
            "--template",
            "template.bin",
            "--filter",
            "nearest",
        ],
        0,
    );
    assert_eq!(png["asset"]["filter"], "nearest");
    let template = root
        .join("theme")
        .join(png["asset"]["template"].as_str().unwrap());
    assert_eq!(fs::read(template).unwrap(), app_icons::canopus_template());
    fs::remove_file(root.join("source.png")).unwrap();
    fs::remove_file(root.join("template.bin")).unwrap();
    fs::remove_file(root.join("square.png")).unwrap();
    run(root, &["build", "--theme", "theme", "--target", "A"], 0);
    let pack =
        crpack::parse_crpack(&fs::read(root.join("theme/dist/corona-A.crpack")).unwrap()).unwrap();
    assert_eq!(pack.replacements["custom/png.dat"], bytes);
    let info =
        lvgl::inspect_image(&pack.replacements[&runtime::destination("/data/template-image")])
            .unwrap();
    assert_eq!((info.width, info.height), (117, 117));
}

#[test]
fn force_updates_in_place_but_never_clobbers_aliases_or_unrelated_destinations() {
    let dir = fixture();
    let root = dir.path();
    let original = add(
        root,
        "/data/one",
        &["--input", "source.dat", "--destination", "custom/one.dat"],
        0,
    );
    add(
        root,
        "/data/two",
        &["--input", "source.dat", "--destination", "custom/two.dat"],
        0,
    );
    let before = snapshot(&root.join("theme"));
    add(root, "/data/one", &["--input", "source.png"], 1);
    add(
        root,
        "/data/three",
        &[
            "--input",
            "source.png",
            "--destination",
            "custom/one.dat",
            "--force",
        ],
        1,
    );
    add(
        root,
        "/data/one",
        &[
            "--input",
            "source.png",
            "--destination",
            "custom/two.dat",
            "--force",
        ],
        1,
    );
    assert_eq!(snapshot(&root.join("theme")), before);
    add(root, "/data/alias", &["--destination", "custom/one.dat"], 0);
    let before = snapshot(&root.join("theme"));
    add(
        root,
        "/data/one",
        &[
            "--input",
            "source.png",
            "--destination",
            "custom/one.dat",
            "--force",
        ],
        1,
    );
    assert_eq!(snapshot(&root.join("theme")), before);
    add(
        root,
        "/data/one",
        &[
            "--input",
            "source.png",
            "--destination",
            "custom/new.bin",
            "--force",
        ],
        0,
    );
    assert_eq!(
        config(root)["runtimeMappings"][0]["destination"],
        "custom/new.bin"
    );
    assert_eq!(config(root)["runtimeMappings"][1]["source"], "/data/two");
    assert_eq!(
        fs::read(
            root.join("theme")
                .join(original["asset"]["input"].as_str().unwrap())
        )
        .unwrap(),
        b"opaque\0bytes"
    );
    // A now-exclusive source may explicitly replace its own declared destination.
    add(
        root,
        "/data/one",
        &[
            "--input",
            "source.dat",
            "--destination",
            "custom/new.bin",
            "--force",
        ],
        0,
    );
    assert_eq!(
        config(root)["runtimeFiles"]["custom/new.bin"]["mode"],
        "raw"
    );
}

#[test]
fn invalid_paths_conversion_and_protocol_budgets_leave_the_project_unchanged() {
    let dir = fixture();
    let root = dir.path();
    fs::write(root.join("broken.png"), b"not a png").unwrap();
    fs::write(root.join("bad-template.bin"), b"invalid").unwrap();
    let before = snapshot(&root.join("theme"));
    for (source, flags) in [
        ("../bad", vec!["--input", "source.dat"]),
        ("/data/../bad", vec!["--input", "source.dat"]),
        (
            "/data/good",
            vec!["--input", "source.dat", "--destination", "../escape"],
        ),
        (
            "/data/good",
            vec!["--input", "source.dat", "--destination", "corona.json"],
        ),
        ("/data/good", vec!["--input", "broken.png"]),
        (
            "/data/good",
            vec!["--input", "source.png", "--template", "bad-template.bin"],
        ),
        ("/data/good", vec![]),
        ("/data/good/", vec!["--input", "source.dat"]),
        ("@quickapp-icon/example", vec!["--input", "source.dat"]),
        (app_icons::CANOPUS_SOURCE, vec!["--input", "source.dat"]),
        ("/data/good", vec!["--input", "theme/targets/A.json"]),
    ] {
        add(root, source, &flags, 1);
        assert_eq!(
            snapshot(&root.join("theme")),
            before,
            "source {source}, flags {flags:?}"
        );
    }
    let long_destination = format!("custom/{}", "a".repeat(240));
    add(
        root,
        "/data/good",
        &["--input", "source.dat", "--destination", &long_destination],
        1,
    );
    assert_eq!(snapshot(&root.join("theme")), before);
    add(
        root,
        "/data/a",
        &["--input", "source.dat", "--destination", "custom/a"],
        0,
    );
    let before = snapshot(&root.join("theme"));
    add(
        root,
        "/data/child",
        &["--input", "source.dat", "--destination", "custom/a/child"],
        1,
    );
    assert_eq!(snapshot(&root.join("theme")), before);
    let mut target = config(root);
    target["runtimeMappings"] = json!(
        (0..256)
            .map(|index| json!({"source":format!("/data/{index}"), "destination":"custom/a"}))
            .collect::<Vec<_>>()
    );
    save_config(root, &target);
    let before = snapshot(&root.join("theme"));
    add(root, "/data/overflow", &["--input", "source.dat"], 1);
    assert_eq!(snapshot(&root.join("theme")), before);
    target["runtimeMappings"] = json!((0..160).map(|index| json!({"source":format!("/data/{}{index}", "s".repeat(220)), "destination":"custom/a"})).collect::<Vec<_>>());
    save_config(root, &target);
    let before = snapshot(&root.join("theme"));
    add(root, "/data/overflow", &["--input", "source.dat"], 1);
    assert_eq!(snapshot(&root.join("theme")), before);
}

#[test]
fn aliases_and_directory_rules_keep_their_order_and_declared_files() {
    let dir = fixture();
    let root = dir.path();
    add(
        root,
        "/data/a",
        &["--input", "source.dat", "--destination", "custom/tree/a"],
        0,
    );
    add(
        root,
        "/data/b",
        &["--input", "source.dat", "--destination", "custom/tree/b"],
        0,
    );
    add(root, "/data/tree/", &["--destination", "custom/tree/"], 0);
    let files = config(root)["runtimeFiles"].clone();
    run(
        root,
        &[
            "mapping", "remove", "--theme", "theme", "--target", "A", "--source", "/data/a",
        ],
        0,
    );
    assert_eq!(config(root)["runtimeFiles"], files);
    assert_eq!(config(root)["runtimeMappings"][1]["source"], "/data/tree/");
    let before = snapshot(&root.join("theme"));
    run(
        root,
        &[
            "mapping",
            "remove",
            "--theme",
            "theme",
            "--target",
            "A",
            "--source",
            "/data/missing",
        ],
        1,
    );
    assert_eq!(snapshot(&root.join("theme")), before);
}

#[cfg(unix)]
#[test]
fn unselected_runtime_inputs_templates_and_aliases_are_protected_from_forced_outputs() {
    use std::os::unix::fs::symlink;
    let dir = fixture();
    let root = dir.path();
    fs::write(root.join("external.dat"), b"runtime input").unwrap();
    fs::write(root.join("external-template.bin"), b"runtime template").unwrap();
    // Discovery is best-effort even if an unselected target is not valid.
    fs::write(root.join("theme/targets/B.json"), serde_json::to_vec(&json!({
        "runtimeFiles": {"custom/other": {"input":"../external.dat", "template":"../external-template.bin"}}
    })).unwrap()).unwrap();
    for input in ["external.dat", "external-template.bin"] {
        for hard in [false, true] {
            let alias = format!("alias-{hard}-{input}");
            if hard {
                fs::hard_link(root.join(input), root.join(&alias)).unwrap();
            } else {
                symlink(input, root.join(&alias)).unwrap();
            }
            let before = fs::read(root.join(input)).unwrap();
            let error = run(
                root,
                &[
                    "extract",
                    "--theme",
                    "theme",
                    "--target",
                    "A",
                    "--resource",
                    "test.bin",
                    "--output",
                    &alias,
                    "--force",
                ],
                1,
            );
            assert_eq!(error["errors"][0]["code"], "protected_input");
            assert_eq!(fs::read(root.join(input)).unwrap(), before);
        }
    }
    fs::remove_file(root.join("theme/targets/A.json")).unwrap();
    symlink("../../external.dat", root.join("theme/targets/A.json")).unwrap();
    let before = fs::read(root.join("external.dat")).unwrap();
    add(root, "/data/a", &["--input", "source.dat"], 1);
    assert_eq!(fs::read(root.join("external.dat")).unwrap(), before);
}

#[test]
fn png_replacement_inherits_an_immutable_native_template_and_raw_clears_it() {
    let dir = fixture();
    let root = dir.path();
    let native = app_icons::canopus_template();
    fs::write(root.join("imported.bin"), &native).unwrap();
    RgbaImage::from_pixel(7, 7, Rgba([13, 29, 47, 123]))
        .save(root.join("edited.png"))
        .unwrap();
    let imported = add(
        root,
        "/data/imported-image",
        &[
            "--input",
            "imported.bin",
            "--destination",
            "custom/original.bin",
        ],
        0,
    );
    let original_input = root
        .join("theme")
        .join(imported["asset"]["input"].as_str().unwrap());
    let replacement = add(
        root,
        "/data/imported-image",
        &["--input", "edited.png", "--force"],
        0,
    );
    assert_eq!(replacement["destination"], "custom/original.bin");
    assert_eq!(replacement["asset"]["mode"], "png");
    let template = root
        .join("theme")
        .join(replacement["asset"]["template"].as_str().unwrap());
    assert_eq!(fs::read(&template).unwrap(), native);
    assert_eq!(fs::read(&original_input).unwrap(), native);
    fs::remove_file(root.join("imported.bin")).unwrap();
    let repeated = add(
        root,
        "/data/imported-image",
        &["--input", "edited.png", "--force"],
        0,
    );
    let repeated_template = root
        .join("theme")
        .join(repeated["asset"]["template"].as_str().unwrap());
    assert_ne!(template, repeated_template);
    assert_eq!(fs::read(&repeated_template).unwrap(), native);
    run(root, &["build", "--theme", "theme", "--target", "A"], 0);
    let pack =
        crpack::parse_crpack(&fs::read(root.join("theme/dist/corona-A.crpack")).unwrap()).unwrap();
    let info = lvgl::inspect_image(&pack.replacements["custom/original.bin"]).unwrap();
    assert_eq!((info.width, info.height), (117, 117));
    let raw = add(
        root,
        "/data/imported-image",
        &["--input", "edited.png", "--raw", "--force"],
        0,
    );
    assert_eq!(raw["asset"]["mode"], "raw");
    assert!(raw["asset"]["template"].is_null());
    assert_eq!(fs::read(original_input).unwrap(), native);
    assert_eq!(fs::read(template).unwrap(), native);
    assert_eq!(fs::read(repeated_template).unwrap(), native);
}

#[test]
fn imported_native_rules_and_quickapp_declarations_are_editable_in_place() {
    let dir = fixture();
    let root = dir.path();
    let native = app_icons::canopus_template();
    fs::write(root.join("original.bin"), &native).unwrap();
    RgbaImage::from_pixel(7, 7, Rgba([13, 29, 47, 123]))
        .save(root.join("edited.png"))
        .unwrap();
    let mut target = config(root);
    target["runtimeFiles"] = json!({
        "custom/canopus.bin":{"input":"../original.bin", "mode":"raw"},
        "custom/mapped.bin":{"input":"../original.bin", "mode":"raw"},
        "custom/declared.bin":{"input":"../original.bin", "mode":"raw"}
    });
    target["runtimeMappings"] = json!([
        {"source":app_icons::CANOPUS_SOURCE, "destination":"custom/canopus.bin"},
        {"source":"@quickapp-icon/org.example.mapped/", "destination":"custom/mapped.bin"}
    ]);
    target["runtimeQuickappIcons"] =
        json!([{"package":"org.example.declared/", "destination":"custom/declared.bin"}]);
    save_config(root, &target);
    let before = snapshot(&root.join("theme"));
    add(
        root,
        "@quickapp-icon/org.example.declared/",
        &["--input", "edited.png"],
        1,
    );
    add(
        root,
        "@quickapp-icon/org.example.declared/",
        &[
            "--input",
            "edited.png",
            "--force",
            "--destination",
            "custom/not-bin.dat",
        ],
        1,
    );
    assert_eq!(snapshot(&root.join("theme")), before);
    for source in [
        app_icons::CANOPUS_SOURCE,
        "@quickapp-icon/org.example.mapped/",
        "@quickapp-icon/org.example.declared/",
    ] {
        let updated = add(root, source, &["--input", "edited.png", "--force"], 0);
        let template = root
            .join("theme")
            .join(updated["asset"]["template"].as_str().unwrap());
        assert_eq!(fs::read(template).unwrap(), native);
    }
    assert_eq!(config(root)["runtimeMappings"].as_array().unwrap().len(), 2);
    assert_eq!(
        config(root)["runtimeQuickappIcons"],
        target["runtimeQuickappIcons"]
    );
    add(
        root,
        "@quickapp-icon/org.example.declared/",
        &["--destination", "custom/mapped.bin", "--force"],
        0,
    );
    assert_eq!(
        config(root)["runtimeQuickappIcons"][0]["destination"],
        "custom/mapped.bin"
    );
    let shared = config(root)["runtimeFiles"]["custom/mapped.bin"].clone();
    let changed = add(
        root,
        "@quickapp-icon/org.example.declared/",
        &[
            "--input",
            "edited.png",
            "--force",
            "--destination",
            "custom/changed.bin",
        ],
        0,
    );
    assert_eq!(config(root)["runtimeFiles"]["custom/mapped.bin"], shared);
    assert_eq!(
        config(root)["runtimeQuickappIcons"][0]["destination"],
        "custom/changed.bin"
    );
    assert_eq!(
        fs::read(
            root.join("theme")
                .join(changed["asset"]["template"].as_str().unwrap())
        )
        .unwrap(),
        native
    );
    assert_eq!(config(root)["runtimeMappings"].as_array().unwrap().len(), 2);
    run(root, &["build", "--theme", "theme", "--target", "A"], 0);
    let files = config(root)["runtimeFiles"].clone();
    for source in [
        app_icons::CANOPUS_SOURCE,
        "@quickapp-icon/org.example.mapped/",
        "@quickapp-icon/org.example.declared/",
    ] {
        run(
            root,
            &[
                "mapping", "remove", "--theme", "theme", "--target", "A", "--source", source,
            ],
            0,
        );
    }
    assert_eq!(config(root)["runtimeFiles"], files);
    assert_eq!(config(root)["runtimeMappings"], json!([]));
    assert_eq!(config(root)["runtimeQuickappIcons"], json!([]));
    assert_eq!(fs::read(root.join("original.bin")).unwrap(), native);
}

#[test]
fn actual_preflight_rejects_manifest_and_aggregate_budgets_without_project_mutation() {
    let dir = fixture();
    let root = dir.path();
    fs::File::create(root.join("full.bin"))
        .unwrap()
        .set_len(64 * 1024 * 1024)
        .unwrap();
    let before = snapshot(&root.join("theme"));
    let error = add(root, "/data/full", &["--input", "full.bin"], 1);
    assert_eq!(error["errors"][0]["code"], "validation");
    assert!(
        error["targets"][0]["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|error| error["code"] == "pack_validation")
    );
    assert_eq!(snapshot(&root.join("theme")), before);
    fs::File::create(root.join("forty.bin"))
        .unwrap()
        .set_len(40 * 1024 * 1024)
        .unwrap();
    fs::File::create(root.join("thirty.bin"))
        .unwrap()
        .set_len(30 * 1024 * 1024)
        .unwrap();
    let mut target = config(root);
    target["runtimeFiles"] = json!({"custom/a.bin":{"input":"../forty.bin", "mode":"raw"}});
    target["runtimeMappings"] = json!([{"source":"/data/existing", "destination":"custom/a.bin"}]);
    save_config(root, &target);
    let before = snapshot(&root.join("theme"));
    let error = add(root, "/data/new", &["--input", "thirty.bin"], 1);
    assert_eq!(error["errors"][0]["code"], "validation");
    assert_eq!(snapshot(&root.join("theme")), before);

    RgbaImage::from_pixel(7, 7, Rgba([13, 29, 47, 123]))
        .save(root.join("square.png"))
        .unwrap();
    fs::write(
        root.join("padded-template.bin"),
        app_icons::canopus_template(),
    )
    .unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(root.join("padded-template.bin"))
        .unwrap()
        .set_len(40 * 1024 * 1024)
        .unwrap();
    target["runtimeFiles"] = json!({"custom/a.bin":{"input":"../square.png", "mode":"png", "template":"../padded-template.bin"}});
    save_config(root, &target);
    let before = snapshot(&root.join("theme"));
    let error = add(
        root,
        "/data/new",
        &["--input", "square.png", "--template", "padded-template.bin"],
        1,
    );
    assert_eq!(error["errors"][0]["code"], "validation");
    assert_eq!(snapshot(&root.join("theme")), before);

    // Small compressed sources can still exceed the aggregate encoded-byte cap.
    RgbaImage::from_pixel(2900, 2900, Rgba([0, 0, 0, 0]))
        .save(root.join("large.png"))
        .unwrap();
    target["runtimeFiles"] = json!({"custom/a.bin":{"input":"../large.png", "mode":"png"}});
    save_config(root, &target);
    let before = snapshot(&root.join("theme"));
    let error = add(root, "/data/new", &["--input", "large.png"], 1);
    assert_eq!(error["errors"][0]["code"], "validation");
    assert_eq!(snapshot(&root.join("theme")), before);
}

#[test]
fn imported_directory_rules_and_new_firmware_bindings_use_exact_exceptions() {
    let dir = fixture();
    let root = dir.path();
    let mut firmware = vec![0; 100];
    firmware[..8].copy_from_slice(b"-rom1fs-");
    firmware[8..12].copy_from_slice(&100u32.to_be_bytes());
    firmware[16..24].copy_from_slice(b"resource");
    firmware[32..36].copy_from_slice(&1u32.to_be_bytes());
    firmware[36..40].copy_from_slice(&64u32.to_be_bytes());
    firmware[48..51].copy_from_slice(b"app");
    firmware[64..68].copy_from_slice(&2u32.to_be_bytes());
    firmware[72..76].copy_from_slice(&4u32.to_be_bytes());
    firmware[80..87].copy_from_slice(b"new.bin");
    fs::write(root.join("nested.bin"), firmware).unwrap();
    run(
        root,
        &[
            "target",
            "add",
            "A",
            "--theme",
            "theme",
            "--firmware",
            "nested.bin",
            "--force",
        ],
        0,
    );
    let mut theme: Value =
        serde_json::from_slice(&fs::read(root.join("theme/theme.json")).unwrap()).unwrap();
    theme["icons"] = json!({"new-role":{"input":"../source.dat", "mode":"raw"}});
    fs::write(
        root.join("theme/theme.json"),
        serde_json::to_vec(&theme).unwrap(),
    )
    .unwrap();
    let mut target = config(root);
    target["bindings"] = json!({"new-role":"app/new.bin"});
    target["runtimeFiles"] =
        json!({"custom/originals/original.bin":{"input":"../source.dat", "mode":"raw"}});
    target["runtimeMappings"] =
        json!([{"source":"/resource/app/", "destination":"custom/originals/"}]);
    save_config(root, &target);
    add(root, "/data/extra", &["--input", "source.dat"], 0);
    run(root, &["build", "--theme", "theme", "--target", "A"], 0);
    let pack =
        crpack::parse_crpack(&fs::read(root.join("theme/dist/corona-A.crpack")).unwrap()).unwrap();
    assert_eq!(pack.mappings[0].source, "/resource/app/new.bin");
    assert_eq!(pack.mappings[1].source, "/resource/app/");
    assert_eq!(pack.mappings[2].source, "/data/extra");
}

#[test]
fn forced_firmware_switch_retains_runtime_declarations_and_rejects_malformed_configs() {
    let dir = fixture();
    let root = dir.path();
    add(
        root,
        "/data/custom",
        &[
            "--input",
            "source.dat",
            "--destination",
            "custom/shared.bin",
        ],
        0,
    );
    let mut previous = config(root);
    previous["runtimeQuickappIcons"] =
        json!([{"package":"org.example.original", "destination":"custom/shared.bin"}]);
    previous["bindings"] = json!({"old-role":"test.bin"});
    previous["overrides"] = json!({"old-role":{"input":"../source.dat", "mode":"raw"}});
    save_config(root, &previous);
    let mut firmware = fs::read(root.join("firmware.bin")).unwrap();
    firmware[64] = 123;
    fs::write(root.join("second.bin"), firmware).unwrap();
    run(
        root,
        &[
            "target",
            "add",
            "A",
            "--theme",
            "theme",
            "--firmware",
            "second.bin",
            "--force",
        ],
        0,
    );
    let switched = config(root);
    for field in ["runtimeFiles", "runtimeMappings", "runtimeQuickappIcons"] {
        assert_eq!(switched[field], previous[field]);
    }
    assert_ne!(switched["firmwareSha256"], previous["firmwareSha256"]);
    assert_eq!(switched["bindings"], json!({}));
    assert_eq!(switched["overrides"], json!({}));
    run(root, &["check", "--theme", "theme", "--target", "A"], 0);
    let mut malformed = switched;
    malformed["runtimeFiles"]["custom/shared.bin"]["mode"] = json!("unknown-mode");
    save_config(root, &malformed);
    let before = snapshot(&root.join("theme"));
    run(
        root,
        &[
            "target",
            "add",
            "A",
            "--theme",
            "theme",
            "--firmware",
            "firmware.bin",
            "--force",
        ],
        1,
    );
    assert_eq!(snapshot(&root.join("theme")), before);
}

#[test]
fn oversized_raw_and_template_inputs_fail_without_creating_assets() {
    let dir = fixture();
    let root = dir.path();
    fs::File::create(root.join("oversized.bin"))
        .unwrap()
        .set_len(64 * 1024 * 1024 + 1)
        .unwrap();
    let before = snapshot(&root.join("theme"));
    add(root, "/data/oversized", &["--input", "oversized.bin"], 1);
    add(
        root,
        "/data/oversized",
        &["--input", "source.png", "--template", "oversized.bin"],
        1,
    );
    assert_eq!(snapshot(&root.join("theme")), before);
}

#[test]
fn imported_custom_quickapp_declarations_are_preserved_and_protect_shared_destinations() {
    let dir = fixture();
    let root = dir.path();
    add(
        root,
        "/data/imported",
        &[
            "--input",
            "source.dat",
            "--destination",
            "custom/imported.bin",
        ],
        0,
    );
    let mut target = config(root);
    let icons = json!([{"package":"org.example.imported", "destination":"custom/imported.bin"}]);
    target["runtimeQuickappIcons"] = icons.clone();
    save_config(root, &target);
    let before = snapshot(&root.join("theme"));
    add(
        root,
        "/data/imported",
        &["--input", "source.png", "--force"],
        1,
    );
    assert_eq!(snapshot(&root.join("theme")), before);
    add(root, "/data/other", &["--input", "source.dat"], 0);
    assert_eq!(config(root)["runtimeQuickappIcons"], icons);
    let list = run(
        root,
        &["mapping", "list", "--theme", "theme", "--target", "A"],
        0,
    );
    assert_eq!(list["runtimeQuickappIcons"], icons);
    assert_eq!(list["runtimeQuickappIconCount"], 1);
    run(root, &["build", "--theme", "theme", "--target", "A"], 0);
    let pack =
        crpack::parse_crpack(&fs::read(root.join("theme/dist/corona-A.crpack")).unwrap()).unwrap();
    assert_eq!(pack.quickapp_icons[0].destination, "custom/imported.bin");
}

#[cfg(unix)]
#[test]
fn redirected_asset_directories_are_rejected_before_publication() {
    use std::os::unix::fs::symlink;
    let dir = fixture();
    let root = dir.path();
    fs::create_dir(root.join("outside")).unwrap();
    symlink(
        root.join("outside"),
        root.join("theme/assets/runtime-files"),
    )
    .unwrap();
    let before = snapshot(&root.join("theme"));
    let error = add(root, "/data/a", &["--input", "source.dat"], 1);
    assert_eq!(error["errors"][0]["code"], "protected_input");
    assert_eq!(snapshot(&root.join("theme")), before);
    assert_eq!(fs::read_dir(root.join("outside")).unwrap().count(), 0);
}
