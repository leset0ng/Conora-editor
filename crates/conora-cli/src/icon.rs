//! Native filesystem icon management; assets are copied into project ownership.
use std::fs;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use conora_core::project;
use serde_json::{Value, json};

use super::{Failure, Result};

#[derive(Subcommand)]
pub(super) enum IconCommand {
    /// Copy an icon (and PNG encoding template) into the theme project.
    Set(Set),
    /// List configured native icons, without loading firmware.
    Ls(List),
    /// Remove an icon declaration, retaining copied assets.
    Remove(Remove),
}

#[derive(Args)]
#[group(skip)]
#[command(group(clap::ArgGroup::new("icon_selector").required(true).multiple(false).args(["canopus", "package"])))]
struct Selector {
    /// Canopus manager icon at /data/canopus/manager_icon.bin.
    #[arg(long)]
    canopus: bool,
    /// Exact QuickApp package identifier (not a filesystem path).
    #[arg(long)]
    package: Option<String>,
}

#[derive(Args)]
pub(super) struct Set {
    #[command(flatten)]
    selector: Selector,
    /// Source PNG, or native BIN when --raw is supplied.
    input: PathBuf,
    #[arg(long, default_value = ".")]
    theme: PathBuf,
    /// Copy bytes unchanged; cannot be combined with --template.
    #[arg(long, conflicts_with = "template")]
    raw: bool,
    /// Original native icon encoding template (required for PNG conversion).
    #[arg(long)]
    template: Option<PathBuf>,
    /// Explicitly permit palette quantization during PNG conversion.
    #[arg(long, conflicts_with = "raw")]
    allow_quantize: bool,
}

#[derive(Args)]
pub(super) struct List {
    #[arg(long, default_value = ".")]
    theme: PathBuf,
}

#[derive(Args)]
pub(super) struct Remove {
    #[command(flatten)]
    selector: Selector,
    #[arg(long, default_value = ".")]
    theme: PathBuf,
}

impl IconCommand {
    pub(super) fn name(&self) -> &'static str {
        match self {
            Self::Set(_) => "icon set",
            Self::Ls(_) => "icon ls",
            Self::Remove(_) => "icon remove",
        }
    }
}

// The theme file is the only replaceable input. Symlink/hardlink aliases to
// firmware, assets or other config files remain protected even for this write.
fn config_preflight(path: &Path, protected: &[PathBuf]) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| super::io_failure("could not inspect theme config", path, e))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(Failure::new(
            "protected_input",
            "theme config must be a regular file, not a symlink",
        ));
    }
    super::preflight(path, true, protected)
}

