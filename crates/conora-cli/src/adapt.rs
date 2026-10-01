//! Read-only cross-firmware binding suggestions; never certifies semantic equivalence.
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use clap::Args;
use conora_core::{firmware::ResourceFile, lvgl, project};
use serde_json::{Value, json};

use super::{Failure, Result, image_json};

const MAX_PLAN_ROLES: usize = 4096;
const MAX_CANDIDATES: usize = 5;
const MAX_COMPARE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Args)]
pub(super) struct Plan {
    #[arg(long, default_value = ".")]
    theme: PathBuf,
    /// Existing source firmware target with reviewed bindings.
    #[arg(long)]
    from: String,
    /// Existing destination firmware target; this command does not edit its bindings.
    #[arg(long)]
    target: String,
    /// Compare normalized artwork with shortlisted firmware images (advisory only).
    #[arg(long)]
    compare_images: bool,
}

fn tokens(value: &str) -> BTreeSet<String> {
    value
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty() && !part.bytes().all(|b| b.is_ascii_digit()))
        .map(str::to_ascii_lowercase)
        .filter(|part| {
            !matches!(
                part.as_str(),
                "app" | "bin" | "png" | "launcher" | "icon" | "resource"
            )
        })
        .collect()
}

fn canonical(token: &str) -> &str {
    match token {
        "music" | "media" => "music",
        "oxygen" | "spo2" => "oxygen",
        "pressure" | "stress" => "pressure",
        "todo" | "todolist" | "dealt" => "todo",
        "card" | "nfccard" => "card",
        "camera" | "remote" => "camera",
        "findphone" | "find" => "findphone",
        "sports" | "training" => "sports",
        _ => token,
    }
}

fn normalized_tokens(value: &str) -> BTreeSet<String> {
    tokens(value)
        .into_iter()
        .map(|token| canonical(&token).to_owned())
        .collect()
}

fn paths(binding: &project::Binding) -> Vec<&str> {
    match binding {
        project::Binding::One(path) => vec![path],
        project::Binding::Many(paths) => paths.iter().map(String::as_str).collect(),
    }
}

fn dimensions_compatible(
    source: Option<lvgl::ImageInfo>,
    candidate: Option<lvgl::ImageInfo>,
) -> Option<bool> {
    match (source, candidate) {
        (Some(a), Some(b)) if a.width != 0 && a.height != 0 && b.width != 0 && b.height != 0 => {
            Some(
                u64::from(a.width) * u64::from(b.height)
                    == u64::from(a.height) * u64::from(b.width),
            )
        }
        _ => None,
    }
}

