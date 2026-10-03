//! Previews are decoded from validated, built replacements, never from source assets.
use std::collections::{BTreeMap, HashSet};
use std::io::Cursor;
use std::path::PathBuf;

use clap::Args;
use corona_core::{crpack, lvgl, project};
use image::{DynamicImage, ImageReader, Limits, RgbaImage, imageops};
use serde_json::{Value, json};
use tempfile::NamedTempFile;

use super::{Failure, Result, Selection};

const MAX_PIXELS: u64 = 16 * 1024 * 1024;
const TILE: u32 = 112;
const COLUMNS: u32 = 8;
// 896x3584 RGBA is under 16 MiB, and both dimensions are under 4096.
const PAGE_TILES: usize = 256;

#[derive(Args)]
pub(super) struct Preview {
    #[command(flatten)]
    selection: Selection,
    /// Output directory (default: previews inside the theme project).
    #[arg(long)]
    output: Option<PathBuf>,
    /// Replace existing previews, but never project inputs.
    #[arg(long)]
    force: bool,
    /// Also verify template or source-dimension metadata/header and source pixels.
    #[arg(long)]
    verify: bool,
}

struct Publisher<'a> {
    directory: PathBuf,
    protected: &'a [PathBuf],
    force: bool,
    destinations: HashSet<PathBuf>,
    staged: Vec<(NamedTempFile, PathBuf)>,
}

impl Publisher<'_> {
    fn add(&mut self, name: &str, bytes: &[u8]) -> Result<PathBuf> {
        let path = self.directory.join(name);
        super::path_text(&path)?;
        if !self.destinations.insert(super::absolute(&path)?) {
            return Err(Failure::new(
                "preview_collision",
                "duplicate preview filename",
            ));
        }
        super::preflight(&path, self.force, self.protected)?;
        super::make_dir(&self.directory)?;
        self.staged
            .push((super::stage(&path, bytes)?, path.clone()));
        Ok(path)
    }
}

pub(super) fn run(args: Preview) -> Result<Value> {
    let theme = project::load_theme(&args.selection.theme)?;
    let ids = if let Some(id) = &args.selection.target {
        project::validate_target_id(id)?;
        vec![id.clone()]
    } else {
        project::target_ids(&theme)?
    };
    if ids.is_empty() {
        return Err(Failure::new("no_targets", "theme has no targets"));
    }
    let mut protected = super::project_inputs(&theme, None)?;
    protected.push(super::theme_input(&args.selection.theme, &theme));
    let mut publisher = Publisher {
        directory: args.output.unwrap_or_else(|| theme.root.join("previews")),
        protected: &protected,
        force: args.force,
        destinations: HashSet::new(),
        staged: Vec::new(),
    };
    super::path_text(&publisher.directory)?;
    let mut reports = Vec::new();
    let mut first_failure = None;
    for id in ids {
        eprintln!("Preparing previews for {id}...");
        // Only one prepared pack (and one decoded resource) is retained at a time.
        let mut last_progress = std::time::Instant::now();
        let prepared = project::prepare_target_with_progress(&theme, &id, |message| {
            if last_progress.elapsed() >= std::time::Duration::from_secs(2) {
                eprintln!("{id}: {message}");
                last_progress = std::time::Instant::now();
            }
        });
        let mut report = serde_json::to_value(&prepared.report)
            .map_err(|e| Failure::new("json", e.to_string()))?;
        if prepared.report.valid
            && let Some(bytes) = prepared.pack.as_ref()
        {
            match target_previews(
                &theme,
                &id,
                bytes,
                &prepared.report,
                args.verify,
                &mut publisher,
            ) {
                Ok(preview) => report["preview"] = preview,
                Err(failure) => {
                    report["valid"] = json!(false);
                    report["errors"].as_array_mut().unwrap().push(json!({
                        "code": failure.code, "message": failure.message
                    }));
                    if first_failure.is_none() {
                        first_failure = Some(failure);
                    }
                }
            }
        } else if first_failure.is_none() {
            first_failure = Some(Failure::new(
                "validation",
                "one or more targets failed validation; no previews published",
            ));
        }
        reports.push(report);
    }
    if let Some(mut failure) = first_failure {
        failure.details = json!({"targets": reports, "outputs": [], "partialCommit": false});
        return Err(failure);
    }
    let mut published = Vec::new();
    for (file, destination) in publisher.staged {
        // Check again immediately before publishing, including input aliases.
        if let Err(mut failure) = super::preflight(&destination, args.force, &protected)
            .and_then(|_| super::commit(file, &destination, args.force))
        {
            failure.details = json!({"targets": reports, "outputs": published, "partialCommit": !published.is_empty()});
            return Err(failure);
        }
        published.push(destination);
    }
    Ok(json!({"targets": reports, "outputs": published}))
}

