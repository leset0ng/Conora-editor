//! Ordered runtime mappings with project-owned, staged input assets.
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use corona_core::{app_icons, crpack, project, runtime};
use serde_json::{Value, json};

use super::{Failure, Result};

#[derive(Subcommand)]
pub(super) enum MappingCommand {
    /// Append a runtime rule, copying any supplied input into project ownership.
    Add(Add),
    /// List runtime rules in their configured order, without loading firmware.
    #[command(alias = "ls")]
    List(Selection),
    /// Remove a runtime rule, retaining file declarations and copied assets.
    Remove(Remove),
}

#[derive(Args)]
pub(super) struct Selection {
    #[arg(long, default_value = ".")]
    theme: PathBuf,
    #[arg(long)]
    target: String,
}

#[derive(Args)]
pub(super) struct Add {
    #[command(flatten)]
    selection: Selection,
    #[arg(long)]
    source: String,
    /// Source file; omit only when aliasing an existing runtime destination.
    #[arg(long)]
    input: Option<PathBuf>,
    #[arg(long)]
    destination: Option<String>,
    /// Original LVGL BIN template for PNG conversion.
    #[arg(long, conflicts_with = "raw", requires = "input")]
    template: Option<PathBuf>,
    /// Copy unchanged even when the input is PNG.
    #[arg(long, requires = "input")]
    raw: bool,
    #[arg(long, conflicts_with = "raw", requires = "input")]
    allow_quantize: bool,
    #[arg(long, conflicts_with = "raw", requires = "input", value_parser = ["nearest", "lanczos3"])]
    filter: Option<String>,
    /// Update an existing source in place; shared destinations are never overwritten.
    #[arg(long)]
    force: bool,
}

#[derive(Args)]
pub(super) struct Remove {
    #[command(flatten)]
    selection: Selection,
    #[arg(long)]
    source: String,
}

impl MappingCommand {
    pub(super) fn name(&self) -> &'static str {
        match self {
            Self::Add(_) => "mapping add",
            Self::List(_) => "mapping list",
            Self::Remove(_) => "mapping remove",
        }
    }
}

pub(super) fn run(command: MappingCommand) -> Result<Value> {
    match command {
        MappingCommand::List(selection) => {
            let theme = project::load_theme(&selection.theme)?;
            let target = project::load_target(&theme, &selection.target)?;
            Ok(json!({"theme":super::theme_input(&selection.theme, &theme),
                "target":selection.target, "mappings":target.runtime_mappings,
                "runtimeFiles":target.runtime_files,
                "runtimeQuickappIcons":target.runtime_quickapp_icons,
                "runtimeQuickappIconCount":target.runtime_quickapp_icons.len(),
                "runtimeMappingCount":target.runtime_mappings.len(),
                "runtimeFileCount":target.runtime_files.len()}))
        }
        MappingCommand::Add(args) => add(args),
        MappingCommand::Remove(args) => remove(args),
    }
}

/// Discover inputs even from invalid or unselected target configurations.
pub(super) fn collect_inputs(config: &Value, root: &Path, inputs: &mut Vec<PathBuf>) {
    if let Some(files) = config["runtimeFiles"].as_object() {
        for asset in files.values() {
            for key in ["input", "template"] {
                if let Some(path) = asset[key].as_str() {
                    inputs.push(root.join(path));
                }
            }
        }
    }
}

fn load(
    selection: &Selection,
) -> Result<(
    project::ThemeProject,
    project::Target,
    PathBuf,
    Vec<PathBuf>,
)> {
    let theme = project::load_theme(&selection.theme)?;
    let target = project::load_target(&theme, &selection.target)?;
    let config = theme
        .root
        .join("targets")
        .join(format!("{}.json", selection.target));
    super::path_text(&config)?;
    let mut protected = super::project_inputs(&theme, Some(&config))?;
    protected.push(super::theme_input(&selection.theme, &theme));
    super::icon::config_preflight(&config, &protected)?;
    Ok((theme, target, config, protected))
}

fn references(mapping: &crpack::Mapping, destination: &str) -> bool {
    mapping.destination == destination
        || (mapping.destination.ends_with('/') && destination.starts_with(&mapping.destination))
}