pub(super) fn run(args: Plan) -> Result<Value> {
    let theme = project::load_theme(&args.theme)?;
    if theme.theme.icons.len() > MAX_PLAN_ROLES {
        return Err(Failure::new(
            "plan_limit",
            "planning supports at most 4096 icon roles per invocation",
        ));
    }
    let source = project::load_target(&theme, &args.from)?;
    let destination = project::load_target(&theme, &args.target)?;
    eprintln!(
        "Planning {} -> {}: loading pinned firmware inventories",
        args.from, args.target
    );
    let source_firmware = project::load_firmware(
        &project::target_firmware_path(&theme, &args.from, &source),
        Some(&source.firmware_sha256),
    )?;
    let destination_firmware = project::load_firmware(
        &project::target_firmware_path(&theme, &args.target, &destination),
        Some(&destination.firmware_sha256),
    )?;
    let files = destination_firmware.index.files();
    let mut by_token: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut by_path = BTreeMap::new();
    for (index, file) in files.iter().enumerate() {
        by_path.insert(file.path.as_str(), index);
        for token in normalized_tokens(&file.path) {
            by_token.entry(token).or_default().push(index);
        }
    }
    let mut roles = Vec::new();
    let mut compare_paths = BTreeSet::new();
    let mut warnings = Vec::new();
    for role in theme.theme.icons.keys() {
        let source_paths = source.bindings.get(role).map(paths).unwrap_or_default();
        let reference = source_paths
            .iter()
            .find_map(|path| source_firmware.index.file(path))
            .and_then(|file| file.image);
        let mut role_tokens = normalized_tokens(role);
        for path in &source_paths {
            role_tokens.extend(normalized_tokens(path));
        }
        let mut indices = BTreeSet::new();
        for path in &source_paths {
            if let Some(index) = by_path.get(path) {
                indices.insert(*index);
            }
            if source_firmware.index.file(path).is_none() {
                warnings
                    .push(json!({"code":"source_resource_missing", "role":role, "resource":path}));
            }
        }
        for token in &role_tokens {
            if let Some(matches) = by_token.get(token) {
                indices.extend(matches);
            }
        }
        let mut ranked: Vec<(i32, &ResourceFile, Vec<&str>)> = indices
            .into_iter()
            .map(|index| {
                let file = &files[index];
                let exact = source_paths.contains(&file.path.as_str());
                let candidate_tokens = normalized_tokens(&file.path);
                let overlap = role_tokens.intersection(&candidate_tokens).count() as i32;
                let mut reasons = Vec::new();
                let mut score = overlap * 20;
                if exact {
                    score += 1000;
                    reasons.push("same_path");
                }
                if overlap > 0 {
                    reasons.push("name_or_alias_overlap");
                }
                if reference.is_some() && reference == file.image {
                    score += 4;
                    reasons.push("same_image_metadata");
                }
                if let (Some(a), Some(b)) = (reference, file.image)
                    && a.width == b.width
                    && a.height == b.height
                {
                    score += 8;
                    reasons.push("same_dimensions");
                }
                if source_paths.iter().any(|path| {
                    path.starts_with("app/launcher/") || path.ends_with("/launcher.bin")
                }) && file.path.ends_with("/launcher.bin")
                {
                    score += 30;
                    reasons.push("launcher_slot_candidate");
                }
                if dimensions_compatible(reference, file.image) == Some(true) {
                    score += 2;
                    reasons.push("same_aspect_ratio");
                }
                (score, file, reasons)
            })
            .collect();
        ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.path.cmp(&b.1.path)));
        ranked.truncate(MAX_CANDIDATES);
        let candidates: Vec<Value> = ranked.into_iter().map(|(score, file, reasons)| {
            if args.compare_images && file.image.is_some() { compare_paths.insert(file.path.clone()); }
            json!({"path":file.path,"score":score,"reasons":reasons,"image":file.image.map(image_json),
                "aspectRatioCompatible":dimensions_compatible(reference,file.image),"requiresConfirmation":true})
        }).collect();
        roles.push(json!({"role":role,"sourceResources":source_paths,"sourceImage":reference.map(image_json),"sourceExcludedReason":source.excluded.get(role),
            "existingBinding":destination.bindings.get(role).map(paths),
            "excludedReason":destination.excluded.get(role),
            "status":if candidates.is_empty(){"unmatched"}else{"review_required"}, "candidates":candidates}));
    }
    if args.compare_images {
        eprintln!("Comparing shortlisted image candidates (not a compatibility guarantee)");
        let selected: Vec<String> = compare_paths.into_iter().collect();
        let mut thumbnails = BTreeMap::new();
        let compared = destination_firmware.index.visit_file_bytes(&selected, project::MAX_TEMPLATE_BYTES, MAX_COMPARE_BYTES, |path, bytes| {
            match lvgl::decode_to_rgba(bytes) {
                Ok((_, pixels)) => {
                    let normalized = image::imageops::resize(&pixels,24,24,image::imageops::FilterType::Nearest);
                    thumbnails.insert(path.to_owned(), normalized);
                }
                Err(error) => warnings.push(json!({"code":"candidate_image_unavailable","resource":path,"message":error})),
            }
            Ok(())
        });
        if let Err(error) = compared {
            warnings.push(json!({"code":"image_comparison_unavailable","message":error}));
        }
        for report in &mut roles {
            if report["candidates"].as_array().unwrap().is_empty() {
                continue;
            }
            let role = report["role"].as_str().unwrap();
            if source.excluded.contains_key(role) {
                continue;
            }
            let asset = &theme.theme.icons[role];
            let asset = source.overrides.get(role).unwrap_or(asset);
            let input = match asset {
                project::Asset::Png(path) => path,
                project::Asset::Detailed(options) => &options.input,
            };
            let bytes =
                match project::read_limited(&theme.root.join(input), project::MAX_TEMPLATE_BYTES) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        warnings.push(
                            json!({"code":"source_image_unavailable","role":role,"message":error}),
                        );
                        continue;
                    }
                };
            let Ok((_, pixels)) = lvgl::decode_to_rgba(&bytes) else {
                warnings.push(json!({"code":"source_image_unavailable","role":role,"message":"Artwork could not be decoded; lexical candidates remain available."}));
                continue;
            };
            let normalized =
                image::imageops::resize(&pixels, 24, 24, image::imageops::FilterType::Nearest);
            for candidate in report["candidates"].as_array_mut().unwrap() {
                if let Some(other) = thumbnails.get(candidate["path"].as_str().unwrap()) {
                    // Premultiply RGB so invisible transparent RGB does not affect the distance.
                    let difference: u64 = normalized
                        .pixels()
                        .zip(other.pixels())
                        .map(|(a, b)| {
                            let mut delta = u64::from(a[3].abs_diff(b[3]));
                            for c in 0..3 {
                                let x = u16::from(a[c]) * u16::from(a[3]) / 255;
                                let y = u16::from(b[c]) * u16::from(b[3]) / 255;
                                delta += u64::from(x.abs_diff(y));
                            }
                            delta
                        })
                        .sum();
                    candidate["artworkSimilarity"] =
                        json!(1.0 - difference as f64 / (24.0 * 24.0 * 4.0 * 255.0));
                }
            }
        }
    }
    let unmatched: Vec<Value> = roles
        .iter()
        .filter(|role| role["status"] == "unmatched")
        .map(|role| role["role"].clone())
        .collect();
    Ok(
        json!({"from":args.from,"target":args.target,"sourceFirmwareSha256":source_firmware.sha256,
        "firmwareSha256":destination_firmware.sha256,"readOnly":true,"requiresConfirmation":true,
        "roles":roles,"unmatched":unmatched,"warnings":warnings,
        "note":"Candidates are advisory. Review semantics, one-to-many slots and size variants before editing bindings; explicitly exclude roles with no valid destination."}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aliases_are_path_token_hints_not_substring_matching() {
        assert!(normalized_tokens("app/media/launcher.bin").contains("music"));
        assert!(normalized_tokens("oxygen").contains("oxygen"));
        assert!(normalized_tokens("app/spo2/launcher.bin").contains("oxygen"));
        assert!(!normalized_tokens("app/timer/icon/stop.bin").contains("launcher"));
    }
}