fn target_previews(
    theme: &project::ThemeProject,
    id: &str,
    bytes: &[u8],
    report: &project::TargetReport,
    verify: bool,
    publisher: &mut Publisher<'_>,
) -> Result<Value> {
    let pack = crpack::parse_crpack(bytes)?;
    let manifest: Value = serde_json::from_slice(&pack.manifest_bytes)
        .map_err(|e| Failure::new("manifest", e.to_string()))?;
    let verification = if verify {
        verify_replacements(theme, id, &pack.replacements, report)?
    } else {
        BTreeMap::new()
    };
    let mut entries = Vec::new();
    let mut sheets = Vec::new();
    let mut sheet = RgbaImage::new(COLUMNS * TILE, 32 * TILE);
    let mut tile_count = 0usize;
    let mut image_count = 0usize;
    let mut skipped = 0usize;
    // Reports have deterministic role/resource ordering and identify overrides as well.
    for resource in &report.resources {
        let archive_path = resource
            .archive_path
            .clone()
            .unwrap_or_else(|| super::icon::archive_path(&resource.resource));
        let built = pack.replacements.get(&archive_path).ok_or_else(|| {
            Failure::new(
                "preview_pack",
                format!("built pack omits {}", resource.resource),
            )
        })?;
        // Resource identities may contain opaque QuickApp packages (including
        // path separators); only the hashed ID belongs in preview filenames.
        let resource_id = resource_id(&resource.role, &resource.resource);
        let mut entry = json!({"id": resource_id, "role": resource.role, "resource": resource.resource,
            "archivePath":archive_path,"origin":resource.origin,
            "mode": resource.mode, "lossy": resource.lossy});
        if resource.origin.as_deref() == Some("runtime") {
            entry["deviceCompatibility"] = json!("unverified");
        }
        if let Some(value) = verification.get(&resource.resource) {
            entry["verification"] = value.clone();
        }
        let opaque_runtime = resource.origin.as_deref() == Some("runtime")
            && resource.mode == project::AssetMode::Raw;
        if lvgl::inspect_image(built).is_none() {
            if !opaque_runtime
                && (built.starts_with(b"\x89PNG\r\n\x1a\n") || built.starts_with(b"\xff\xd8\xff"))
            {
                return Err(Failure::new(
                    "preview_decode",
                    format!("invalid or oversized raster image: {}", resource.resource),
                ));
            }
            if resource.mode != project::AssetMode::Raw {
                return Err(Failure::new(
                    "preview_decode",
                    format!("built replacement is not an image: {}", resource.resource),
                ));
            }
            entry["status"] = json!("skipped");
            entry["reason"] = json!("raw replacement is not a supported image");
            skipped += 1;
            entries.push(entry);
            continue;
        }
        let (info, image) = match decode_bounded(built) {
            Ok(image) => image,
            Err(error) if opaque_runtime => {
                // A raw runtime file is allowed to resemble an image header without
                // being one. Optional bounded preview must not invalidate its pack.
                entry["status"] = json!("skipped");
                entry["reason"] = json!(format!(
                    "raw runtime file cannot be previewed as an image: {error}"
                ));
                skipped += 1;
                entries.push(entry);
                continue;
            }
            Err(error) => {
                return Err(Failure::new(
                    "preview_decode",
                    format!("{}: {error}", resource.resource),
                ));
            }
        };
        // Individual files are also bounded; index retains actual built dimensions.
        let preview = fit(&image, 2048);
        let filename = format!("preview-{id}-{resource_id}.png");
        publisher.add(&filename, &png(&preview)?)?;
        let thumbnail = fit(&image, TILE);
        let col = tile_count as u32 % COLUMNS;
        let row = tile_count as u32 / COLUMNS;
        let x = col * TILE + (TILE - thumbnail.width()) / 2;
        let y = row * TILE + (TILE - thumbnail.height()) / 2;
        imageops::replace(&mut sheet, &thumbnail, i64::from(x), i64::from(y));
        let sheet_name = format!("contact_sheet-{id}-{:04}.png", sheets.len() + 1);
        entry["status"] = json!("previewed");
        entry["png"] = json!(filename);
        entry["image"] = super::image_json(info);
        entry["previewSize"] = json!({"width": preview.width(), "height": preview.height()});
        entry["tile"] = json!({"sheet": sheet_name, "index": tile_count, "column": col, "row": row, "x": x, "y": y,
            "width": thumbnail.width(), "height": thumbnail.height()});
        entries.push(entry);
        image_count += 1;
        tile_count += 1;
        if tile_count == PAGE_TILES {
            flush_sheet(publisher, &sheet_name, &sheet, tile_count)?;
            sheets.push(sheet_name);
            // All tiles are replaced on the next full page; clear the unused tail.
            sheet.fill(0);
            tile_count = 0;
        }
    }
    if tile_count > 0 {
        let name = format!("contact_sheet-{id}-{:04}.png", sheets.len() + 1);
        flush_sheet(publisher, &name, &sheet, tile_count)?;
        sheets.push(name);
    }
    let filename = format!("preview_index-{id}.json");
    let verified_count = verification
        .values()
        .filter(|v| v["status"] == "verified")
        .count();
    let fully_verified = verify && verified_count == report.resources.len();
    let index = json!({"schemaVersion": 1, "target": id, "firmwareSha256": report.firmware_sha256,
        "verified": fully_verified, "verificationRequested":verify, "verifiedResources":verified_count,
        "manifest": manifest, "contactSheets": sheets, "resources": entries});
    let path = publisher.add(&filename, &super::json_bytes(&index)?)?;
    eprintln!(
        "{id}: {image_count} image previews, {skipped} skipped raw resources, {} contact sheets",
        sheets.len()
    );
    Ok(
        json!({"index": path, "images": image_count, "skipped": skipped, "verified": fully_verified,
            "verificationRequested":verify,"verifiedResources":verified_count,"contactSheets": sheets}),
    )
}

