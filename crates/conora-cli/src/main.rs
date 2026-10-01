use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand, ValueEnum, error::ErrorKind};
use conora_core::{crpack, lvgl, project};
use serde_json::{Value, json};
use tempfile::NamedTempFile;

#[derive(Parser)]
#[command(
    name = "conora",
    version,
    about = "Build icon themes for watch firmware"
)]
struct Cli {
    /// Emit a schemaVersion 1 JSON result instead of human-readable output.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a theme project without guessing resource bindings.
    Init(Init),
    /// Manage firmware targets.
    Target {
        #[command(subcommand)]
        command: TargetCommand,
    },
    /// List resources in a firmware.
    Ls(List),
    /// Extract a firmware resource as raw bytes or PNG.
    Extract(Extract),
    /// Validate all bindings and prepare packs without writing them.
    Check(Selection),
    /// Validate and build CRPack files into a directory.
    Build(Build),
    /// Strictly inspect a CRPack archive.
    Inspect { pack: PathBuf },
}

#[derive(Args)]
struct Init {
    dir: PathBuf,
    #[arg(long, default_value = "conora")]
    theme_id: String,
    #[arg(long, default_value = "Conora Theme")]
    name: String,
    #[arg(long)]
    firmware: Option<PathBuf>,
    #[arg(long, default_value = "default", requires = "firmware")]
    target: String,
    #[arg(long, requires = "firmware")]
    device: Option<String>,
}

#[derive(Subcommand)]
enum TargetCommand {
    /// Add a firmware target, leaving bindings empty.
    Add(TargetAdd),
}

#[derive(Args)]
struct TargetAdd {
    id: String,
    #[arg(long, default_value = ".")]
    theme: PathBuf,
    #[arg(long)]
    firmware: PathBuf,
    #[arg(long)]
    device: Option<String>,
    /// Explicitly replace an existing target configuration.
    #[arg(long)]
    force: bool,
}

#[derive(Args)]
#[group(skip)]
#[command(group(clap::ArgGroup::new("firmware_source").required(true).multiple(false).args(["firmware", "theme"])))]
struct FirmwareSelector {
    #[arg(long, conflicts_with = "target")]
    firmware: Option<PathBuf>,
    #[arg(long, requires = "target")]
    theme: Option<PathBuf>,
    #[arg(long, requires = "theme")]
    target: Option<String>,
}

#[derive(Args)]
struct List {
    #[command(flatten)]
    source: FirmwareSelector,
    /// Show only recognized images.
    #[arg(long)]
    images: bool,
    /// Filter resource paths by a case-sensitive substring.
    #[arg(long)]
    search: Option<String>,
}

#[derive(Clone, Copy, ValueEnum)]
enum ExtractFormat {
    Raw,
    Png,
}

#[derive(Args)]
struct Extract {
    #[command(flatten)]
    source: FirmwareSelector,
    #[arg(long)]
    resource: String,
    #[arg(long = "as", value_enum, default_value = "raw")]
    format: ExtractFormat,
    #[arg(long)]
    output: PathBuf,
    #[arg(long)]
    force: bool,
}

#[derive(Args)]
#[group(skip)]
#[command(group(clap::ArgGroup::new("target_selection").required(true).multiple(false).args(["target", "all_targets"])))]
struct Selection {
    #[arg(long, default_value = ".")]
    theme: PathBuf,
    #[arg(long)]
    target: Option<String>,
    #[arg(long)]
    all_targets: bool,
}

#[derive(Args)]
struct Build {
    #[command(flatten)]
    selection: Selection,
    /// Output directory (default: dist inside the theme project).
    #[arg(long)]
    output: Option<PathBuf>,
    #[arg(long)]
    force: bool,
}

struct Failure {
    code: &'static str,
    message: String,
    details: Value,
}

type Result<T> = std::result::Result<T, Failure>;

