# Corona CRPack Builder

An AstroBox NG API Level 4 Rust/WASM editor and native `corona` CLI for creating Canopus Resource Pack (`.crpack`) v1 files from watch firmware. Both use the same `corona-core` library.

## What it does

- Opens a firmware `.bin` OTA ZIP/JAR containing `vela_resource.bin`, or a raw ROMFS image.
- Browses the ROMFS resource tree without changing the source firmware.
- Previews files recognized as LVGL images (v9 I8/A8/ARGB8888/I4/A4 uncompressed or RLE, v8 RGB565/I8), PNG, and JPEG.
- Extracts the selected resource in its current state; recognized image resources can also be converted and extracted as PNG.
- Replaces a selected resource with a PNG converted against its original BIN template (with bidirectional conversion across supported formats), or with arbitrary file bytes.
- Edits third-party application icons separately from the firmware tree: Canopus uses its exact runtime-path mapping, while QuickApps use package-based `quickappIcons` declarations.
- Exports only replaced files plus a root `corona.json` manifest as a `.crpack` ZIP. Imports also accept the legacy root `canora.json`, but reject packs containing both names. Re-export always uses `corona.json`.

PNG conversion keeps the original dimensions and stride. Inputs with the same aspect ratio are automatically scaled up or down to the original dimensions using nearest-neighbor sampling, preserving palette colors and transparent pixels; different aspect ratios are rejected without padding, cropping, or stretching. Same-size inputs are not resampled. PNG inputs are limited to 64 MiB and 16 * 1024 * 1024 pixels. PNGs with more than 256 RGBA colors require opting into lossy quantization. Unsupported BIN formats remain extractable and replaceable as ordinary files, but are not previewed or converted. When a resource has a pending replacement, extraction uses that current replacement; otherwise it extracts the firmware original.

CRPack export follows `../Canopus-Module-Resource-Hook/docs/interconnect_proto.md`. Import and export do not impose a file-count limit; export validates safe paths, `themeId`, mapping count and generated `mappings.tsv` size, manifest size and the 64 MiB uncompressed package limit. It does not install a pack or send it to the watch. Packs with more than 128 files require an updated device Manager; Interconnect's four-digit hexadecimal file index still limits a single transfer to 65,536 files, including `corona.json`.

The initial target is Xiaomi Band 11 firmware `4.100.155`; `targets` is inferred from the selected firmware filename when possible and remains editable. The importer streams the ROMFS tree into metadata and reads original file bytes lazily; it does not keep a full expanded ROMFS in memory. AstroBox's file-picker still passes the complete compressed archive to the WASM plugin. For deflated ROMFS entries, loading a selected file may need to inflate the stream up to that file's offset, trading latency for lower memory use. Actual imports remain subject to available host memory.

## Native CLI: one icon theme, multiple firmwares

The CLI separates shared source assets in `theme.json` from per-firmware resource bindings in `targets/*.json`. Each target pins the SHA-256 of its complete firmware (no per-resource hashes) and uses that firmware's original image templates. Builds produce one `.crpack` per target; no model service or AstroBox host is required.

```bash
cargo install --path crates/corona-cli --locked
corona init my-icons --theme-id dark --name "Dark Icons" \
  --firmware /path/to/firmware-A.bin --target band11-A
corona target add band11-B --theme my-icons --firmware /path/to/firmware-B.bin
corona ls --theme my-icons --target band11-A --images --json
# Add source images and declare logical icon roles and target bindings.
corona check --theme my-icons --all-targets --json
corona build --theme my-icons --all-targets --output ./packs
```

For installation from crates.io, use `cargo install corona-cli --locked`. The crate package is `corona-cli`; the installed executable is `corona`. The shared library is published separately as `corona-core`.

Existing packs can be imported with `corona import pack.crpack --into ./theme --firmware firmware.bin --target p67-3.101.043`. `corona plan --theme ./theme --from p67-3.101.043 --target q66-4.100.155` proposes bindings without changing them; `corona preview --theme ./theme --target q66-4.100.155 --verify` previews actual encoded resources and verifies conversion. Explicit target exclusions let one firmware retain icons another firmware cannot use. Native batch operations traverse compressed resources once instead of restarting decompression for every icon.

### Third-party application icons

```bash
# Canopus uses the known 117x117 LVGL v9 ARGB8888 layout by default.
corona icon set --canopus ./canopus.png --theme ./my-icons
# QuickApp PNGs need no template: retain their dimensions and alpha as LVGL v9 ARGB8888.
corona icon set --package ng.lst.corona ./corona.png \
  --theme ./my-icons
corona icon ls --theme ./my-icons --json
corona icon remove --package ng.lst.corona --theme ./my-icons
```

QuickApp PNGs without `--template` are encoded losslessly at their own dimensions as uncompressed LVGL v9 ARGB8888, retaining alpha. An optional original LVGL BIN template preserves the existing proportional-resize/layout behavior. Use `--raw` for an already encoded, supported LVGL BIN. Commands copy assets into the project and update declarations; existing `check`, `preview --verify`, and `build` include these icons without ordinary firmware bindings. Targets can override application assets/templates in their configs. CLI builds still use explicit fingerprinted firmware targets. The plugin's **Third-party application icons** section can import, preview, replace, and export these icons without loading firmware.

Canopus exports an exact mapping from `/data/canopus/manager_icon.bin`; QuickApps export optional `quickappIcons` entries with `package` and a safe archive-relative `.bin` `destination`. Both share the existing CRPack path, rule-count, manifest, TSV, and total-byte limits. Import/re-export preserves these declarations, including normalized `@quickapp-icon/<package>` rules. Package identifiers are opaque exact strings (even empty, Unicode or containing spaces/slashes), never trimmed or treated as paths. Only the 255-byte UTF-8 source budget and forbidden TSV bytes 0–31/127 apply. New QuickApp destinations use `quickapp-icons/<16-hex-hash>.bin`; imported safe destinations may be shared by multiple icons.

QuickApp icon application requires an updated device Manager/module and an approved exact device target; the current sibling contract supports Band 11 `.139/.155`, not `.043` or PNG/in-memory source icons. Old receivers may ignore this optional field. The Canopus preset is a layout-only template, not the original artwork, and is not a compatibility claim for other devices; supply an explicit template for another layout. Creating a pack neither installs applications nor writes device files, and successful encoding does not prove Launcher refresh on a watch.

See [the CLI guide](crates/corona-cli/README.md) for import preservation/limitations, schemas, extraction, exclusions, planning, previews, machine-readable diagnostics and safety rules.

## AstroBox plugin build

Requires an AstroBox NG host with API Level 4 support.

API Level 4 uses async WIT exports, while the Rust guest standard library must target WASI P2:

```bash
rustup target add wasm32-wasip2
python3 scripts/build_dist.py --release --package
```

The `.abp` output is the installable AstroBox plugin. A `.crpack` is generated from either the plugin UI or the native CLI.

The root package remains the plugin; `crates/corona-core` and `crates/corona-cli` are workspace members. Default Cargo commands build/test the native core and CLI. The plugin script selects its package and `wasm32-wasip2` target explicitly.

## Tests

```bash
cargo test --locked
cargo test -p corona-crpack-builder
CORONA_TEST_FIRMWARE="$HOME/develop/temp/miwear.watch.q66tc_v4.100.155_full_f1c824fe.bin" \
  cargo test -p corona-core parses_real_firmware_when_requested -- --nocapture
```