fn flush_sheet(
    publisher: &mut Publisher<'_>,
    name: &str,
    sheet: &RgbaImage,
    count: usize,
) -> Result<()> {
    let height = (count as u32).div_ceil(COLUMNS) * TILE;
    let cropped = imageops::crop_imm(sheet, 0, 0, sheet.width(), height).to_image();
    publisher.add(name, &png(&cropped)?)?;
    Ok(())
}

fn png(image: &RgbaImage) -> Result<Vec<u8>> {
    // Encoder borrows the image; avoid cloning a complete contact sheet.
    let mut bytes = Vec::new();
    use image::ImageEncoder;
    image::codecs::png::PngEncoder::new(&mut bytes)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| Failure::new("preview_encode", e.to_string()))?;
    Ok(bytes)
}

fn fit(image: &RgbaImage, bound: u32) -> RgbaImage {
    let largest = image.width().max(image.height());
    if largest <= bound {
        return image.clone();
    }
    let w = ((u64::from(image.width()) * u64::from(bound)) / u64::from(largest)).max(1) as u32;
    let h = ((u64::from(image.height()) * u64::from(bound)) / u64::from(largest)).max(1) as u32;
    imageops::resize(image, w, h, imageops::FilterType::Nearest)
}

// Stable filename ID, independent of source filenames, punctuation and binding order.
// Publisher rejects collisions rather than ever overwriting another resource's preview.
fn resource_id(role: &str, path: &str) -> String {
    let hash = role
        .bytes()
        .chain([0])
        .chain(path.bytes())
        .fold(0xcbf29ce484222325u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
        });
    format!("{hash:016x}")
}