fn owned_directory(root: &Path, directory: &Path) -> Result<()> {
    // Never follow a redirected assets subtree, even when it points back into
    // the project. Fresh filenames are published with no-clobber semantics.
    let relative = directory
        .strip_prefix(root)
        .map_err(|_| Failure::new("path", "icon asset directory escaped project"))?;
    let mut path = root.to_path_buf();
    for part in relative.components() {
        path.push(part);
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
            Ok(_) => {
                return Err(Failure::new(
                    "protected_input",
                    format!(
                        "icon asset directory must not be redirected: {}",
                        path.display()
                    ),
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(super::io_failure(
                    "could not inspect icon asset directory",
                    &path,
                    e,
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn run(command: IconCommand) -> Result<Value> {
    match command {
        IconCommand::Ls(args) => {
            let theme = project::load_theme(&args.theme)?;
            let value = serde_json::to_value(&theme.theme)
                .map_err(|e| Failure::new("json", e.to_string()))?;
            let mut entries = Vec::new();
            if value["canopusIcon"].is_object() {
                entries.push(json!({"kind":"canopus", "source":conora_core::app_icons::CANOPUS_SOURCE,
                    "destination":conora_core::app_icons::CANOPUS_DESTINATION, "asset":value["canopusIcon"]}));
            }
            if let Some(icons) = value["quickappIcons"].as_object() {
                for (package, asset) in icons {
                    entries.push(json!({"kind":"quickapp", "package":package,
                        "destination":conora_core::app_icons::destination(package), "asset":asset}));
                }
            }
            Ok(json!({"theme":super::theme_input(&args.theme, &theme), "icons":entries}))
        }
        IconCommand::Set(args) => mutate(
            &args.theme,
            args.selector,
            Some((args.input, args.raw, args.template, args.allow_quantize)),
        ),
        IconCommand::Remove(args) => mutate(&args.theme, args.selector, None),
    }
}

fn mutate(
    theme_argument: &Path,
    selector: Selector,
    set: Option<(PathBuf, bool, Option<PathBuf>, bool)>,
) -> Result<Value> {
    if let Some(package) = &selector.package {
        conora_core::app_icons::validate_package(package)?;
    }
    let theme = project::load_theme(theme_argument)?;
    // Keep the caller's lexical file path: load_theme canonicalizes it, which
    // must not hide a redirected theme.json when --theme names a directory.
    let config = if theme_argument.is_dir() {
        theme_argument.join("theme.json")
    } else {
        theme_argument.to_path_buf()
    };
    super::path_text(&config)?;
    let mut protected = super::project_inputs(&theme, None)?;
    // Exempt only project_inputs' initial config entry. Later entries may
    // deliberately reference that exact path as an asset/template/firmware;
    // those uses must remain protected, including identical paths (not just aliases).
    let canonical_config = super::absolute(&config)?;
    if protected.first() == Some(&canonical_config) {
        protected.remove(0);
    }
    config_preflight(&config, &protected)?;
    if set.is_none() {
        refuse_target_overrides(&theme, &selector)?;
    }
    let mut value = super::read_json(&config)?;
    let field = if selector.canopus {
        "canopusIcon"
    } else {
        "quickappIcons"
    };
    let package = selector.package.as_deref();
    let mut published = Vec::new();
    let asset = if let Some((input, raw, template_path, allow_quantize)) = set {
        super::path_text(&input)?;
        if let Some(path) = &template_path {
            super::path_text(path)?;
        }
        if !raw && !selector.canopus && template_path.is_none() {
            return Err(Failure::new(
                "template_required",
                "QuickApp PNG conversion requires --template original.bin",
            ));
        }
        let bytes = project::read_limited(&input, project::MAX_TEMPLATE_BYTES)?;
        let template = template_path
            .as_ref()
            .map(|path| project::read_limited(path, project::MAX_TEMPLATE_BYTES))
            .transpose()?;
        // Validate the complete conversion before any project mutation. The core
        // encoder bounds image dimensions and rejects malformed native raw BINs.
        conora_core::app_icons::encode(
            &bytes,
            template.as_deref(),
            selector.canopus,
            raw,
            allow_quantize,
        )?;
        protected.push(input);
        if let Some(path) = template_path {
            protected.push(path);
        }
        config_preflight(&config, &protected)?;
        let directory = theme.root.join("assets/native-icons");
        owned_directory(&theme.root, &directory)?;
        super::make_dir(&directory)?;
        let staged = tempfile::Builder::new()
            .prefix("icon-")
            .tempdir_in(&directory)
            .map_err(|e| super::io_failure("could not stage icon assets", &directory, e))?;
        let asset_dir = staged.path();
        let input_path = asset_dir.join(if raw { "input.bin" } else { "input.png" });
        fs::write(&input_path, bytes)
            .map_err(|e| super::io_failure("could not stage icon input", &input_path, e))?;
        let mut asset = json!({"input":super::portable_relative(&input_path, &theme.root)?,
            "mode":if raw {"raw"} else {"png"}, "allowQuantize":allow_quantize});
        if let Some(bytes) = template {
            let path = asset_dir.join("template.bin");
            fs::write(&path, bytes)
                .map_err(|e| super::io_failure("could not stage icon template", &path, e))?;
            asset["template"] = json!(super::portable_relative(&path, &theme.root)?);
        }
        // Asset names are fresh and owned; publish before referencing them from
        // the atomic config write. A failed config write may leave unbound assets.
        let kept = staged.keep();
        published.push(kept);
        Some(asset)
    } else {
        None
    };
    let removed = if let Some(package) = package {
        if !value[field].is_object() {
            value[field] = json!({});
        }
        let icons = value[field].as_object_mut().unwrap();
        if let Some(asset) = &asset {
            icons.insert(package.to_owned(), asset.clone());
            false
        } else {
            icons.remove(package).is_some()
        }
    } else if let Some(asset) = &asset {
        value[field] = asset.clone();
        false
    } else {
        value.as_object_mut().unwrap().remove(field).is_some()
    };
    if asset.is_none() && !removed {
        return Err(Failure::new(
            "icon_missing",
            "selected icon is not configured",
        ));
    }
    let bytes = super::json_bytes(&value)?;
    if bytes.len() > 1024 * 1024 {
        return Err(Failure::new(
            "config_size",
            "updated theme config exceeds the 1 MiB limit",
        ));
    }
    let file = super::stage(&config, &bytes)?;
    config_preflight(&config, &protected)?;
    super::commit(file, &config, true)?;
    Ok(
        json!({"theme":config, "kind":if selector.canopus {"canopus"} else {"quickapp"},
        "package":package, "asset":asset, "removed":removed, "copiedAssets":published}),
    )
}

/// Removing a shared declaration must not orphan target-specific overrides.
/// Inspect every target, not just a selected firmware, and leave all files alone.
fn refuse_target_overrides(theme: &project::ThemeProject, selector: &Selector) -> Result<()> {
    let directory = theme.root.join("targets");
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(super::io_failure(
                "could not inspect target overrides",
                &directory,
                error,
            ));
        }
    };
    let mut configs = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|e| super::io_failure("could not inspect target overrides", &directory, e))?
            .path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("json") {
            configs.push(path);
        }
    }
    configs.sort();
    let mut overrides = Vec::new();
    let mut names = Vec::new();
    for path in configs {
        // Do not guess whether an unreadable/malformed target has an override.
        let value = super::read_json(&path)?;
        let overridden = if selector.canopus {
            value["canopusIcon"].is_object()
        } else {
            value["quickappIcons"]
                .get(selector.package.as_deref().unwrap())
                .is_some()
        };
        if overridden {
            let name = path
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or("unknown")
                .to_owned();
            overrides.push(json!({"target":name, "config":super::path_text(&path)?}));
            names.push(name);
        }
    }
    if overrides.is_empty() {
        return Ok(());
    }
    let mut failure = Failure::new(
        "icon_overridden",
        format!(
            "cannot remove shared icon while target overrides exist in: {}; remove those overrides explicitly first",
            names.join(", ")
        ),
    );
    failure.details = json!({"targetOverrides":overrides});
    Err(failure)
}

pub(super) fn collect_inputs(value: &Value, root: &Path, inputs: &mut Vec<PathBuf>) {
    let mut assets = Vec::new();
    if value["canopusIcon"].is_object() {
        assets.push(&value["canopusIcon"]);
    }
    if let Some(icons) = value["quickappIcons"].as_object() {
        assets.extend(icons.values());
    }
    for asset in assets {
        for field in ["input", "template"] {
            if let Some(path) = asset[field].as_str() {
                inputs.push(root.join(path));
            }
        }
    }
}

pub(super) fn archive_path(identity: &str) -> String {
    if identity == conora_core::app_icons::CANOPUS_SOURCE {
        conora_core::app_icons::CANOPUS_DESTINATION.into()
    } else if let Some(package) = identity.strip_prefix("@quickapp-icon/") {
        conora_core::app_icons::destination(package)
    } else {
        identity.into()
    }
}

pub(super) fn asset(
    theme: &project::ThemeProject,
    target: &project::Target,
    identity: &str,
) -> Result<Option<Value>> {
    if identity != conora_core::app_icons::CANOPUS_SOURCE
        && !identity.starts_with("@quickapp-icon/")
    {
        return Ok(None);
    }
    let shared =
        serde_json::to_value(&theme.theme).map_err(|e| Failure::new("json", e.to_string()))?;
    let overrides =
        serde_json::to_value(target).map_err(|e| Failure::new("json", e.to_string()))?;
    let get = |value: &Value| -> Value {
        if identity == conora_core::app_icons::CANOPUS_SOURCE {
            value["canopusIcon"].clone()
        } else {
            value["quickappIcons"][identity.strip_prefix("@quickapp-icon/").unwrap()].clone()
        }
    };
    let override_asset = get(&overrides);
    Ok(Some(if override_asset.is_object() {
        override_asset
    } else {
        get(&shared)
    }))
}
