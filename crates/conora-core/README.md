# Conora Core

Shared Rust implementation for the AstroBox Conora editor and the native `conora` CLI.

- Inspect OTA ZIP/JAR archives containing `vela_resource.bin`, or raw ROMFS images.
- Inspect and convert supported LVGL, PNG and JPEG images using firmware originals as templates.
- Build and strictly parse CRPack v1 archives.
- Build declarative icon themes against multiple independently fingerprinted firmwares.

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

Native project firmware reads are limited to 512 MiB, config files to 1 MiB, and assets/original conversion templates to 64 MiB. Special-file inputs are rejected; Unix reads avoid blocking on FIFOs.

PNG inputs are proportionally resized to each original template with nearest-neighbor sampling. Different aspect ratios are rejected; use a target asset override rather than implicit cropping. Palette quantization needs explicit opt-in. Native color precision reduction (RGB565, alpha-only formats) and JPEG encoding can be intrinsically lossy and are reported separately. Raw replacements are copied without certifying device compatibility.

CRPack structure validation is not a device compatibility guarantee. `device`/manifest `targets` values are advisory metadata. The core does not install or transmit packs. The AstroBox UI imports replacement files but still rebuilds mappings when exporting; it is not a lossless editor for arbitrary third-party manifests.