fn raster_bounded(bytes: &[u8]) -> std::result::Result<RgbaImage, String> {
    if bytes.len() > project::MAX_TEMPLATE_BYTES {
        return Err("image exceeds 64 MiB limit".into());
    }
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_PIXELS as u32);
    limits.max_image_height = Some(MAX_PIXELS as u32);
    limits.max_alloc = Some(MAX_PIXELS * 16 + 1024 * 1024);
    reader.limits(limits.clone());
    let (w, h) = reader.into_dimensions().map_err(|e| e.to_string())?;
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > MAX_PIXELS {
        return Err("image exceeds 16-megapixel limit".into());
    }
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    reader.limits(limits);
    reader
        .decode()
        .map(DynamicImage::into_rgba8)
        .map_err(|e| e.to_string())
}

fn decode_bounded(bytes: &[u8]) -> std::result::Result<(lvgl::ImageInfo, RgbaImage), String> {
    if bytes.len() > project::MAX_TEMPLATE_BYTES {
        return Err("replacement exceeds 64 MiB limit".into());
    }
    let mut info = lvgl::inspect_image(bytes).ok_or("not a supported image")?;
    if matches!(
        info.format,
        lvgl::ImageFormatKind::Png | lvgl::ImageFormatKind::Jpeg
    ) {
        // Keep preview source decoding and replacement decoding under the same pixel budget.
        let image = raster_bounded(bytes)?;
        info.width =
            u16::try_from(image.width()).map_err(|_| "image width exceeds metadata limit")?;
        info.height =
            u16::try_from(image.height()).map_err(|_| "image height exceeds metadata limit")?;
        return Ok((info, image));
    }
    if u64::from(info.width) * u64::from(info.height) > MAX_PIXELS {
        return Err("image exceeds 16-megapixel limit".into());
    }
    if info.format.is_rle() {
        let header = bytes.get(20..24).ok_or("truncated RLE header")?;
        let expanded = u32::from_le_bytes(header.try_into().unwrap()) as usize;
        let palette = match info.format {
            lvgl::ImageFormatKind::Lvgl9I8Rle => 1024,
            lvgl::ImageFormatKind::Lvgl9I4Rle => 64,
            _ => 0,
        };
        let expected = usize::from(info.stride) * usize::from(info.height) + palette;
        if expanded < expected || expanded > project::MAX_TEMPLATE_BYTES {
            return Err("RLE expanded payload does not match bounded image dimensions".into());
        }
    }
    lvgl::decode_to_rgba(bytes)
}

