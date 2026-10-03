//! Arbitrary runtime files and mappings independent of the pinned firmware tree.

use sha2::{Digest, Sha256};

use crate::{app_icons, lvgl};

/// Runtime files share the application icon input/conversion options.
pub type RuntimeAsset = app_icons::AppIconAsset;

/// Keep exact source identities out of archive paths using a deterministic hash.
pub fn destination(source: &str) -> String {
    let hash = format!("{:x}", Sha256::digest(source.as_bytes()));
    format!("custom/{}.bin", &hash[..16])
}

/// Raw files are opaque bytes; PNGs use the existing QuickApp image codec.
/// Without a template, PNGs retain native dimensions and alpha in uncompressed
/// LVGL v9 ARGB8888. Explicit templates use the existing template conversion.
pub fn encode(
    input: &[u8],
    template: Option<&[u8]>,
    raw: bool,
    allow_quantize: bool,
    filter: lvgl::ResizeFilter,
) -> Result<lvgl::EncodedImage, String> {
    if raw {
        return Ok(lvgl::EncodedImage {
            bytes: input.to_vec(),
            lossy_quantization: false,
        });
    }
    app_icons::encode_detailed_with_filter(input, template, false, false, allow_quantize, filter)
}
