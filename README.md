# Conora CRPack Builder

AstroBox NG API Level 4 Rust/WASM plugin for creating Canopus Resource Pack (`.crpack`) v1 files from watch firmware.

## What it does

- Opens a firmware `.bin` OTA ZIP/JAR containing `vela_resource.bin`, or a raw ROMFS image.
- Browses the ROMFS resource tree without changing the source firmware.
- Previews files recognized as LVGL images (v9 I8/A8/ARGB8888/I4/A4 uncompressed or RLE, v8 RGB565/I8), PNG, and JPEG.
- Extracts the selected resource in its current state; recognized image resources can also be converted and extracted as PNG.
- Replaces a selected resource with a PNG converted against its original BIN template (with bidirectional conversion across supported formats), or with arbitrary file bytes.
- Exports only replaced files plus a root `canora.json` manifest as a `.crpack` ZIP.

PNG conversion keeps the original dimensions and stride. PNGs with more than 256 RGBA colors require opting into lossy quantization. Unsupported BIN formats remain extractable and replaceable as ordinary files, but are not previewed or converted. When a resource has a pending replacement, extraction uses that current replacement; otherwise it extracts the firmware original.

CRPack export follows `../Canopus-Module-Resource-Hook/docs/interconnect_proto.md`. It validates safe paths, `themeId`, mapping count and generated `mappings.tsv` size, manifest size, file count and the 64 MiB uncompressed package limit. It does not install a pack or send it to the watch.

The initial target is Xiaomi Band 11 firmware `4.100.155`; `targets` is inferred from the selected firmware filename when possible and remains editable. The importer streams the ROMFS tree into metadata and reads original file bytes lazily; it does not keep a full expanded ROMFS in memory. AstroBox's file-picker still passes the complete compressed archive to the WASM plugin. For deflated ROMFS entries, loading a selected file may need to inflate the stream up to that file's offset, trading latency for lower memory use. Actual imports remain subject to available host memory.

## Build

Requires an AstroBox NG host with API Level 4 support.

API Level 4 uses async WIT exports, while the Rust guest standard library must target WASI P2:

```bash
rustup target add wasm32-wasip2
python3 scripts/build_dist.py --release --package
```

The `.abp` output is the installable AstroBox plugin. The `.crpack` is generated later from the plugin UI.

## Tests

```bash
cargo test --target aarch64-apple-darwin
CONORA_TEST_FIRMWARE="$HOME/develop/temp/miwear.watch.q66tc_v4.100.155_full_f1c824fe.bin" \
  cargo test --target aarch64-apple-darwin parses_real_firmware_when_requested -- --nocapture
```