impl Failure {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: json!({}),
        }
    }

    fn json(&self, command: &str) -> Value {
        let mut value = self.details.clone();
        value["schemaVersion"] = json!(1);
        value["ok"] = json!(false);
        value["command"] = json!(command);
        value["errors"] = json!([{ "code": self.code, "message": self.message }]);
        value
    }
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self::new("runtime", message)
    }
}

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().collect();
    let wants_json = args.iter().any(|arg| arg == "--json");
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) => {
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) {
                let _ = error.print();
                return ExitCode::SUCCESS;
            }
            if wants_json {
                let value = Failure::new("usage", error.to_string()).json("usage");
                if let Err(error) = print_json(&value) {
                    eprintln!("{error}");
                }
            } else {
                let _ = error.print();
            }
            return ExitCode::from(2);
        }
    };
    let command_name = match &cli.command {
        Command::Init(_) => "init",
        Command::Target { .. } => "target add",
        Command::Ls(_) => "ls",
        Command::Extract(_) => "extract",
        Command::Check(_) => "check",
        Command::Build(_) => "build",
        Command::Inspect { .. } => "inspect",
    };
    match run(cli.command) {
        Ok(mut value) => {
            value["schemaVersion"] = json!(1);
            value["ok"] = json!(true);
            value["command"] = json!(command_name);
            let printed = if cli.json {
                print_json(&value)
            } else {
                print_human(&value)
            };
            if let Err(error) = printed {
                eprintln!("could not write output: {error}");
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(error) => {
            if cli.json {
                if let Err(error) = print_json(&error.json(command_name)) {
                    eprintln!("{error}");
                }
            } else {
                eprintln!("error: {}", error.message);
                print_report_diagnostics(&error.details);
            }
            ExitCode::FAILURE
        }
    }
}

fn print_json(value: &Value) -> io::Result<()> {
    let mut out = io::stdout().lock();
    serde_json::to_writer(&mut out, value).map_err(io::Error::other)?;
    writeln!(out)
}

fn print_human(value: &Value) -> io::Result<()> {
    let mut out = io::stdout().lock();
    if let Some(resources) = value["resources"].as_array() {
        writeln!(
            out,
            "Firmware SHA256: {}",
            value["firmwareSha256"].as_str().unwrap_or("")
        )?;
        for resource in resources {
            write!(
                out,
                "{}\t{} bytes",
                resource["path"].as_str().unwrap_or(""),
                resource["size"]
            )?;
            if resource["image"].is_object() {
                let image = &resource["image"];
                write!(
                    out,
                    "\t{}x{} stride {} {}",
                    image["width"],
                    image["height"],
                    image["stride"],
                    image["format"].as_str().unwrap_or("")
                )?;
            }
            writeln!(out)?;
        }
    } else if let Some(targets) = value["targets"].as_array()
        && matches!(value["command"].as_str(), Some("check" | "build"))
    {
        for report in targets {
            writeln!(
                out,
                "{}: {} ({} pack bytes)",
                report["target"].as_str().unwrap_or(""),
                if report["valid"] == true {
                    "valid"
                } else {
                    "invalid"
                },
                report["packBytes"]
            )?;
        }
        if let Some(outputs) = value["outputs"].as_array() {
            for output in outputs {
                writeln!(out, "Wrote {}", output.as_str().unwrap_or(""))?;
            }
        }
    } else {
        serde_json::to_writer_pretty(&mut out, value).map_err(io::Error::other)?;
        writeln!(out)?;
    }
    drop(out);
    print_report_diagnostics(value);
    Ok(())
}

fn print_report_diagnostics(value: &Value) {
    if let Some(reports) = value["targets"].as_array() {
        for report in reports {
            for kind in ["errors", "warnings"] {
                if let Some(diagnostics) = report[kind].as_array() {
                    for diagnostic in diagnostics {
                        eprintln!(
                            "{}: {}: {}",
                            report["target"].as_str().unwrap_or(""),
                            diagnostic["code"].as_str().unwrap_or(kind),
                            diagnostic["message"].as_str().unwrap_or("")
                        );
                    }
                }
            }
        }
    }
}

fn run(command: Command) -> Result<Value> {
    match command {
        Command::Init(args) => init(args),
        Command::Target {
            command: TargetCommand::Add(args),
        } => target_add(args),
        Command::Ls(args) => list(args),
        Command::Extract(args) => extract(args),
        Command::Check(args) => check_build(args, None),
        Command::Build(args) => check_build(args.selection, Some((args.output, args.force))),
        Command::Inspect { pack } => inspect(&pack),
    }
}

fn io_failure(action: &str, path: &Path, error: impl std::fmt::Display) -> Failure {
    Failure::new("io", format!("{action} {}: {error}", path.display()))
}

fn make_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(|e| io_failure("could not create directory", path, e))
}

