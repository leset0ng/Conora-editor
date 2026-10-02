//! Application icon authoring independent of the firmware ROMFS tree.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{lvgl, project::AssetMode};

pub const CANOPUS_SOURCE: &str = "/data/canopus/manager_icon.bin";
pub const CANOPUS_DESTINATION: &str = "canopus/manager_icon.bin";
pub const QUICKAPP_SOURCE_PREFIX: &str = "@quickapp-icon/";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppIconAsset {
    pub input: PathBuf,
    #[serde(default)]
    pub mode: AssetMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<PathBuf>,
    #[serde(default)]
    pub allow_quantize: bool,
    #[serde(default, skip_serializing_if = "crate::lvgl::ResizeFilter::is_default")]
    pub filter: crate::lvgl::ResizeFilter,
}

/// Packages are opaque, exact identifiers, not filesystem paths.
/// Only enforce the native mappings.tsv wire limits (including the source prefix).
pub fn validate_package(package: &str) -> Result<(), String> {
    if package.len() > 255 - QUICKAPP_SOURCE_PREFIX.len() {
        return Err("QuickApp source '@quickapp-icon/' + package exceeds 255 UTF-8 bytes".into());
    }
    if package.bytes().any(|byte| byte < 32 || byte == 127) {
        return Err(
            "QuickApp package cannot contain bytes 0-31 or 127 (mappings.tsv safety)".into(),
        );
    }
    Ok(())
}

/// Keep exact identifiers out of paths using a short, deterministic 64-bit hash.
pub fn destination(package: &str) -> String {
    let hash = format!("{:x}", Sha256::digest(package.as_bytes()));
    format!("quickapp-icons/{}.bin", &hash[..16])
}

pub fn source(package: &str) -> String {
    format!("{QUICKAPP_SOURCE_PREFIX}{package}")
}

/// Layout-only template for the known Canopus LVGL v9 117x117 ARGB8888 icon.
/// Blank pixels avoid bundling or depending on the installer's original artwork.
/// Other devices must supply their actual original BIN template.
pub fn canopus_template() -> Vec<u8> {
    let mut bytes = vec![0; 12 + 117 * 117 * 4];
    bytes[..12].copy_from_slice(&[0x19, 0x10, 0, 0, 117, 0, 117, 0, 0xd4, 1, 0, 0]);
    bytes
}

pub fn inspect_bin(bytes: &[u8]) -> Result<lvgl::ImageInfo, String> {
    let info =
        lvgl::inspect_image(bytes).ok_or("application icon must be a supported LVGL BIN image")?;
    if matches!(
        info.format,
        lvgl::ImageFormatKind::Png | lvgl::ImageFormatKind::Jpeg
    ) {
        return Err("application icon must be LVGL BIN, not PNG or JPEG".into());
    }
    // Header inspection alone does not detect every malformed compressed payload.
    lvgl::decode_to_rgba(bytes)?;
    Ok(info)
}

pub fn encode_detailed_with_filter(
    input: &[u8],
    template: Option<&[u8]>,
    canopus: bool,
    raw: bool,
    allow_quantize: bool,
    filter: crate::lvgl::ResizeFilter,
) -> Result<lvgl::EncodedImage, String> {
    if raw {
        inspect_bin(input)?;
        return Ok(lvgl::EncodedImage {
            bytes: input.to_vec(),
            lossy_quantization: false,
        });
    }
    let preset;
    let template = match template {
        Some(template) => template,
        None if canopus => {
            preset = canopus_template();
            &preset
        }
        None => return lvgl::encode_png_argb8888(input),
    };
    inspect_bin(template)?;
    lvgl::encode_png_to_template_with_filter(input, template, allow_quantize, filter)
}

pub fn encode_detailed(
    input: &[u8],
    template: Option<&[u8]>,
    canopus: bool,
    raw: bool,
    allow_quantize: bool,
) -> Result<lvgl::EncodedImage, String> {
    encode_detailed_with_filter(
        input,
        template,
        canopus,
        raw,
        allow_quantize,
        crate::lvgl::ResizeFilter::default(),
    )
}

pub fn encode(
    input: &[u8],
    template: Option<&[u8]>,
    canopus: bool,
    raw: bool,
    allow_quantize: bool,
) -> Result<Vec<u8>, String> {
    encode_detailed(input, template, canopus, raw, allow_quantize).map(|image| image.bytes)
}

pub fn encode_with_filter(
    input: &[u8],
    template: Option<&[u8]>,
    canopus: bool,
    raw: bool,
    allow_quantize: bool,
    filter: crate::lvgl::ResizeFilter,
) -> Result<Vec<u8>, String> {
    encode_detailed_with_filter(input, template, canopus, raw, allow_quantize, filter)
        .map(|image| image.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_validation_matches_wire_contract() {
        for valid in [
            "",
            "canopus",
            "ng.lst.corona",
            ".",
            "..",
            "a..b",
            "/",
            "../a/",
            "a:b",
            r"a\b",
            " 快应用 ",
            "a.é",
            "a\u{0085}b",
            "a\u{009f}b",
        ] {
            assert!(validate_package(valid).is_ok(), "{valid:?}");
        }
        for byte in (0..32).chain([127]) {
            assert!(validate_package(&format!("a{}b", char::from(byte))).is_err());
        }
        let budget = 255 - QUICKAPP_SOURCE_PREFIX.len();
        assert!(validate_package(&"a".repeat(budget)).is_ok());
        assert!(validate_package(&"a".repeat(budget + 1)).is_err());
        let unicode = format!("{}{}", "é".repeat(budget / 2), "a".repeat(budget % 2));
        assert_eq!(source(&unicode).len(), 255);
        assert!(validate_package(&unicode).is_ok());
        assert!(validate_package(&format!("{unicode}a")).is_err());
    }

    #[test]
    fn destinations_are_independent_safe_fixed_size_names() {
        assert_eq!(destination(""), "quickapp-icons/e3b0c44298fc1c14.bin");
        for package in ["ng.lst.corona", "../", "/", r"C:\foo", " a ", "快应用"] {
            let path = destination(package);
            crate::crpack::validate_relative_path(&path).unwrap();
            assert_eq!(path.len(), "quickapp-icons/".len() + 16 + 4);
            assert_eq!(path.matches('/').count(), 1);
            assert_eq!(path, destination(package));
        }
        assert_ne!(destination(" a "), destination("a"));
        assert_ne!(destination("é"), destination("e\u{0301}"));
    }

    #[test]
    fn canopus_template_is_valid_and_needs_no_sibling_checkout() {
        let info = inspect_bin(&canopus_template()).unwrap();
        assert_eq!((info.width, info.height, info.stride), (117, 117, 468));
        assert!(encode(b"not a bin", None, true, true, false).is_err());
        assert!(encode(b"not a png", None, false, false, false).is_err());
    }
}
