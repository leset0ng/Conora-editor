# Product

<!-- impeccable:product-schema 1 -->

## Platform

web and native CLI

## Stack

Shared Rust `corona-core`, a native `corona` CLI, and an AstroBox NG API Level 4 Rust plugin using `wasm32-wasip2`, AstroBox UI v3 and host dialogs.

## Users

AstroBox users and coding agents who create or customize Canopus/Corona resource packs for watch firmware, including Xiaomi Band 11.

## Product Purpose

Import a watch firmware image, inspect its resources, selectively replace files, and export a valid `.crpack`. Success means a user can prepare a shareable pack without manually unpacking ROMFS or writing its manifest.

## Positioning

Firmware replacements are generated from the actual resource tree, preserving resource-relative paths and providing previews/conversion for supported LVGL images. Explicit custom rules also address runtime paths absent from firmware; their source existence and device compatibility are not inferred from the firmware inventory.

## Operating Context

The user selects a local OTA firmware archive, browses its resource tree, selects an item to preview or replace, and exports the pending replacements as a CRPack v1 archive.

## Capabilities and Constraints

- Initial real-device validation target is the supplied Xiaomi Band 11 firmware `4.100.155`; synthetic tests cover multiple firmware paths, sizes and image formats.
- CLI themes share logical icon roles and source assets across targets. Each target stores bindings and the complete firmware SHA-256, never per-resource hashes; optional target asset overrides support differing layouts. Builds produce separate CRPack v1 outputs, not a multi-firmware container.
- CLI default imports preserve original CRPack bytes/manifest as evidence, all archive file bytes, ordered mappings and original application declarations/destinations in a target-scoped runtime lane. Aliases, directory/exact exceptions, missing firmware sources and unmapped files remain intact. `--normalize-firmware` opts into editable shared PNGs with original-target raw overrides and strict firmware binding resolution. Advisory cross-firmware planning is read-only; explicit exclusions retain firmware-specific artwork without weakening missing-binding validation. Actual encoded previews support bounded contact sheets and conversion verification, not on-device certification.
- Native batch resource operations preflight byte budgets and traverse compressed streams once; progress goes to stderr while JSON stdout stays parseable. Firmware-version target IDs may contain safe dots.
- CLI operations are non-interactive, support JSON diagnostics, refuse implicit output overwrite and protect source firmware/configs/assets. All selected targets must validate before outputs are published; each output is atomically committed, but multi-file commits are not a filesystem transaction.
- The input OTA is a ZIP/JAR containing `vela_resource.bin`, which contains a ROMFS resource tree.
- Automatic image preview and bidirectional PNG conversion support LVGL v9 (I8, A8, ARGB8888, I4, A4; uncompressed and RLE-compressed), LVGL v8 (RGB565, I8), PNG, and JPEG; PNG inputs with the same aspect ratio are automatically scaled to the source template dimensions using nearest-neighbor sampling; mismatched aspect ratios are rejected without padding, cropping, or stretching. Same-size inputs are not resampled. PNG inputs are limited to 64 MiB and 16 * 1024 * 1024 pixels. Lossy quantization must be explicit.
- Non-image replacements are copied as file bytes; their device-specific format is not inferred or certified.
- Export follows `../Canopus-Module-Resource-Hook/docs/interconnect_proto.md` CRPack v1: root `corona.json`, only replacement assets, safe paths, no `mappings.tsv`, and documented size/rule limits. Import accepts the legacy `canora.json` with identical validation; both names in one pack are rejected. Re-export and extracted file lists use `corona.json`. CRPack import/export has no file-count cap; transfers above 128 files need an updated Manager and remain subject to the protocol's 65,536-file index range.
- Third-party application icons are authored outside the ROMFS tree. Canopus uses the exact `/data/canopus/manager_icon.bin` mapping and a known LVGL v9 117x117 layout preset; QuickApps use optional package-based `quickappIcons` declarations with actual BIN destinations. Template-free QuickApp PNG conversion retains source dimensions and alpha losslessly as uncompressed LVGL v9 ARGB8888 (native BGRA pixels); an optional supported original BIN template retains existing proportional-resize/layout behavior. Canopus PNGs retain the known preset unless an explicit template is supplied; raw icons must be supported LVGL BIN images. CLI assets/templates are project-owned and can have target overrides; the plugin can export application-only packs without firmware. Import/re-export preserves application declarations and normalized `@quickapp-icon/<package>` keys. QuickApp identifiers are opaque exact strings, not package grammar or paths: no trimming; only a 255-byte UTF-8 prefixed-source budget and exclusion of TSV bytes 0–31/127. New archive filenames use SHA-256 of the package UTF-8 bytes, independent of the identifier syntax.
- Custom runtime rules are independent of firmware inventory membership. The editor supports exact-file authoring without firmware and keeps custom/imported assets when firmware changes; PNGs default to native-dimension lossless LVGL v9 ARGB8888, or use an imported/explicit supported BIN template. Generic raw data is copied without LVGL validation. CLI `mapping add/list/remove` manages target-scoped `runtimeFiles`/ordered `runtimeMappings`; `runtimeQuickappIcons` preserves imported declarations. CLI targets still require pinned firmware. Shared destinations reference one packaged file; unreferenced files never acquire inferred mappings. Encoding/preview verifies structure and pixels, not device compatibility or runtime access.
- Firmware mappings and QuickApp declarations share the 256-rule/32 KiB TSV budgets. Package syntax, duplicate normalized sources, actual `.bin` files, and safe installed paths are checked. QuickApp application requires an updated Manager/module and approved exact device target (currently Band 11 `.139/.155`); encoding does not verify application installation or Launcher refresh.
- `.crpack` export is separate from AstroBox `.abp` plugin packaging and does not install or transmit resources to the watch.
- The AstroBox file-picker returns the complete selected archive as bytes. The importer streams ROMFS metadata and lazily loads selected resources rather than retaining an expanded ROMFS; compressed entries may incur additional inflate latency when loading files. Validate guest memory and responsiveness on the actual host.

## Evidence on Hand

- User-provided OTA: `~/develop/temp/miwear.watch.q66tc_v4.100.155_full_f1c824fe.bin`.
- Resource format reference and tested helper: `../Canopus-Module-Resource-Hook/tools/resource_image.py` and `tools/RESOURCE_IMAGE.md`.
- CRPack v1 contract: `../Canopus-Module-Resource-Hook/docs/interconnect_proto.md`.

## Product Principles

- Keep firmware analysis local to the editor or CLI; do not require a model service.
- Separate reusable theme design from firmware-specific resource bindings; never infer compatibility solely from filenames or advisory targets.
- Preserve source paths; export only intentional replacements.
- Refuse malformed archives, unsafe paths, unsupported requested image conversions, and CRPack limit violations rather than silently repairing them; arbitrary raw runtime bytes remain opaque.
- Separate device source paths from archive destinations. Preserve authoritative imported rules, shared assets and unmapped files rather than normalizing or inferring their intent.
- Distinguish validated format support from generic binary replacement.