fn json_bytes(value: &Value) -> Result<Vec<u8>> {
    let mut bytes =
        serde_json::to_vec_pretty(value).map_err(|e| Failure::new("json", e.to_string()))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn validate_theme_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 12
        || !id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    {
        return Err(Failure::new(
            "theme_id",
            "themeId must be 1-12 lowercase ASCII letters, digits, '_' or '-'",
        ));
    }
    Ok(())
}

fn init(args: Init) -> Result<Value> {
    crpack::validate_pack_metadata(&args.theme_id, &args.name, None, None, None, None)?;
    project::validate_target_id(&args.target)?;
    validate_device(args.device.as_deref())?;
    let root = absolute(&args.dir)?;
    let theme_path = root.join("theme.json");
    path_text(&theme_path)?;
    let mut protected = Vec::new();
    let loaded = if let Some(firmware) = &args.firmware {
        protected.push(absolute(firmware)?);
        Some(project::load_firmware(firmware, None)?)
    } else {
        None
    };
    preflight(&theme_path, false, &protected)?;
    let target_path = root.join("targets").join(format!("{}.json", args.target));
    let target_config = if let (Some(firmware), Some(loaded)) = (&args.firmware, &loaded) {
        path_text(&target_path)?;
        preflight(&target_path, false, &protected)?;
        Some(json_bytes(&target_json(
            firmware,
            target_path.parent().unwrap(),
            &loaded.sha256,
            args.device.as_deref(),
        )?)?)
    } else {
        None
    };
    for dir in [
        &root,
        &root.join("assets"),
        &root.join("targets"),
        &root.join("previews"),
    ] {
        make_dir(dir)?;
    }
    let theme =
        json!({ "schemaVersion": 1, "themeId": args.theme_id, "name": args.name, "icons": {} });
    let theme_staged = stage(&theme_path, &json_bytes(&theme)?)?;
    let target_staged = target_config
        .as_ref()
        .map(|bytes| stage(&target_path, bytes))
        .transpose()?;
    commit(theme_staged, &theme_path, false)?;
    if let Some(staged) = target_staged {
        commit(staged, &target_path, false)?;
    }
    Ok(json!({ "theme": theme_path, "target": loaded.map(|_| args.target) }))
}

fn validate_device(device: Option<&str>) -> Result<()> {
    if let Some(device) = device
        && (device.is_empty() || device.len() > 128 || device.chars().any(char::is_control))
    {
        return Err(Failure::new(
            "device",
            "device must be 1-128 UTF-8 bytes without control characters",
        ));
    }
    Ok(())
}

fn target_json(
    firmware: &Path,
    parent: &Path,
    sha256: &str,
    device: Option<&str>,
) -> Result<Value> {
    let mut value = json!({ "schemaVersion": 1, "firmware": portable_relative(firmware, parent)?, "firmwareSha256": sha256, "bindings": {}, "overrides": {} });
    if let Some(device) = device {
        value["device"] = json!(device);
    }
    Ok(value)
}

fn target_add(args: TargetAdd) -> Result<Value> {
    project::validate_target_id(&args.id)?;
    validate_device(args.device.as_deref())?;
    let project = project::load_theme(&args.theme)?;
    let loaded = project::load_firmware(&args.firmware, None)?;
    let destination = project
        .root
        .join("targets")
        .join(format!("{}.json", args.id));
    path_text(&destination)?;
    let mut protected = project_inputs(&project, Some(&destination))?;
    protected.push(theme_input(&args.theme, &project));
    protected.push(absolute(&args.firmware)?);
    preflight(&destination, args.force, &protected)?;
    let value = target_json(
        &args.firmware,
        destination.parent().unwrap(),
        &loaded.sha256,
        args.device.as_deref(),
    )?;
    make_dir(destination.parent().unwrap())?;
    let staged = stage(&destination, &json_bytes(&value)?)?;
    commit(staged, &destination, args.force)?;
    Ok(json!({ "target": args.id, "config": destination, "firmwareSha256": loaded.sha256 }))
}

struct ResolvedFirmware {
    loaded: project::LoadedFirmware,
    protected: Vec<PathBuf>,
}

