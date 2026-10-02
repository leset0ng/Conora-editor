use std::collections::BTreeMap;
use std::io::{Cursor, Write};

use corona_core::{
    app_icons,
    crpack::{self, Mapping, PackOptions, QuickappIcon},
};
use serde_json::{Value, json};

fn options(files: &BTreeMap<String, Vec<u8>>) -> PackOptions<'_> {
    PackOptions {
        theme_id: "icons",
        name: "Application icons",
        version: None,
        version_code: None,
        author: None,
        description: None,
        target: None,
        replacements: files,
    }
}

fn archive(manifest: Value, files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default();
    writer.start_file("corona.json", opts).unwrap();
    writer
        .write_all(&serde_json::to_vec(&manifest).unwrap())
        .unwrap();
    for (path, bytes) in files {
        writer.start_file(*path, opts).unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn manifest() -> Value {
    json!({"format":"canopus-resource-pack", "formatVersion":1, "themeId":"icons", "name":"Icons", "mappings":[]})
}

#[test]
fn application_only_pack_has_no_inferred_firmware_mappings() {
    let files = BTreeMap::from([
        (
            app_icons::CANOPUS_DESTINATION.into(),
            app_icons::canopus_template(),
        ),
        (
            app_icons::destination("ng.lst.corona"),
            app_icons::canopus_template(),
        ),
    ]);
    let mappings = vec![Mapping {
        source: app_icons::CANOPUS_SOURCE.into(),
        destination: app_icons::CANOPUS_DESTINATION.into(),
    }];
    let icons = vec![QuickappIcon {
        package: "ng.lst.corona".into(),
        destination: app_icons::destination("ng.lst.corona"),
    }];
    let bytes = crpack::build_crpack_with_icons(&options(&files), &mappings, &icons).unwrap();
    let imported = crpack::parse_crpack(&bytes).unwrap();
    assert_eq!(imported.mappings, mappings);
    assert_eq!(imported.quickapp_icons, icons);
    assert_eq!(imported.replacements, files);
    let rebuilt = crpack::build_crpack_with_icons(
        &options(&imported.replacements),
        &imported.mappings,
        &imported.quickapp_icons,
    )
    .unwrap();
    let again = crpack::parse_crpack(&rebuilt).unwrap();
    assert_eq!(again.quickapp_icons, icons);
    assert_eq!(again.mappings, mappings);
}

#[test]
fn authoritative_mapping_export_keeps_unused_files_unmapped() {
    let files = BTreeMap::from([("unused.bin".into(), vec![1])]);
    let bytes = crpack::build_crpack_with_mappings(&options(&files), &[], &[]).unwrap();
    let imported = crpack::parse_crpack(&bytes).unwrap();
    assert!(imported.mappings.is_empty());
    assert_eq!(imported.replacements, files);
}

#[test]
fn multiple_application_sources_may_share_one_bin_destination() {
    let files = BTreeMap::from([("shared.bin".into(), app_icons::canopus_template())]);
    let mappings = [Mapping {
        source: app_icons::CANOPUS_SOURCE.into(),
        destination: "shared.bin".into(),
    }];
    let icons = [
        QuickappIcon {
            package: "a.b".into(),
            destination: "shared.bin".into(),
        },
        QuickappIcon {
            package: "c.d".into(),
            destination: "shared.bin".into(),
        },
    ];
    let pack = crpack::parse_crpack(
        &crpack::build_crpack_with_mappings(&options(&files), &mappings, &icons).unwrap(),
    )
    .unwrap();
    assert_eq!(pack.quickapp_icons, icons);
    assert_eq!(pack.mappings, mappings);
    assert_eq!(pack.replacements.len(), 1);
}

#[test]
fn firmware_and_application_declarations_coexist() {
    let files = BTreeMap::from([
        ("app/test.bin".into(), vec![1]),
        ("quickapp-icons/a.bin".into(), vec![2]),
    ]);
    let icons = [QuickappIcon {
        package: "a.b".into(),
        destination: "quickapp-icons/a.bin".into(),
    }];
    let pack = crpack::parse_crpack(
        &crpack::build_crpack_with_icons(&options(&files), &[], &icons).unwrap(),
    )
    .unwrap();
    assert_eq!(
        pack.mappings,
        vec![Mapping {
            source: "/resource/app/".into(),
            destination: "app/".into()
        }]
    );
    assert_eq!(pack.quickapp_icons, icons);
}

#[test]
fn malformed_icon_declarations_are_rejected() {
    for invalid in [
        json!(null),
        json!({}),
        json!([{}]),
        json!([{"package":"a\t", "destination":"icon.bin"}]),
        json!([{"package":"a\u{007f}", "destination":"icon.bin"}]),
        json!([{"package":"a.b", "destination":"../icon.bin"}]),
        json!([{"package":"a.b", "destination":"icon.BIN"}]),
        json!([{"package":"a.b", "destination":"icon.png"}]),
        json!([{"package":"a.b", "destination":"icons/"}]),
        json!([{"package":"a.b", "destination":"missing.bin"}]),
        json!([{"package":"a.b", "destination":"icon.bin"}, {"package":"a.b", "destination":"icon.bin"}]),
    ] {
        let mut value = manifest();
        value["quickappIcons"] = invalid.clone();
        assert!(
            crpack::parse_crpack(&archive(value, &[("icon.bin", b"x")])).is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn normalized_quickapp_mapping_imports_but_cannot_duplicate_declaration() {
    let mut value = manifest();
    value["mappings"] =
        json!([{"source":"@quickapp-icon/ng.lst.corona", "destination":"icon.bin"}]);
    let pack = crpack::parse_crpack(&archive(value.clone(), &[("icon.bin", b"x")])).unwrap();
    assert_eq!(pack.mappings[0].source, "@quickapp-icon/ng.lst.corona");
    value["quickappIcons"] = json!([{"package":"ng.lst.corona", "destination":"icon.bin"}]);
    assert!(
        crpack::parse_crpack(&archive(value, &[("icon.bin", b"x")]))
            .unwrap_err()
            .contains("duplicate")
    );
}

#[test]
fn quickapp_rules_share_count_and_tsv_budgets_with_mappings() {
    let files = BTreeMap::from([("icon.bin".into(), vec![1])]);
    let icons: Vec<_> = (0..256)
        .map(|i| QuickappIcon {
            package: format!("a.p{i}"),
            destination: "icon.bin".into(),
        })
        .collect();
    assert!(crpack::build_crpack_with_icons(&options(&files), &[], &icons).is_ok());
    let mappings = [Mapping {
        source: app_icons::CANOPUS_SOURCE.into(),
        destination: "icon.bin".into(),
    }];
    assert!(
        crpack::build_crpack_with_icons(&options(&files), &mappings, &icons)
            .unwrap_err()
            .contains("256")
    );
    let large_icons: Vec<_> = (0..256)
        .map(|i| QuickappIcon {
            package: format!("{}.p{i}", "a".repeat(118)),
            destination: "icon.bin".into(),
        })
        .collect();
    assert!(
        crpack::build_crpack_with_icons(&options(&files), &[], &large_icons)
            .unwrap_err()
            .contains("32 KiB")
    );
}

#[test]
fn raw_icons_require_actual_lvgl_bin_and_pngs_allow_optional_quickapp_templates() {
    let (_, png) = corona_core::lvgl::decode_image_png(&app_icons::canopus_template()).unwrap();
    assert!(app_icons::encode(&png, None, false, true, false).is_err());
    let quickapp = app_icons::encode(&png, None, false, false, false).unwrap();
    assert_eq!(quickapp, app_icons::canopus_template());
    let encoded = app_icons::encode(&png, None, true, false, false).unwrap();
    assert_eq!(encoded, app_icons::canopus_template());
    assert!(app_icons::encode(&png, Some(&png), false, false, false).is_err());
}

#[test]
fn opaque_packages_round_trip_without_becoming_paths_or_directories() {
    let budget = 255 - app_icons::QUICKAPP_SOURCE_PREFIX.len();
    let packages = [
        "".into(),
        "single".into(),
        " 快应用 ".into(),
        "/../a//".into(),
        r"C:\a:b".into(),
        "a..b".into(),
        "a\u{0085}\u{009f}".into(),
        "é".repeat(budget / 2) + &"a".repeat(budget % 2),
    ];
    let files = packages
        .iter()
        .map(|package| (app_icons::destination(package), vec![1]))
        .collect();
    let icons: Vec<_> = packages
        .iter()
        .map(|package| QuickappIcon {
            package: package.clone(),
            destination: app_icons::destination(package),
        })
        .collect();
    let mut opts = options(&files);
    opts.theme_id = "longtheme123";
    let bytes = crpack::build_crpack_with_mappings(&opts, &[], &icons).unwrap();
    let parsed = crpack::parse_crpack(&bytes).unwrap();
    assert_eq!(parsed.quickapp_icons, icons);
    assert_eq!(parsed.replacements, files);
    let semantic_mappings: Vec<_> = icons
        .iter()
        .map(|icon| Mapping {
            source: app_icons::source(&icon.package),
            destination: icon.destination.clone(),
        })
        .collect();
    let bytes = crpack::build_crpack_with_mappings(&opts, &semantic_mappings, &[]).unwrap();
    assert_eq!(
        crpack::parse_crpack(&bytes).unwrap().mappings,
        semantic_mappings
    );

    // Shared file destinations remain legal. Ending '/' in an opaque source
    // must not invoke firmware directory-prefix or slash-matching semantics.
    let shared_files = BTreeMap::from([("shared.bin".into(), vec![1])]);
    let mappings = [Mapping {
        source: app_icons::source("/../a//"),
        destination: "shared.bin".into(),
    }];
    let shared_icons = [QuickappIcon {
        package: "".into(),
        destination: "shared.bin".into(),
    }];
    let bytes =
        crpack::build_crpack_with_mappings(&options(&shared_files), &mappings, &shared_icons)
            .unwrap();
    let parsed = crpack::parse_crpack(&bytes).unwrap();
    assert_eq!(parsed.mappings, mappings);
    assert_eq!(parsed.quickapp_icons, shared_icons);
    let directory = [Mapping {
        source: app_icons::source("/../a//"),
        destination: "icons/".into(),
    }];
    assert!(crpack::build_crpack_with_mappings(&options(&shared_files), &directory, &[]).is_err());
}

#[test]
fn both_quickapp_declarations_and_semantic_mapping_sources_enforce_wire_safety() {
    let budget = 255 - app_icons::QUICKAPP_SOURCE_PREFIX.len();
    for package in [
        "a\t".into(),
        "a\n".into(),
        "a\r".into(),
        "a\0".into(),
        "a\u{007f}".into(),
        "a".repeat(budget + 1),
        "é".repeat(budget / 2 + 1),
    ] {
        for normalized in [false, true] {
            let mut value = manifest();
            if normalized {
                value["mappings"] = json!([{
                    "source": app_icons::source(&package), "destination":"icon.bin"
                }]);
            } else {
                value["quickappIcons"] = json!([{
                    "package":package, "destination":"icon.bin"
                }]);
            }
            assert!(crpack::parse_crpack(&archive(value, &[("icon.bin", b"x")])).is_err());
        }
    }
    for package in [
        "a".repeat(budget),
        "é".repeat(budget / 2) + &"a".repeat(budget % 2),
    ] {
        let files = BTreeMap::from([("icon.bin".into(), vec![1])]);
        let mappings = [Mapping {
            source: app_icons::source(&package),
            destination: "icon.bin".into(),
        }];
        assert_eq!(mappings[0].source.len(), 255);
        let bytes = crpack::build_crpack_with_mappings(&options(&files), &mappings, &[]).unwrap();
        assert_eq!(crpack::parse_crpack(&bytes).unwrap().mappings, mappings);
    }
}

#[test]
fn template_free_quickapp_encoding_ignores_resize_and_quantize_options() {
    use corona_core::lvgl::{self, ResizeFilter};
    use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
    let image = RgbaImage::from_fn(257, 1, |x, _| Rgba([x as u8, (x / 256) as u8, 91, x as u8]));
    let mut png = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(image.clone())
        .write_to(&mut png, ImageFormat::Png)
        .unwrap();
    let expected = lvgl::encode_png_argb8888(png.get_ref()).unwrap();
    for allow_quantize in [false, true] {
        let encoded =
            app_icons::encode_detailed(png.get_ref(), None, false, false, allow_quantize).unwrap();
        assert_eq!(encoded, expected);
        for filter in [
            ResizeFilter::Nearest,
            ResizeFilter::Lanczos3,
            ResizeFilter::Triangle,
        ] {
            let bytes = app_icons::encode_with_filter(
                png.get_ref(),
                None,
                false,
                false,
                allow_quantize,
                filter,
            )
            .unwrap();
            assert_eq!(bytes, expected.bytes);
            assert_eq!(lvgl::decode_to_rgba(&bytes).unwrap().1, image);
        }
    }
    // An explicit template still enforces its existing aspect-ratio contract.
    assert!(
        app_icons::encode(
            png.get_ref(),
            Some(&app_icons::canopus_template()),
            false,
            false,
            false
        )
        .unwrap_err()
        .contains("aspect ratio")
    );
    assert!(
        app_icons::encode(png.get_ref(), None, true, false, false)
            .unwrap_err()
            .contains("aspect ratio")
    );
}