fn add(args: Add) -> Result<Value> {
    crpack::validate_absolute_source(&args.source)?;
    let (theme, mut target, config, mut protected) = load(&args.selection)?;
    let existing = target
        .runtime_mappings
        .iter()
        .position(|rule| rule.source == args.source);
    let declaration = target
        .runtime_quickapp_icons
        .iter()
        .position(|icon| app_icons::source(&icon.package) == args.source);
    let configured = existing.is_some() || declaration.is_some();
    if !configured
        && (args.source.starts_with(app_icons::QUICKAPP_SOURCE_PREFIX)
            || args.source == app_icons::CANOPUS_SOURCE)
    {
        return Err(Failure::new(
            "reserved_source",
            "use icon commands to author new native application icon sources",
        ));
    }
    if configured && !args.force {
        return Err(Failure::new(
            "mapping_exists",
            "source is already mapped; use --force to update it",
        ));
    }
    let previous_destination = existing
        .map(|index| target.runtime_mappings[index].destination.clone())
        .or_else(|| {
            declaration.map(|index| target.runtime_quickapp_icons[index].destination.clone())
        });
    let destination = args.destination.unwrap_or_else(|| {
        previous_destination
            .clone()
            .unwrap_or_else(|| runtime::destination(&args.source))
    });
    crpack::validate_relative_destination(&destination)?;
    if args.input.is_some() {
        crpack::validate_relative_path(&destination)?;
        if target.runtime_files.contains_key(&destination) {
            let exclusive = (existing
                .is_some_and(|index| target.runtime_mappings[index].destination == destination)
                || declaration.is_some_and(|index| {
                    target.runtime_quickapp_icons[index].destination == destination
                }))
                && !target
                    .runtime_mappings
                    .iter()
                    .enumerate()
                    .any(|(index, rule)| Some(index) != existing && references(rule, &destination))
                && !target
                    .runtime_quickapp_icons
                    .iter()
                    .enumerate()
                    .any(|(index, icon)| {
                        Some(index) != declaration && icon.destination == destination
                    });
            if !args.force || !exclusive {
                return Err(Failure::new(
                    "destination_exists",
                    "runtime destination already exists; omit --input to add an alias, or choose a new destination",
                ));
            }
        }
        // A runtime input must never replace a firmware or native icon declaration.
        if non_runtime_files(&theme, &target)?.contains_key(&destination) {
            return Err(Failure::new(
                "destination_exists",
                "destination belongs to a firmware resource or native icon",
            ));
        }
    } else if !target.runtime_files.keys().any(|path| {
        path == &destination || (destination.ends_with('/') && path.starts_with(&destination))
    }) {
        return Err(Failure::new(
            "input_required",
            "--input is required for a new runtime destination",
        ));
    }
    let filter = match args.filter.as_deref() {
        Some("nearest") => corona_core::ResizeFilter::Nearest,
        _ => corona_core::ResizeFilter::default(),
    };
    let prepared = if let Some(input) = &args.input {
        super::path_text(input)?;
        let bytes = project::read_limited(input, project::MAX_TEMPLATE_BYTES)?;
        let png = !args.raw
            && (bytes.starts_with(b"\x89PNG\r\n\x1a\n")
                || input
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("png"))
                || args.template.is_some()
                || args.allow_quantize
                || args.filter.is_some());
        let template = if let Some(path) = &args.template {
            super::path_text(path)?;
            Some(project::read_limited(path, project::MAX_TEMPLATE_BYTES)?)
        } else if png {
            inherited_template(
                &theme,
                previous_destination
                    .as_ref()
                    .and_then(|path| target.runtime_files.get(path)),
            )?
        } else {
            // Raw replacements never inherit conversion templates.
            None
        };
        let encoded = runtime::encode(
            &bytes,
            template.as_deref(),
            !png,
            args.allow_quantize,
            filter,
        )?;
        if encoded.bytes.len() > project::MAX_TEMPLATE_BYTES {
            return Err(Failure::new(
                "asset_limit",
                "encoded runtime file exceeds 64 MiB",
            ));
        }
        protected.push(input.clone());
        if let Some(path) = &args.template {
            protected.push(path.clone());
        }
        super::icon::config_preflight(&config, &protected)?;
        // Placeholder paths are replaced only after protocol validation succeeds.
        target.runtime_files.insert(
            destination.clone(),
            runtime::RuntimeAsset {
                input: PathBuf::from(
                    "assets/runtime-files/mapping-000000000000000000000000/input.png",
                ),
                mode: if png {
                    project::AssetMode::Png
                } else {
                    project::AssetMode::Raw
                },
                template: template.as_ref().map(|_| {
                    PathBuf::from(
                        "assets/runtime-files/mapping-000000000000000000000000/template.bin",
                    )
                }),
                allow_quantize: args.allow_quantize,
                filter,
            },
        );
        Some((bytes, template, png))
    } else {
        None
    };
    let rule = crpack::Mapping {
        source: args.source.clone(),
        destination: destination.clone(),
    };
    if let Some(index) = declaration {
        target.runtime_quickapp_icons[index].destination = destination.clone();
    } else if let Some(index) = existing {
        target.runtime_mappings[index] = rule;
    } else {
        target.runtime_mappings.push(rule);
    }
    protocol_preflight(&theme, &target)?;
    // Bound and prepare the complete proposed target before creating any
    // project-owned assets. Temporary validation copies live outside the project.
    config_bytes(&config, &target)?;
    actual_preflight(
        &theme,
        &args.selection.target,
        &target,
        &destination,
        prepared.as_ref(),
    )?;
    let mut staged_assets = None;
    if let Some((bytes, template, png)) = prepared {
        let directory = theme.root.join("assets/runtime-files");
        super::icon::owned_directory(&theme.root, &directory)?;
        super::make_dir(&directory)?;
        let staged = tempfile::Builder::new()
            .prefix("mapping-")
            .tempdir_in(&directory)
            .map_err(|e| super::io_failure("could not stage runtime assets", &directory, e))?;
        let input = staged
            .path()
            .join(if png { "input.png" } else { "input.bin" });
        fs::write(&input, bytes)
            .map_err(|e| super::io_failure("could not stage runtime input", &input, e))?;
        let asset = target.runtime_files.get_mut(&destination).unwrap();
        asset.input = PathBuf::from(super::portable_relative(&input, &theme.root)?);
        if let Some(bytes) = template {
            let path = staged.path().join("template.bin");
            fs::write(&path, bytes)
                .map_err(|e| super::io_failure("could not stage runtime template", &path, e))?;
            asset.template = Some(PathBuf::from(super::portable_relative(&path, &theme.root)?));
        }
        staged_assets = Some(staged);
    }
    let bytes = config_bytes(&config, &target)?;
    let staged_config = super::stage(&config, &bytes)?;
    super::icon::config_preflight(&config, &protected)?;
    // Assets are already present, but remain RAII-owned until the config commits.
    super::commit(staged_config, &config, true)?;
    let copied = staged_assets.map(|directory| directory.keep());
    Ok(
        json!({"theme":super::theme_input(&args.selection.theme, &theme), "target":args.selection.target,
        "config":config, "source":args.source, "destination":destination,
        "asset":target.runtime_files.get(&destination), "updated":configured, "copiedAssets":copied}),
    )
}

