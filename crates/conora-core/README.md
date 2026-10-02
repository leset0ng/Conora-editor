# Conora Core

Shared Rust implementation for the AstroBox Conora editor and the native `conora` CLI.

- Inspect OTA ZIP/JAR archives containing `vela_resource.bin`, or raw ROMFS images.
- Inspect and convert supported LVGL, PNG and JPEG images using firmware originals as templates.
- Build and strictly parse CRPack v1 archives.
- Build declarative icon themes against multiple independently fingerprinted firmwares.
- Author Canopus runtime icons and package-based QuickApp icons without ROMFS bindings; validate optional `quickappIcons` and canonical `@quickapp-icon/<package>` mappings.

```rust,no_run
use std::path::Path;
use conora_core::project::{load_theme, prepare_target};

let theme = load_theme(Path::new("my-icons"))?;
let prepared = prepare_target(&theme, "band11-A");
if let Some(pack) = prepared.pack {
    std::fs::write("dark-band11-A.crpack", pack)?;
} else {
    for diagnostic in prepared.report.errors {
        eprintln!("{}: {}", diagnostic.code, diagnostic.message);
    }
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

The theme schema separates logical icon roles from target-specific firmware paths. Assets are resolved relative to `theme.json`; firmware paths are resolved relative to the target config in `targets/`. A target records only the SHA-256 of the entire input firmware, not hashes of individual resources or assets. A different firmware must be explicitly pinned and its bindings reviewed.

Targets can explicitly exclude a shared role with a nonblank reason in `excluded`; exclusions are reported and cannot coexist with bindings/overrides. All other roles remain required. Dotted firmware-version target IDs such as `p67-3.101.043` are accepted without permitting hidden paths or traversal.

Native project firmware reads are limited to 512 MiB, config files to 1 MiB, and assets/original conversion templates to 64 MiB per file and in aggregate per prepared target. Special-file inputs are rejected; Unix reads avoid blocking on FIFOs.

`FirmwareIndex::visit_file_bytes` preflights all requested paths and byte budgets, deduplicates and visits them in physical order through a single compressed stream, releasing each buffer after its callback. Project preparation uses this batch path while individual browser reads remain lazy. `prepare_target_with_progress` exposes callback progress without writing to stderr from the shared core.

Raster decoding is capped at 16 megapixels before allocation. RLE expansion must cover the indexed dimensions, fit the 64 MiB payload budget and complete the declared expansion; bounded vendor padding is allowed.

PNG inputs are proportionally resized to each original template with nearest-neighbor sampling. Different aspect ratios are rejected; use a target asset override rather than implicit cropping. Palette quantization needs explicit opt-in. Native color precision reduction (RGB565, alpha-only formats) and JPEG encoding can be intrinsically lossy and are reported separately. Raw replacements are copied without certifying device compatibility.

CRPack structure validation is not a device compatibility guarantee. `device`/manifest `targets` values are advisory metadata. The core does not install or transmit packs. `build_crpack_with_icons` combines automatically inferred firmware mappings with explicit mappings and `QuickappIcon { package, destination }` declarations. Explicitly referenced assets do not acquire inferred `/resource/` mappings. `build_crpack_with_mappings` instead serializes authoritative rules without inference, preserving unreferenced imported files as unmapped. Both declaration forms share the 256-rule and 32 KiB TSV budgets; QuickApp packages are opaque exact strings, including empty strings, Unicode, whitespace, path separators and dot patterns. Validation only enforces the native TSV wire contract: `@quickapp-icon/` plus the package is at most 255 UTF-8 bytes, and bytes 0–31 and 127 are forbidden (not all Unicode control characters). No trimming or normalization occurs. Destinations must name actual lowercase `.bin` files; semantic QuickApp sources are never directory mappings, even when ending in `/`. `app_icons::destination` always uses `quickapp-icons/<16-hex-hash>.bin` (the first 16 lowercase hex digits of SHA-256), keeping arbitrary identifiers out of filesystem paths and leaving ample space for the device theme root and theme ID. `UnpackedCrpack` retains declarations, ordered mappings, original manifest bytes, and replacement files.

Project `canopusIcon` and `quickappIcons` assets use `AppIconAsset` with `input`, `mode`, optional `template`, and `allowQuantize`; target fields override declared application assets without using ordinary `bindings`. The built-in Canopus template is a synthesized blank LVGL v9 ARGB8888 117x117 layout, not an external repository dependency or original artwork. QuickApp PNG assets without a template are encoded at their own dimensions as uncompressed LVGL v9 ARGB8888 (native BGRA payload), retaining every RGBA channel without resampling or quantization. The width/height/stride must fit the 16-bit header fields (maximum width 16383, height 65535); the 16 * 1024 * 1024-pixel and 64 MiB PNG-input limits still apply. An optional original LVGL BIN template retains existing proportional-resize/layout behavior. Raw application assets must decode as supported LVGL BIN images. Asset and template byte budgets remain aggregate across ordinary and application resources.

The AstroBox UI preserves application declarations and imported mapping rules on re-export, but it does not preserve arbitrary unknown manifest fields verbatim. QuickApp application requires an updated receiver/module and approved exact device target; producing a pack cannot certify application installation or Launcher cache refresh.
