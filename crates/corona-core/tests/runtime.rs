use std::io::Cursor;

use corona_core::{ResizeFilter, app_icons, crpack, lvgl, runtime};
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};

fn png(image: RgbaImage) -> Vec<u8> {
    let mut output = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(image)
        .write_to(&mut output, ImageFormat::Png)
        .unwrap();
    output.into_inner()
}

#[test]
fn runtime_png_defaults_preserve_native_dimensions_and_alpha() {
    let pixels = RgbaImage::from_fn(7, 3, |x, y| Rgba([x as u8, y as u8, 173, (x * 37) as u8]));
    let input = png(pixels.clone());
    let encoded = runtime::encode(&input, None, false, false, ResizeFilter::default()).unwrap();
    assert!(!encoded.lossy_quantization);
    let (info, decoded) = lvgl::decode_to_rgba(&encoded.bytes).unwrap();
    assert_eq!((info.width, info.height, info.stride), (7, 3, 28));
    assert_eq!(info.format.display_name(), "LVGL v9 ARGB8888");
    assert_eq!(&encoded.bytes[..4], &[0x19, 0x10, 0, 0]);
    assert_eq!(decoded, pixels);
    assert_eq!(encoded.bytes.len(), 12 + 7 * 3 * 4);
    assert_eq!(
        encoded.bytes,
        app_icons::encode(&input, None, false, false, false).unwrap()
    );
}

#[test]
fn runtime_png_explicit_templates_reuse_the_app_icon_codec() {
    let input = png(RgbaImage::from_pixel(3, 3, Rgba([12, 34, 56, 127])));
    let template = app_icons::canopus_template();
    let encoded = runtime::encode(
        &input,
        Some(&template),
        false,
        false,
        ResizeFilter::Lanczos3,
    )
    .unwrap();
    let expected = app_icons::encode_detailed_with_filter(
        &input,
        Some(&template),
        false,
        false,
        false,
        ResizeFilter::Lanczos3,
    )
    .unwrap();
    assert_eq!(encoded.bytes, expected.bytes);
    assert_eq!(lvgl::inspect_image(&encoded.bytes).unwrap().width, 117);
    assert!(
        runtime::encode(
            &input,
            Some(b"not a template"),
            false,
            false,
            ResizeFilter::default()
        )
        .is_err()
    );
    assert!(runtime::encode(b"not a PNG", None, false, false, ResizeFilter::default()).is_err());
}

#[test]
fn raw_runtime_bytes_are_not_inspected_even_with_an_invalid_template() {
    for bytes in [
        b"".as_slice(),
        b"arbitrary\0\xff data",
        b"\x19\x10\0\0 malformed LVGL",
        b"\x89PNG\r\n\x1a\n",
    ] {
        let encoded = runtime::encode(
            bytes,
            Some(b"not a template"),
            true,
            true,
            ResizeFilter::default(),
        )
        .unwrap();
        assert_eq!(encoded.bytes, bytes);
        assert!(!encoded.lossy_quantization);
    }
}

#[test]
fn destinations_and_public_validators_follow_the_existing_protocol() {
    assert_eq!(runtime::destination(""), "custom/e3b0c44298fc1c14.bin");
    for source in ["/data/app/icon.bin", "/data/tree/", "/", "/你好/icon"] {
        crpack::validate_absolute_source(source).unwrap();
        let path = runtime::destination(source);
        crpack::validate_relative_path(&path).unwrap();
        crpack::validate_relative_destination(&path).unwrap();
        assert_eq!(path.len(), "custom/".len() + 16 + 4);
        assert_eq!(path, runtime::destination(source));
    }
    assert_ne!(runtime::destination("/a"), runtime::destination("/a/"));
    crpack::validate_relative_destination("custom/tree/").unwrap();
    for source in ["relative", "/a/../b", "/a//b", "/a\tb"] {
        assert!(crpack::validate_absolute_source(source).is_err());
    }
    for path in [
        "/absolute",
        "../a",
        "custom//",
        "corona.json",
        "x/mappings.tsv",
    ] {
        assert!(crpack::validate_relative_destination(path).is_err());
    }
}