type PreparedAsset = (Vec<u8>, Option<Vec<u8>>, bool);

fn actual_preflight(
    theme: &project::ThemeProject,
    id: &str,
    target: &project::Target,
    destination: &str,
    input: Option<&PreparedAsset>,
) -> Result<()> {
    let mut proposed = target.clone();
    let staging = if let Some((bytes, template, _)) = input {
        let staging = tempfile::tempdir().map_err(|e| {
            Failure::new(
                "io",
                format!("could not stage runtime validation inputs: {e}"),
            )
        })?;
        let input_path = staging.path().join("input");
        fs::write(&input_path, bytes).map_err(|e| {
            super::io_failure("could not stage runtime validation input", &input_path, e)
        })?;
        let asset = proposed.runtime_files.get_mut(destination).unwrap();
        asset.input = input_path;
        asset.template = if let Some(bytes) = template {
            let path = staging.path().join("template.bin");
            fs::write(&path, bytes).map_err(|e| {
                super::io_failure("could not stage runtime validation template", &path, e)
            })?;
            Some(path)
        } else {
            None
        };
        Some(staging)
    } else {
        None
    };
    let prepared = project::prepare_target_config(theme, id, &proposed);
    // Keep the externally staged files alive through all reads/conversions.
    drop(staging);
    if !prepared.report.valid || prepared.pack.is_none() {
        return Err(Failure {
            code: "validation",
            message: "proposed target failed validation; project is unchanged".into(),
            details: json!({"targets":[prepared.report]}),
        });
    }
    Ok(())
}

/// Re-edits keep their existing PNG layout. An imported raw native BIN becomes
/// an immutable template only after full codec validation; opaque raw data does
/// not impose a layout on its PNG replacement.
fn inherited_template(
    theme: &project::ThemeProject,
    previous: Option<&runtime::RuntimeAsset>,
) -> Result<Option<Vec<u8>>> {
    let Some(previous) = previous else {
        return Ok(None);
    };
    if previous.mode == project::AssetMode::Png {
        return previous
            .template
            .as_ref()
            .map(|path| {
                project::read_limited(&theme.root.join(path), project::MAX_TEMPLATE_BYTES)
                    .map_err(Failure::from)
            })
            .transpose();
    }
    let bytes = project::read_limited(
        &theme.root.join(&previous.input),
        project::MAX_TEMPLATE_BYTES,
    )
    .ok();
    Ok(bytes.filter(|bytes| app_icons::inspect_bin(bytes).is_ok()))
}

