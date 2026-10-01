# Conora CRPack Builder

An AstroBox NG API Level 4 Rust/WASM editor and native `conora` CLI for creating Canopus Resource Pack (`.crpack`) v1 files from watch firmware. Both use the same `conora-core` library.

## What it does

- Opens a firmware `.bin` OTA ZIP/JAR containing `vela_resource.bin`, or a raw ROMFS image.
- Browses the ROMFS resource tree without changing the source firmware.
- Previews files recognized as LVGL images (v9 I8/A8/ARGB8888/I4/A4 uncompressed or RLE, v8 RGB565/I8), PNG, and JPEG.
- Extracts the selected resource in its current state; recognized image resources can also be converted and extracted as PNG.
- Replaces a selected resource with a PNG converted against its original BIN template (with bidirectional conversion across supported formats), or with arbitrary file bytes.
- Exports only replaced files plus a root `canora.json` manifest as a `.crpack` ZIP.

PNG conversion keeps the original dimensions and stride. Inputs with the same aspect ratio are automatically scaled up or down to the original dimensions using nearest-neighbor sampling, preserving palette colors and transparent pixels; different aspect ratios are rejected without padding, cropping, or stretching. Same-size inputs are not resampled. PNG inputs are limited to 64 MiB and 16 * 1024 * 1024 pixels. PNGs with more than 256 RGBA colors require opting into lossy quantization. Unsupported BIN formats remain extractable and replaceable as ordinary files, but are not previewed or converted. When a resource has a pending replacement, extraction uses that current replacement; otherwise it extracts the firmware original.

CRPack export follows `../Canopus-Module-Resource-Hook/docs/interconnect_proto.md`. Import and export do not impose a file-count limit; export validates safe paths, `themeId`, mapping count and generated `mappings.tsv` size, manifest size and the 64 MiB uncompressed package limit. It does not install a pack or send it to the watch. Packs with more than 128 files require an updated device Manager; Interconnect's four-digit hexadecimal file index still limits a single transfer to 65,536 files, including `canora.json`.

The initial target is Xiaomi Band 11 firmware `4.100.155`; `targets` is inferred from the selected firmware filename when possible and remains editable. The importer streams the ROMFS tree into metadata and reads original file bytes lazily; it does not keep a full expanded ROMFS in memory. AstroBox's file-picker still passes the complete compressed archive to the WASM plugin. For deflated ROMFS entries, loading a selected file may need to inflate the stream up to that file's offset, trading latency for lower memory use. Actual imports remain subject to available host memory.

## Native CLI: one icon theme, multiple firmwares

The CLI separates shared source assets in `theme.json` from per-firmware resource bindings in `targets/*.json`. Each target pins the SHA-256 of its complete firmware (no per-resource hashes) and uses that firmware's original image templates. Builds produce one `.crpack` per target; no model service or AstroBox host is required.

```bash
cargo install --path crates/conora-cli --locked
conora init my-icons --theme-id dark --name "Dark Icons" \
  --firmware /path/to/firmware-A.bin --target band11-A
conora target add band11-B --theme my-icons --firmware /path/to/firmware-B.bin
conora ls --theme my-icons --target band11-A --images --json
# Add source images and declare logical icon roles and target bindings.
conora check --theme my-icons --all-targets --json
conora build --theme my-icons --all-targets --output ./packs
```

After the core and CLI crates are published to crates.io, installation will also be available with `cargo install conora-cli --locked`. They are not published by this repository change.

Existing packs can be imported with `conora import pack.crpack --into ./theme --firmware firmware.bin --target p67-3.101.043`. `conora plan --theme ./theme --from p67-3.101.043 --target q66-4.100.155` proposes bindings without changing them; `conora preview --theme ./theme --target q66-4.100.155 --verify` previews actual encoded resources and verifies conversion. Explicit target exclusions let one firmware retain icons another firmware cannot use. Native batch operations traverse compressed resources once instead of restarting decompression for every icon.

See [the CLI guide](crates/conora-cli/README.md) for import preservation/limitations, schemas, extraction, exclusions, planning, previews, machine-readable diagnostics and safety rules.

## AstroBox plugin build

Requires an AstroBox NG host with API Level 4 support.

API Level 4 uses async WIT exports, while the Rust guest standard library must target WASI P2:

```bash
rustup target add wasm32-wasip2
python3 scripts/build_dist.py --release --package
```

The `.abp` output is the installable AstroBox plugin. A `.crpack` is generated from either the plugin UI or the native CLI.

The root package remains the plugin; `crates/conora-core` and `crates/conora-cli` are workspace members. Default Cargo commands build/test the native core and CLI. The plugin script selects its package and `wasm32-wasip2` target explicitly.

## Tests

```bash
cargo test --locked
cargo test -p conora-crpack-builder
CONORA_TEST_FIRMWARE="$HOME/develop/temp/miwear.watch.q66tc_v4.100.155_full_f1c824fe.bin" \
  cargo test -p conora-core parses_real_firmware_when_requested -- --nocapture
```