fn resolve_firmware(
    selector: &FirmwareSelector,
    protect_outputs: bool,
) -> Result<ResolvedFirmware> {
    if let Some(path) = &selector.firmware {
        Ok(ResolvedFirmware {
            loaded: project::load_firmware(path, None)?,
            protected: if protect_outputs {
                vec![absolute(path)?]
            } else {
                Vec::new()
            },
        })
    } else {
        let theme = selector
            .theme
            .as_ref()
            .ok_or_else(|| Failure::new("usage", "firmware or theme is required"))?;
        let id = selector
            .target
            .as_deref()
            .ok_or_else(|| Failure::new("usage", "target is required with theme"))?;
        project::validate_target_id(id)?;
        let project = project::load_theme(theme)?;
        let target = project::load_target(&project, id)?;
        let firmware = project::target_firmware_path(&project, id, &target);
        let loaded = project::load_firmware(&firmware, Some(&target.firmware_sha256))?;
        let mut protected = Vec::new();
        if protect_outputs {
            protected = project_inputs(&project, None)?;
            protected.push(theme_input(theme, &project));
            protected.push(firmware);
        }
        Ok(ResolvedFirmware { loaded, protected })
    }
}

fn list(args: List) -> Result<Value> {
    let resolved = resolve_firmware(&args.source, false)?;
    let resources: Vec<Value> = resolved.loaded.index.files().iter()
        .filter(|file| !args.images || file.image.is_some())
        .filter(|file| args.search.as_ref().is_none_or(|search| file.path.contains(search)))
        .map(|file| json!({ "path": file.path, "size": file.size, "image": file.image.map(image_json) }))
        .collect();
    Ok(json!({ "firmwareSha256": resolved.loaded.sha256, "resources": resources }))
}

fn image_json(image: lvgl::ImageInfo) -> Value {
    json!({ "width": image.width, "height": image.height, "stride": image.stride, "format": image.format.display_name() })
}

fn validate_extract_resource(
    size: usize,
    recognized_image: bool,
    format: ExtractFormat,
) -> Result<()> {
    if size > project::MAX_TEMPLATE_BYTES {
        return Err(Failure::new(
            "resource_limit",
            "resource exceeds the 64 MiB extraction limit",
        ));
    }
    if matches!(format, ExtractFormat::Png) && !recognized_image {
        return Err(Failure::new(
            "unsupported_image",
            "resource is not a supported image; use --as raw",
        ));
    }
    Ok(())
}

fn extract(args: Extract) -> Result<Value> {
    path_text(&args.output)?;
    let resolved = resolve_firmware(&args.source, true)?;
    preflight(&args.output, args.force, &resolved.protected)?;
    let file = resolved.loaded.index.file(&args.resource).ok_or_else(|| {
        Failure::new(
            "resource_not_found",
            format!("firmware has no resource {}", args.resource),
        )
    })?;
    // Enforce the indexed size before materializing compressed resource bytes.
    validate_extract_resource(file.size, file.image.is_some(), args.format)?;
    let bytes = resolved
        .loaded
        .index
        .file_bytes(&args.resource)?
        .ok_or_else(|| {
            Failure::new(
                "resource_not_found",
                format!("firmware has no resource {}", args.resource),
            )
        })?;
    let (output, format, image) = match args.format {
        ExtractFormat::Raw => (bytes.as_ref().clone(), "raw", None),
        ExtractFormat::Png => {
            let (info, png) = lvgl::decode_image_png(bytes.as_slice())?;
            (png, "png", Some(image_json(info)))
        }
    };
    if let Some(parent) = args.output.parent().filter(|p| !p.as_os_str().is_empty()) {
        make_dir(parent)?;
    }
    let staged = stage(&args.output, &output)?;
    commit(staged, &args.output, args.force)?;
    Ok(
        json!({ "resource": args.resource, "output": args.output, "format": format, "size": output.len(), "image": image, "firmwareSha256": resolved.loaded.sha256 }),
    )
}