fn verify_replacements(
    theme: &project::ThemeProject,
    id: &str,
    replacements: &BTreeMap<String, Vec<u8>>,
    report: &project::TargetReport,
) -> Result<BTreeMap<String, Value>> {
    let target = project::load_target(theme, id)?;
    let firmware = project::load_firmware(
        &project::target_firmware_path(theme, id, &target),
        Some(&target.firmware_sha256),
    )?;
    let summaries: BTreeMap<_, _> = report
        .resources
        .iter()
        .map(|r| (r.resource.as_str(), r))
        .collect();
    let mut values = BTreeMap::new();
    let mut paths = Vec::new();
    for summary in &report.resources {
        let path = &summary.resource;
        let summary = summaries
            .get(path.as_str())
            .ok_or_else(|| Failure::new("preview_verify", "replacement has no resource summary"))?;
        if summary.mode == project::AssetMode::Raw {
            values.insert(path.clone(), json!({"status": "notApplicable", "reason": "raw replacements have no PNG conversion contract"}));
        } else if summary.origin.as_deref() == Some("runtime") {
            let archive_path = summary.archive_path.as_deref().ok_or_else(|| {
                Failure::new("preview_verify", "runtime replacement has no archive path")
            })?;
            let asset = target.runtime_files.get(archive_path).ok_or_else(|| {
                Failure::new("preview_verify", "runtime replacement has no asset")
            })?;
            let built = replacements
                .get(archive_path)
                .ok_or_else(|| Failure::new("preview_verify", "runtime replacement missing"))?;
            let mut proof = if let Some(template) = &asset.template {
                let template =
                    project::read_limited(&theme.root.join(template), project::MAX_TEMPLATE_BYTES)?;
                verify_image(summary, &template, built)
            } else {
                verify_template_free_icon(summary, built)
            }
            .map_err(|e| Failure::new("preview_verify", format!("{archive_path}: {e}")))?;
            proof["scope"] = json!("encoding");
            proof["deviceCompatibility"] = json!("unverified");
            values.insert(path.clone(), proof);
        } else if let Some(asset) = super::icon::asset(theme, &target, path)? {
            let original = if let Some(template) = asset["template"].as_str() {
                Some(project::read_limited(
                    &theme.root.join(template),
                    project::MAX_TEMPLATE_BYTES,
                )?)
            } else if path == corona_core::app_icons::CANOPUS_SOURCE {
                Some(corona_core::app_icons::canopus_template())
            } else {
                None
            };
            let built = replacements
                .get(&super::icon::archive_path(path))
                .ok_or_else(|| Failure::new("preview_verify", "native replacement missing"))?;
            let verified = match original {
                Some(original) => verify_image(summary, &original, built),
                None => verify_template_free_icon(summary, built),
            }
            .map_err(|e| Failure::new("preview_verify", format!("{path}: {e}")))?;
            values.insert(path.clone(), verified);
        } else {
            paths.push(path.clone());
        }
    }
    firmware
        .index
        .visit_file_bytes(
            &paths,
            project::MAX_TEMPLATE_BYTES,
            project::MAX_TEMPLATE_BYTES,
            |path, original| {
                let summary = summaries
                    .get(path)
                    .ok_or("replacement has no resource summary")?;
                let built = replacements.get(path).ok_or("replacement missing")?;
                let verified =
                    verify_image(summary, original, built).map_err(|e| format!("{path}: {e}"))?;
                values.insert(path.to_owned(), verified);
                Ok(())
            },
        )
        .map_err(|e| Failure::new("preview_verify", e))?;
    Ok(values)
}

fn verify_template_free_icon(
    summary: &project::ResourceSummary,
    built: &[u8],
) -> std::result::Result<Value, String> {
    let source = project::read_limited(&summary.input, project::MAX_TEMPLATE_BYTES)?;
    let source = raster_bounded(&source)?;
    let width = u16::try_from(source.width()).map_err(|_| "source width exceeds LVGL v9 limit")?;
    let height =
        u16::try_from(source.height()).map_err(|_| "source height exceeds LVGL v9 limit")?;
    let stride = width
        .checked_mul(4)
        .ok_or("source stride exceeds LVGL v9 limit")?;
    let expected_info = lvgl::ImageInfo {
        format: lvgl::ImageFormatKind::Lvgl9Argb8888,
        width,
        height,
        stride,
    };
    let mut expected_header = vec![0x19, 0x10, 0, 0];
    expected_header.extend_from_slice(&width.to_le_bytes());
    expected_header.extend_from_slice(&height.to_le_bytes());
    expected_header.extend_from_slice(&stride.to_le_bytes());
    expected_header.extend_from_slice(&[0, 0]);
    let (info, actual) = decode_bounded(built)?;
    let same_metadata = info == expected_info;
    let same_header = built.get(..12) == Some(expected_header.as_slice());
    let exact_size = built.len() == 12 + usize::from(stride) * usize::from(height);
    if !same_metadata || !same_header || !exact_size {
        return Err("built icon differs from source-dimension LVGL v9 ARGB8888 layout".into());
    }
    let same_pixels = actual == source;
    if !same_pixels || summary.lossy {
        return Err("built icon pixels differ from unresized RGBA source".into());
    }
    Ok(
        json!({"status":"verified", "contract":"sourceDimensionsArgb8888",
        "sameMetadata":same_metadata, "sameHeader":same_header, "samePixels":same_pixels, "lossy":false}),
    )
}