fn remove(args: Remove) -> Result<Value> {
    crpack::validate_absolute_source(&args.source)?;
    let (theme, mut target, config, protected) = load(&args.selection)?;
    let destination = if let Some(index) = target
        .runtime_mappings
        .iter()
        .position(|rule| rule.source == args.source)
    {
        target.runtime_mappings.remove(index).destination
    } else if let Some(index) = target
        .runtime_quickapp_icons
        .iter()
        .position(|icon| app_icons::source(&icon.package) == args.source)
    {
        target.runtime_quickapp_icons.remove(index).destination
    } else {
        return Err(Failure::new("mapping_missing", "source is not configured"));
    };
    // Retain file declarations and physical inputs: aliases and directory rules
    // may still use them, and unreferenced imported files are intentional.
    let bytes = config_bytes(&config, &target)?;
    let staged = super::stage(&config, &bytes)?;
    super::icon::config_preflight(&config, &protected)?;
    super::commit(staged, &config, true)?;
    Ok(
        json!({"theme":super::theme_input(&args.selection.theme, &theme), "target":args.selection.target,
        "config":config, "source":args.source, "destination":destination, "removed":true}),
    )
}

fn config_bytes(config: &Path, target: &project::Target) -> Result<Vec<u8>> {
    // Modify only the runtime fields; preserve fingerprint, bindings and metadata.
    let mut value = super::read_json(config)?;
    value["runtimeFiles"] = serde_json::to_value(&target.runtime_files)
        .map_err(|e| Failure::new("json", e.to_string()))?;
    value["runtimeMappings"] = serde_json::to_value(&target.runtime_mappings)
        .map_err(|e| Failure::new("json", e.to_string()))?;
    value["runtimeQuickappIcons"] = serde_json::to_value(&target.runtime_quickapp_icons)
        .map_err(|e| Failure::new("json", e.to_string()))?;
    let bytes = super::json_bytes(&value)?;
    if bytes.len() > 1024 * 1024 {
        return Err(Failure::new(
            "config_size",
            "updated target config exceeds the 1 MiB limit",
        ));
    }
    Ok(bytes)
}

fn firmware_files(target: &project::Target) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    for binding in target.bindings.values() {
        match binding {
            project::Binding::One(path) => {
                files.insert(path.clone(), Vec::new());
            }
            project::Binding::Many(paths) => {
                for path in paths {
                    files.insert(path.clone(), Vec::new());
                }
            }
        }
    }
    files
}

fn non_runtime_files(
    theme: &project::ThemeProject,
    target: &project::Target,
) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut files = firmware_files(target);
    if target.canopus_icon.is_some() || theme.theme.canopus_icon.is_some() {
        files.insert(app_icons::CANOPUS_DESTINATION.into(), Vec::new());
    }
    for package in theme
        .theme
        .quickapp_icons
        .keys()
        .chain(target.quickapp_icons.keys())
    {
        files.insert(app_icons::destination(package), Vec::new());
    }
    Ok(files)
}

/// Exercise the actual CRPack protocol validation with zero-byte placeholders.
/// No firmware is loaded and this does not certify source/device compatibility.
fn protocol_preflight(theme: &project::ThemeProject, target: &project::Target) -> Result<()> {
    let mut files = non_runtime_files(theme, target)?;
    let firmware_files = firmware_files(target);
    for destination in target.runtime_files.keys() {
        files.insert(destination.clone(), Vec::new());
    }
    // Adding a runtime rule activates core's exact firmware exceptions. Grouped
    // rules could collide with an imported directory rule or map its siblings.
    let mut mappings: Vec<_> = firmware_files
        .keys()
        .map(|path| crpack::Mapping {
            source: format!("/resource/{path}"),
            destination: path.clone(),
        })
        .collect();
    if target.canopus_icon.is_some() || theme.theme.canopus_icon.is_some() {
        mappings.push(crpack::Mapping {
            source: app_icons::CANOPUS_SOURCE.into(),
            destination: app_icons::CANOPUS_DESTINATION.into(),
        });
    }
    mappings.extend_from_slice(&target.runtime_mappings);
    let packages: std::collections::BTreeSet<_> = theme
        .theme
        .quickapp_icons
        .keys()
        .chain(target.quickapp_icons.keys())
        .collect();
    let mut icons: Vec<_> = packages
        .into_iter()
        .map(|package| crpack::QuickappIcon {
            package: package.clone(),
            destination: app_icons::destination(package),
        })
        .collect();
    icons.extend_from_slice(&target.runtime_quickapp_icons);
    crpack::build_crpack_with_mappings(
        &crpack::PackOptions {
            theme_id: &theme.theme.theme_id,
            name: &theme.theme.name,
            version: theme.theme.version.as_deref(),
            version_code: theme.theme.version_code,
            author: theme.theme.author.as_deref(),
            description: theme.theme.description.as_deref(),
            target: target.device.as_deref(),
            replacements: &files,
        },
        &mappings,
        &icons,
    )?;
    Ok(())
}