fn check_build(selection: Selection, build: Option<(Option<PathBuf>, bool)>) -> Result<Value> {
    let project = project::load_theme(&selection.theme)?;
    let ids = if let Some(id) = selection.target {
        project::validate_target_id(&id)?;
        vec![id]
    } else {
        project::target_ids(&project)?
    };
    if ids.is_empty() {
        return Err(Failure::new("no_targets", "theme has no targets"));
    }
    let mut write_failure = None;
    let outputs = if let Some((output, force)) = build {
        let output = output.unwrap_or_else(|| project.root.join("dist"));
        let theme_id = &project.theme.theme_id;
        validate_theme_id(theme_id)?;
        let mut protected = project_inputs(&project, None)?;
        protected.push(theme_input(&selection.theme, &project));
        let destinations: Vec<PathBuf> = ids
            .iter()
            .map(|id| output.join(format!("{theme_id}-{id}.crpack")))
            .collect();
        // Preflight every destination before staging, while still reporting all targets.
        for destination in &destinations {
            if let Err(failure) = path_text(destination)
                .map(|_| ())
                .and_then(|_| preflight(destination, force, &protected))
                && write_failure.is_none()
            {
                write_failure = Some(failure);
            }
        }
        Some((output, force, destinations))
    } else {
        None
    };
    let mut reports = Vec::new();
    let mut staged = Vec::new();
    let mut invalid = false;
    for (index, id) in ids.iter().enumerate() {
        // Retain only reports and temp files: each target's pack bytes die before
        // preparing the next target, bounding pack memory independently of target count.
        let prepared = project::prepare_target(&project, id);
        reports.push(
            serde_json::to_value(&prepared.report)
                .map_err(|e| Failure::new("json", e.to_string()))?,
        );
        invalid |= !prepared.report.valid || prepared.pack.is_none();
        if let Some((output, _, destinations)) = &outputs
            && !invalid
            && write_failure.is_none()
        {
            let destination = &destinations[index];
            match make_dir(output).and_then(|_| stage(destination, prepared.pack.as_ref().unwrap()))
            {
                Ok(file) => staged.push((file, destination.clone())),
                Err(failure) => write_failure = Some(failure),
            }
        }
    }
    let details = json!({ "targets": reports });
    if invalid {
        return Err(Failure {
            code: "validation",
            message: "one or more targets failed validation; no outputs published".into(),
            details,
        });
    }
    if let Some(mut failure) = write_failure {
        failure.details = details;
        return Err(failure);
    }
    let Some((_, force, _)) = outputs else {
        return Ok(details);
    };
    let mut published = Vec::new();
    // Each rename is atomic, but an I/O failure can leave a partially committed batch.
    for (file, destination) in staged {
        if let Err(mut failure) = commit(file, &destination, force) {
            failure.details = json!({ "targets": reports, "outputs": published, "partialCommit": !published.is_empty() });
            return Err(failure);
        }
        published.push(destination);
    }
    Ok(json!({ "targets": reports, "outputs": published }))
}

fn inspect(path: &Path) -> Result<Value> {
    path_text(path)?;
    let bytes = project::read_limited(path, 64 * 1024 * 1024)?;
    let pack = crpack::parse_crpack(&bytes)?;
    let manifest: Value = serde_json::from_slice(&pack.manifest_bytes)
        .map_err(|e| Failure::new("manifest", e.to_string()))?;
    let mappings: Vec<Value> = pack
        .mappings
        .iter()
        .map(|mapping| json!({ "source": mapping.source, "destination": mapping.destination }))
        .collect();
    let files: Vec<Value> = pack
        .files()
        .map(|(path, contents)| json!({ "path": path, "size": contents.len() }))
        .collect();
    Ok(json!({
        "pack": path,
        "metadata": { "themeId": pack.theme_id, "name": pack.name, "version": pack.version,
            "author": pack.author, "description": pack.description, "target": pack.target },
        "resourceCount": pack.replacements.len(), "packBytes": bytes.len(),
        "manifest": manifest, "targets": pack.targets, "mappings": mappings, "files": files
    }))
}

fn read_json(path: &Path) -> Result<Value> {
    let bytes = project::read_limited(path, 1024 * 1024)?;
    serde_json::from_slice(&bytes)
        .map_err(|e| Failure::new("config", format!("invalid JSON in {}: {e}", path.display())))
}

fn absolute(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| Failure::new("io", e.to_string()))?
            .join(path)
    };
    // Resolve existing symlinked ancestors before processing any later '..'.
    let mut resolved = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => continue,
            Component::ParentDir => {
                resolved.pop();
                continue;
            }
            _ => resolved.push(component.as_os_str()),
        }
        match fs::canonicalize(&resolved) {
            Ok(canonical) => resolved = canonical,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_failure("could not resolve", path, error)),
        }
    }
    Ok(resolved)
}

fn path_text(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| {
        Failure::new(
            "path",
            format!(
                "reportable/config paths must be valid UTF-8: {}",
                path.display()
            ),
        )
    })
}