fn verify_image(
    summary: &project::ResourceSummary,
    original: &[u8],
    built: &[u8],
) -> std::result::Result<Value, String> {
    let original_info = lvgl::inspect_image(original).ok_or("original template is not an image")?;
    let (info, actual) = decode_bounded(built)?;
    let same_metadata = info == original_info;
    let header_len = match info.format {
        lvgl::ImageFormatKind::Png | lvgl::ImageFormatKind::Jpeg => 0,
        lvgl::ImageFormatKind::Lvgl8I8 | lvgl::ImageFormatKind::Lvgl8Rgb565 => 4,
        _ => 12,
    };
    let same_header = built.get(..header_len) == original.get(..header_len);
    if !same_metadata || !same_header {
        return Err("built image metadata/template header differs from original".into());
    }
    let source = project::read_limited(&summary.input, project::MAX_TEMPLATE_BYTES)?;
    let source = raster_bounded(&source)?;
    let expected = imageops::resize(
        &source,
        actual.width(),
        actual.height(),
        summary.filter.to_image_filter(),
    );
    let same_pixels = actual == expected;
    if !same_pixels && !summary.lossy {
        return Err("built pixels differ from resized source".into());
    }
    Ok(json!({"status":"verified", "sameMetadata":same_metadata,
        "sameHeader":same_header, "samePixels":same_pixels, "lossy":summary.lossy}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_free_verification_rejects_changed_header_or_rgba_pixels() {
        let dir = tempfile::tempdir().unwrap();
        let source = RgbaImage::from_pixel(3, 2, image::Rgba([11, 22, 33, 44]));
        let input = dir.path().join("source.png");
        let png = png(&source).unwrap_or_else(|_| panic!("fixture PNG encoding failed"));
        std::fs::write(&input, &png).unwrap();
        let summary = project::ResourceSummary {
            role: "quickapp:org.own".into(),
            resource: corona_core::app_icons::source("org.own"),
            archive_path: None,
            origin: None,
            input,
            mode: project::AssetMode::Png,
            size_bytes: 36,
            format: Some("LVGL v9 ARGB8888".into()),
            width: Some(3),
            height: Some(2),
            lossy: false,
            filter: lvgl::ResizeFilter::Nearest,
        };
        let encoded = lvgl::encode_png_argb8888(&png).unwrap().bytes;
        let proof = verify_template_free_icon(&summary, &encoded).unwrap();
        assert_eq!(proof["samePixels"], true);
        let mut changed = encoded.clone();
        changed[10] = 1;
        assert!(verify_template_free_icon(&summary, &changed).is_err());
        let mut changed = encoded.clone();
        changed[12] ^= 1;
        assert!(
            verify_template_free_icon(&summary, &changed)
                .unwrap_err()
                .contains("pixels")
        );
        let mut changed = encoded.clone();
        changed[15] ^= 1;
        assert!(
            verify_template_free_icon(&summary, &changed)
                .unwrap_err()
                .contains("pixels")
        );
        let mut changed = encoded;
        changed.push(0);
        assert!(verify_template_free_icon(&summary, &changed).is_err());
    }
}
