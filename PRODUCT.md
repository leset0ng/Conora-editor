# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

## Stack

delegated: AstroBox NG API Level 4 Rust plugin, `wasm32-wasip2` standard library, AstroBox UI v3 and host dialogs.

## Users

AstroBox users who create or customize Canopus/Conora resource packs for Xiaomi Band 11 firmware.

## Product Purpose

Import a watch firmware image, inspect its resources, selectively replace files, and export a valid `.crpack`. Success means a user can prepare a shareable pack without manually unpacking ROMFS or writing its manifest.

## Positioning

The pack is generated from the firmware's actual resource tree, preserving resource-relative paths and providing previews/conversion for supported LVGL images.

## Operating Context

The user selects a local OTA firmware archive, browses its resource tree, selects an item to preview or replace, and exports the pending replacements as a CRPack v1 archive.

## Capabilities and Constraints

- Initial validation target is the supplied Xiaomi Band 11 firmware `4.100.155`.
- The input OTA is a ZIP/JAR containing `vela_resource.bin`, which contains a ROMFS resource tree.
- Automatic image preview and bidirectional PNG conversion support LVGL v9 (I8, A8, ARGB8888, I4, A4; uncompressed and RLE-compressed), LVGL v8 (RGB565, I8), PNG, and JPEG; PNG dimensions must match the source template. Lossy quantization must be explicit.
- Non-image replacements are copied as file bytes; their device-specific format is not inferred or certified.
- Export follows `../Canopus-Module-Resource-Hook/docs/interconnect_proto.md` CRPack v1: root `canora.json`, only replacement assets, safe paths, no `mappings.tsv`, and documented file/size/rule limits.
- `.crpack` export is separate from AstroBox `.abp` plugin packaging and does not install or transmit resources to the watch.
- The AstroBox file-picker returns the complete selected archive as bytes. The importer streams ROMFS metadata and lazily loads selected resources rather than retaining an expanded ROMFS; compressed entries may incur additional inflate latency when loading files. Validate guest memory and responsiveness on the actual host.

## Evidence on Hand

- User-provided OTA: `~/develop/temp/miwear.watch.q66tc_v4.100.155_full_f1c824fe.bin`.
- Resource format reference and tested helper: `../Canopus-Module-Resource-Hook/tools/resource_image.py` and `tools/RESOURCE_IMAGE.md`.
- CRPack v1 contract: `../Canopus-Module-Resource-Hook/docs/interconnect_proto.md`.

## Product Principles

- Keep firmware analysis local to the plugin.
- Preserve source paths; export only intentional replacements.
- Refuse malformed archives, unsafe paths, unsupported image layouts, and CRPack limit violations rather than silently repairing them.
- Distinguish validated format support from generic binary replacement.