fn portable_relative(path: &Path, base: &Path) -> Result<String> {
    let path = absolute(path)?;
    let base = absolute(base)?;
    let path_components: Vec<_> = path.components().collect();
    let base_components: Vec<_> = base.components().collect();
    let common = path_components
        .iter()
        .zip(&base_components)
        .take_while(|(a, b)| a == b)
        .count();
    let relative = if common > 0 {
        let mut relative = PathBuf::new();
        for _ in common..base_components.len() {
            relative.push("..");
        }
        for component in &path_components[common..] {
            relative.push(component.as_os_str());
        }
        if relative.as_os_str().is_empty() {
            relative.push(".");
        }
        relative
    } else {
        path
    };
    let text = path_text(&relative)?;
    #[cfg(windows)]
    let text = text.replace('\\', "/");
    #[cfg(not(windows))]
    let text = text.to_owned();
    Ok(text)
}

fn same_file(a: &Path, b: &Path) -> Result<bool> {
    if absolute(a)? == absolute(b)? {
        return Ok(true);
    }
    // Canonical paths catch symlinks; inode identity also catches hard links on Unix.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(a), Ok(b)) = (fs::metadata(a), fs::metadata(b))
            && a.dev() == b.dev()
            && a.ino() == b.ino()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn preflight(destination: &Path, force: bool, protected: &[PathBuf]) -> Result<()> {
    for input in protected {
        if same_file(destination, input)?
            || (input.is_dir() && absolute(destination)?.starts_with(absolute(input)?))
        {
            return Err(Failure::new(
                "protected_input",
                format!(
                    "refusing to overwrite input {} with output {}",
                    input.display(),
                    destination.display()
                ),
            ));
        }
    }
    match fs::symlink_metadata(destination) {
        Ok(metadata) => {
            if !force {
                return Err(Failure::new(
                    "already_exists",
                    format!(
                        "output already exists: {} (use --force where supported)",
                        destination.display()
                    ),
                ));
            }
            if metadata.is_dir() {
                return Err(Failure::new(
                    "output_directory",
                    format!("output is a directory: {}", destination.display()),
                ));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_failure("could not inspect output", destination, error)),
    }
    Ok(())
}

fn stage(destination: &Path, bytes: &[u8]) -> Result<NamedTempFile> {
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut file = NamedTempFile::new_in(parent)
        .map_err(|e| io_failure("could not stage output", destination, e))?;
    file.write_all(bytes)
        .map_err(|e| io_failure("could not write staged output", destination, e))?;
    file.as_file()
        .sync_all()
        .map_err(|e| io_failure("could not sync staged output", destination, e))?;
    Ok(file)
}

fn commit(file: NamedTempFile, destination: &Path, force: bool) -> Result<()> {
    let result = if force {
        file.persist(destination)
    } else {
        file.persist_noclobber(destination)
    };
    result
        .map(|_| ())
        .map_err(|e| io_failure("could not publish output", destination, e.error))
}

fn collect_tree(path: &Path, inputs: &mut Vec<PathBuf>) -> Result<()> {
    fn visit(
        path: &Path,
        inputs: &mut Vec<PathBuf>,
        seen: &mut std::collections::HashSet<PathBuf>,
    ) -> Result<()> {
        match fs::metadata(path) {
            Ok(metadata) if metadata.is_dir() => {
                if !seen.insert(absolute(path)?) {
                    return Ok(());
                }
                for entry in
                    fs::read_dir(path).map_err(|e| io_failure("could not list inputs", path, e))?
                {
                    let entry = entry.map_err(|e| io_failure("could not list inputs", path, e))?;
                    visit(&entry.path(), inputs, seen)?;
                }
            }
            Ok(_) => {
                inputs.push(path.to_path_buf());
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if fs::symlink_metadata(path).is_ok() {
                    inputs.push(path.to_path_buf());
                }
            }
            Err(error) => return Err(io_failure("could not inspect input", path, error)),
        }
        Ok(())
    }
    visit(path, inputs, &mut std::collections::HashSet::new())
}

fn collect_assets(value: &Value, root: &Path, inputs: &mut Vec<PathBuf>) {
    if let Some(assets) = value.as_object() {
        for asset in assets.values() {
            if let Some(path) = asset.as_str().or_else(|| asset["input"].as_str()) {
                inputs.push(root.join(path));
            }
        }
    }
}

fn theme_input(argument: &Path, project: &project::ThemeProject) -> PathBuf {
    if argument.is_dir() {
        project.root.join("theme.json")
    } else {
        argument.to_path_buf()
    }
}

fn project_inputs(
    project: &project::ThemeProject,
    replace_config: Option<&Path>,
) -> Result<Vec<PathBuf>> {
    let mut inputs = vec![project.root.join("theme.json")];
    collect_tree(&project.root.join("assets"), &mut inputs)?;
    collect_tree(&project.root.join("targets"), &mut inputs)?;
    // A forced target add may replace only its selected configuration, not other inputs.
    if let Some(path) = replace_config {
        inputs.retain(|input| input != path);
    }
    let theme =
        serde_json::to_value(&project.theme).map_err(|e| Failure::new("json", e.to_string()))?;
    collect_assets(&theme["icons"], &project.root, &mut inputs);
    let directory = project.root.join("targets");
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(inputs),
        Err(error) => return Err(io_failure("could not list inputs", &directory, error)),
    };
    // Even unselected targets' firmware and replacement assets remain protected.
    for entry in entries {
        let path = entry
            .map_err(|e| io_failure("could not list inputs", &directory, e))?
            .path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        // Unselected configs need not be valid to use a selected target. Their paths
        // are protected above; discover additional inputs best-effort from raw JSON.
        let Ok(config) = read_json(&path) else {
            continue;
        };
        if let Some(firmware) = config["firmware"].as_str() {
            inputs.push(path.parent().unwrap().join(firmware));
        }
        collect_assets(&config["overrides"], &project.root, &mut inputs);
    }
    Ok(inputs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_selector_groups_are_exclusive_and_required() {
        assert!(Cli::try_parse_from(["conora", "check"]).is_err());
        assert!(Cli::try_parse_from(["conora", "check", "--theme", "."]).is_err());
        assert!(
            Cli::try_parse_from(["conora", "build", "--target", "A", "--all-targets"]).is_err()
        );
        assert!(Cli::try_parse_from(["conora", "check", "--target", "A"]).is_ok());
        assert!(
            Cli::try_parse_from(["conora", "ls", "--theme", ".", "--target", "A", "--json"])
                .is_ok()
        );
        assert!(Cli::try_parse_from(["conora", "ls", "--theme", "."]).is_err());
        assert!(Cli::try_parse_from(["conora", "ls", "--target", "A"]).is_err());
        assert!(
            Cli::try_parse_from([
                "conora",
                "ls",
                "--firmware",
                "f.bin",
                "--theme",
                ".",
                "--target",
                "A"
            ])
            .is_err()
        );
    }

    #[test]
    fn init_has_no_firmware_requirement_unless_target_or_device_is_explicit() {
        assert!(Cli::try_parse_from(["conora", "init", "new-theme"]).is_ok());
        assert!(Cli::try_parse_from(["conora", "init", "new-theme", "--target", "A"]).is_err());
        assert!(Cli::try_parse_from(["conora", "init", "new-theme", "--device", "watch"]).is_err());
    }

    #[test]
    fn output_commit_is_no_clobber_unless_explicitly_forced() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("result.bin");
        fs::write(&output, b"old").unwrap();
        let file = stage(&output, b"new").unwrap_or_else(|e| panic!("{}", e.message));
        assert!(commit(file, &output, false).is_err());
        assert_eq!(fs::read(&output).unwrap(), b"old");
        let file = stage(&output, b"new").unwrap_or_else(|e| panic!("{}", e.message));
        commit(file, &output, true).unwrap_or_else(|e| panic!("{}", e.message));
        assert_eq!(fs::read(&output).unwrap(), b"new");
    }

    #[test]
    fn portable_config_paths_are_relative_to_the_config_directory() {
        let dir = tempfile::tempdir().unwrap();
        let targets = dir.path().join("theme/targets");
        fs::create_dir_all(&targets).unwrap();
        let firmware = dir.path().join("firmware.bin");
        fs::write(&firmware, b"firmware").unwrap();
        assert_eq!(
            portable_relative(&firmware, &targets).unwrap_or_else(|e| panic!("{}", e.message)),
            "../../firmware.bin"
        );
    }

    #[test]
    fn empty_projects_can_collect_inputs_and_create_new_assets() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("theme");
        let args = Init {
            dir: root.clone(),
            theme_id: "conora".into(),
            name: "Conora Theme".into(),
            firmware: None,
            target: "default".into(),
            device: None,
        };
        init(args).unwrap_or_else(|e| panic!("{}", e.message));
        let project = project::load_theme(&root).unwrap();
        let inputs = project_inputs(&project, None).unwrap_or_else(|e| panic!("{}", e.message));
        assert!(preflight(&root.join("theme.json"), true, &inputs).is_err());
        assert!(preflight(&root.join("assets/new.png"), false, &inputs).is_ok());
        fs::write(root.join("assets/existing.png"), b"asset").unwrap();
        let inputs = project_inputs(&project, None).unwrap_or_else(|e| panic!("{}", e.message));
        assert!(preflight(&root.join("assets/existing.png"), true, &inputs).is_err());
    }

    #[test]
    fn malformed_unselected_configs_stay_protected_without_blocking_discovery() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("theme");
        init(Init {
            dir: root.clone(),
            theme_id: "conora".into(),
            name: "Theme".into(),
            firmware: None,
            target: "default".into(),
            device: None,
        })
        .unwrap_or_else(|e| panic!("{}", e.message));
        let bad = root.join("targets/B.json");
        fs::write(&bad, b"{invalid").unwrap();
        let firmware = dir.path().join("unselected.bin");
        let asset = dir.path().join("unselected.png");
        fs::write(&firmware, b"firmware").unwrap();
        fs::write(&asset, b"asset").unwrap();
        fs::write(
            root.join("targets/C.json"),
            serde_json::to_vec(&json!({
                "schemaVersion": 999, "firmware": "../../unselected.bin",
                "overrides": { "role": { "input": "../unselected.png", "mode": "raw" } }
            }))
            .unwrap(),
        )
        .unwrap();
        let project = project::load_theme(&root).unwrap();
        let inputs = project_inputs(&project, None).unwrap_or_else(|e| panic!("{}", e.message));
        for input in [bad, firmware, asset] {
            assert!(preflight(&input, true, &inputs).is_err());
        }
    }

    #[test]
    fn extraction_limits_are_checked_without_allocating_resource_contents() {
        for format in [ExtractFormat::Raw, ExtractFormat::Png] {
            assert!(validate_extract_resource(project::MAX_TEMPLATE_BYTES, true, format).is_ok());
            assert!(
                validate_extract_resource(project::MAX_TEMPLATE_BYTES + 1, true, format).is_err()
            );
            assert!(validate_extract_resource(usize::MAX, true, format).is_err());
        }
        assert!(validate_extract_resource(10, false, ExtractFormat::Raw).is_ok());
        assert!(validate_extract_resource(10, false, ExtractFormat::Png).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn portable_paths_preserve_unix_backslashes_in_filenames() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("targets");
        fs::create_dir(&base).unwrap();
        let firmware = dir.path().join("firm\\ware.bin");
        fs::write(&firmware, b"firmware").unwrap();
        assert_eq!(
            portable_relative(&firmware, &base).unwrap_or_else(|e| panic!("{}", e.message)),
            "../firm\\ware.bin"
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_output_paths_fail_before_creating_files_or_directories() {
        use std::os::unix::ffi::OsStringExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(OsString::from_vec(b"theme-\xff".to_vec()));
        let error = init(Init {
            dir: root.clone(),
            theme_id: "conora".into(),
            name: "Theme".into(),
            firmware: None,
            target: "default".into(),
            device: None,
        })
        .expect_err("non-UTF8 root must fail");
        assert_eq!(error.code, "path");
        assert_eq!(error.json("init")["ok"], false);
        assert!(!root.exists());
        let output = root.join("extract.bin");
        let error = extract(Extract {
            source: FirmwareSelector {
                firmware: Some(dir.path().join("missing.bin")),
                theme: None,
                target: None,
            },
            resource: "test.bin".into(),
            format: ExtractFormat::Raw,
            output: output.clone(),
            force: false,
        })
        .expect_err("non-UTF8 output must fail");
        assert_eq!(error.code, "path");
        assert!(!output.exists());
        assert!(!root.exists());
    }

    #[cfg(unix)]
    #[test]
    fn forced_outputs_cannot_overwrite_symlink_or_hardlink_input_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("firmware.bin");
        let symlink = dir.path().join("alias.bin");
        let hardlink = dir.path().join("hardlink.bin");
        fs::write(&input, b"firmware").unwrap();
        std::os::unix::fs::symlink(&input, &symlink).unwrap();
        fs::hard_link(&input, &hardlink).unwrap();
        assert!(preflight(&symlink, true, std::slice::from_ref(&input)).is_err());
        assert!(preflight(&hardlink, true, std::slice::from_ref(&input)).is_err());
        assert_eq!(fs::read(&input).unwrap(), b"firmware");
    }
}
