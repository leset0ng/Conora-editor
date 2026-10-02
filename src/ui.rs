use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, OnceLock};

use astrobox_ng_wit::astrobox::psys_host_v4::{self as psys_host, ui};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::Value;

use crate::crpack::{self, Mapping, PackOptions, QuickappIcon};
use crate::firmware::{BrowserEntry, FirmwareIndex};
use crate::lvgl;
use conora_core::app_icons;

const MAX_VISIBLE_ENTRIES: usize = 300;
const MAX_SEARCH_RESULTS: usize = 200;
const FILE_THUMBNAIL_SIZE: u32 = 28;
const SAVE_CHUNK_BYTES: usize = 256 * 1024;
const MAX_ICON_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResourceFilter {
    All,
    Images,
    Replaced,
}

#[derive(Clone)]
struct IconAsset {
    bytes: Arc<Vec<u8>>,
    preview_uri: Option<String>,
}

impl IconAsset {
    fn new(bytes: Vec<u8>) -> Self {
        Self::shared(Arc::new(bytes))
    }

    fn shared(bytes: Arc<Vec<u8>>) -> Self {
        let preview_uri = preview_from_bytes(bytes.as_slice()).0;
        Self { bytes, preview_uri }
    }
}

struct QuickappEntry {
    declaration: QuickappIcon,
    template: Option<Arc<Vec<u8>>>,
    asset: Option<IconAsset>,
}

#[derive(Clone)]
struct IconSnapshot {
    name: String,
    destination: String,
    template_info: Option<lvgl::I8Info>,
    size: Option<usize>,
    preview_uri: Option<String>,
}

struct UiState {
    root_element_id: Option<String>,
    firmware_name: String,
    firmware: Option<FirmwareIndex>,
    current_dir: String,
    search_query: String,
    filter_mode: ResourceFilter,
    selected_path: Option<String>,
    preview_uri: Option<String>,
    thumbnail_cache: BTreeMap<String, Option<String>>,
    replacements: BTreeMap<String, Vec<u8>>,
    mappings: Vec<Mapping>,
    imported_paths: BTreeSet<String>,
    canopus_destination: String,
    canopus: Option<IconAsset>,
    quickapps: Vec<QuickappEntry>,
    quickapp_package: String,
    theme_id: String,
    pack_name: String,
    version: String,
    author: String,
    description: String,
    target: String,
    allow_quantize: bool,
    resize_filter: lvgl::ResizeFilter,
    busy: bool,
    status: String,
    error: Option<String>,
}

static UI_STATE: OnceLock<Mutex<UiState>> = OnceLock::new();

impl Default for UiState {
    fn default() -> Self {
        Self {
            root_element_id: None,
            firmware_name: String::new(),
            firmware: None,
            current_dir: String::new(),
            search_query: String::new(),
            filter_mode: ResourceFilter::All,
            selected_path: None,
            preview_uri: None,
            thumbnail_cache: BTreeMap::new(),
            replacements: BTreeMap::new(),
            mappings: Vec::new(),
            imported_paths: BTreeSet::new(),
            canopus_destination: app_icons::CANOPUS_DESTINATION.into(),
            canopus: None,
            quickapps: Vec::new(),
            quickapp_package: String::new(),
            theme_id: "conora".into(),
            pack_name: "Conora Resource Pack".into(),
            version: "1.0.0".into(),
            author: String::new(),
            description: String::new(),
            target: String::new(),
            allow_quantize: false,
            resize_filter: lvgl::ResizeFilter::default(),
            busy: false,
            status: "选择固件浏览资源，或直接编辑第三方应用图标。".into(),
            error: None,
        }
    }
}

fn ui_state() -> &'static Mutex<UiState> {
    UI_STATE.get_or_init(|| Mutex::new(UiState::default()))
}

#[derive(Clone)]
struct UiSnapshot {
    firmware_name: String,
    file_count: usize,
    image_count: usize,
    current_dir: String,
    search_query: String,
    filter_mode: ResourceFilter,
    entries: Vec<BrowserEntry>,
    thumbnail_uris: BTreeMap<String, String>,
    hidden_entries: usize,
    selected_path: Option<String>,
    selected_size: Option<usize>,
    selected_image: Option<lvgl::I8Info>,
    selected_template_image: bool,
    selected_replaced: bool,
    preview_uri: Option<String>,
    replacement_count: usize,
    firmware_replacement_count: usize,
    icon_replacement_count: usize,
    canopus: IconSnapshot,
    quickapps: Vec<IconSnapshot>,
    quickapp_package: String,
    replacement_sizes: BTreeMap<String, usize>,
    theme_id: String,
    pack_name: String,
    version: String,
    author: String,
    description: String,
    target: String,
    allow_quantize: bool,
    resize_filter: lvgl::ResizeFilter,
    busy: bool,
    status: String,
    error: Option<String>,
}

fn snapshot(state: &mut UiState) -> UiSnapshot {
    let (file_count, image_count, entries, hidden_entries) = if let Some(firmware) = &state.firmware
    {
        let total_files = firmware.file_count();
        let total_images = firmware.image_count();
        let is_images_only = state.filter_mode == ResourceFilter::Images;

        let (entries, hidden_entries) = match state.filter_mode {
            ResourceFilter::Replaced => {
                let query = state.search_query.trim().to_lowercase();
                let mut list = Vec::new();
                for (path, bytes) in &state.replacements {
                    if !query.is_empty()
                        && !path.to_lowercase().contains(&query)
                        && !path
                            .rsplit('/')
                            .next()
                            .is_some_and(|name| name.to_lowercase().contains(&query))
                    {
                        continue;
                    }
                    let original = firmware.file(path);
                    let image = lvgl::inspect_i8(bytes.as_slice())
                        .or_else(|| original.and_then(|file| file.image));
                    let name = path.rsplit('/').next().unwrap_or(path.as_str()).to_string();
                    list.push(BrowserEntry {
                        name,
                        path: path.clone(),
                        is_directory: false,
                        size: bytes.len(),
                        image,
                        child_count: 0,
                        has_replacements: true,
                    });
                }
                list.sort_by(|left, right| {
                    left.path.to_lowercase().cmp(&right.path.to_lowercase())
                });
                let hidden = list.len().saturating_sub(MAX_SEARCH_RESULTS);
                (list.into_iter().take(MAX_SEARCH_RESULTS).collect(), hidden)
            }
            ResourceFilter::All | ResourceFilter::Images => {
                if state.search_query.trim().is_empty() {
                    let mut all = firmware.entries_in_dir(&state.current_dir);
                    all.retain_mut(|entry| {
                        if entry.is_directory {
                            entry.child_count = firmware.file_count_in_dir(&entry.path);
                            let prefix = format!("{}/", entry.path);
                            entry.has_replacements =
                                state.replacements.keys().any(|k| k.starts_with(&prefix));
                            if is_images_only {
                                firmware.dir_has_images(&entry.path)
                            } else {
                                true
                            }
                        } else {
                            entry.has_replacements = state.replacements.contains_key(&entry.path);
                            if is_images_only {
                                entry.image.is_some()
                            } else {
                                true
                            }
                        }
                    });
                    let hidden = all.len().saturating_sub(MAX_VISIBLE_ENTRIES);
                    (all.into_iter().take(MAX_VISIBLE_ENTRIES).collect(), hidden)
                } else {
                    let query = state.search_query.trim().to_lowercase();
                    let all = firmware
                        .files()
                        .iter()
                        .filter(|file| {
                            if is_images_only && file.image.is_none() {
                                return false;
                            }
                            file.path.to_lowercase().contains(&query)
                                || file
                                    .path
                                    .rsplit('/')
                                    .next()
                                    .is_some_and(|name| name.to_lowercase().contains(&query))
                        })
                        .map(|file| BrowserEntry {
                            name: file
                                .path
                                .rsplit('/')
                                .next()
                                .unwrap_or(&file.path)
                                .to_string(),
                            path: file.path.clone(),
                            is_directory: false,
                            size: file.size,
                            image: file.image,
                            child_count: 0,
                            has_replacements: state.replacements.contains_key(&file.path),
                        })
                        .collect::<Vec<_>>();
                    let hidden = all.len().saturating_sub(MAX_SEARCH_RESULTS);
                    (all.into_iter().take(MAX_SEARCH_RESULTS).collect(), hidden)
                }
            }
        };

        (total_files, total_images, entries, hidden_entries)
    } else {
        (0, 0, Vec::new(), 0)
    };

    state.thumbnail_cache.retain(|path, _| {
        entries
            .iter()
            .any(|entry| entry.image.is_some() && entry.path.as_str() == path.as_str())
    });
    let missing_firmware_thumbnails = entries
        .iter()
        .filter(|entry| {
            entry.image.is_some()
                && !state.replacements.contains_key(&entry.path)
                && !state.thumbnail_cache.contains_key(&entry.path)
        })
        .map(|entry| entry.path.clone())
        .collect::<Vec<_>>();
    let mut generated_thumbnails = state
        .firmware
        .as_ref()
        .and_then(|firmware| {
            firmware
                .file_thumbnail_pngs(&missing_firmware_thumbnails, FILE_THUMBNAIL_SIZE)
                .ok()
        })
        .unwrap_or_default();
    let mut thumbnail_uris = BTreeMap::new();
    for entry in entries.iter().filter(|entry| entry.image.is_some()) {
        if !state.thumbnail_cache.contains_key(&entry.path) {
            let thumbnail_uri = state
                .replacements
                .get(&entry.path)
                .and_then(|bytes| thumbnail_from_bytes(bytes))
                .or_else(|| {
                    generated_thumbnails
                        .remove(&entry.path)
                        .map(thumbnail_uri_from_png)
                });
            state
                .thumbnail_cache
                .insert(entry.path.clone(), thumbnail_uri);
        }
        if let Some(Some(uri)) = state.thumbnail_cache.get(&entry.path) {
            thumbnail_uris.insert(entry.path.clone(), uri.clone());
        }
    }

    let selected = state
        .selected_path
        .as_deref()
        .and_then(|path| state.firmware.as_ref()?.file(path));
    let selected_size = state.selected_path.as_ref().and_then(|path| {
        state
            .replacements
            .get(path)
            .map(Vec::len)
            .or_else(|| selected.map(|file| file.size))
    });
    let selected_replaced = state
        .selected_path
        .as_ref()
        .is_some_and(|path| state.replacements.contains_key(path));
    let selected_image = state.selected_path.as_ref().and_then(|path| {
        if selected_replaced {
            state
                .replacements
                .get(path)
                .and_then(|bytes| lvgl::inspect_image(bytes))
        } else if let Some(file) = selected {
            if let Some(mut info) = file.image {
                if info.width == 0
                    && info.height == 0
                    && let Ok(Some(bytes)) = state.firmware.as_ref().unwrap().file_bytes(path)
                    && let Some(inspected) = lvgl::inspect_image(bytes.as_slice())
                {
                    info = inspected;
                }
                Some(info)
            } else {
                None
            }
        } else {
            None
        }
    });
    let selected_template_image = selected.is_some_and(|file| file.image.is_some());

    UiSnapshot {
        firmware_name: state.firmware_name.clone(),
        file_count,
        image_count,
        current_dir: state.current_dir.clone(),
        search_query: state.search_query.clone(),
        filter_mode: state.filter_mode,
        entries,
        thumbnail_uris,
        hidden_entries,
        selected_path: state.selected_path.clone(),
        selected_size,
        selected_image,
        selected_template_image,
        selected_replaced,
        preview_uri: state.preview_uri.clone(),
        replacement_count: replacement_count(state),
        firmware_replacement_count: state.replacements.len(),
        icon_replacement_count: icon_replacement_count(state),
        canopus: icon_snapshot(
            "Canopus",
            &state.canopus_destination,
            Some(&app_icons::canopus_template()),
            state.canopus.as_ref(),
        ),
        quickapps: state
            .quickapps
            .iter()
            .map(|entry| {
                icon_snapshot(
                    &entry.declaration.package,
                    &entry.declaration.destination,
                    entry.template.as_ref().map(|bytes| bytes.as_slice()),
                    entry.asset.as_ref(),
                )
            })
            .collect(),
        quickapp_package: state.quickapp_package.clone(),
        replacement_sizes: state
            .replacements
            .iter()
            .map(|(path, bytes)| (path.clone(), bytes.len()))
            .collect(),
        theme_id: state.theme_id.clone(),
        pack_name: state.pack_name.clone(),
        version: state.version.clone(),
        author: state.author.clone(),
        description: state.description.clone(),
        target: state.target.clone(),
        allow_quantize: state.allow_quantize,
        resize_filter: state.resize_filter,
        busy: state.busy,
        status: state.status.clone(),
        error: state.error.clone(),
    }
}

pub async fn ui_event_processor(event: ui::Event, event_id: &str, event_payload: &str) {
    match event {
        ui::Event::Click => process_click(event_id).await,
        ui::Event::Change => process_change(event_id, event_payload),
        ui::Event::KeyDown
            if event_id == "browser.search.key"
                && payload_field(event_payload, "key").as_deref() == Some("Enter") =>
        {
            apply_search();
        }
        _ => {}
    }
}

async fn process_click(event_id: &str) {
    if lock_state().busy {
        return;
    }
    match event_id {
        "firmware.upload" => begin_firmware_pick().await,
        "pack.import" => begin_crpack_pick().await,
        "pack.export" => begin_export().await,
        "resource.extract" => begin_resource_extract(false).await,
        "resource.extract.png" => begin_resource_extract(true).await,
        "browser.apply-search" => apply_search(),
        "browser.clear-search" => {
            {
                let mut state = lock_state();
                state.search_query.clear();
                state.status = "已清空搜索筛选。".into();
                state.error = None;
            }
            render_current();
        }
        "browser.filter:all" => {
            {
                let mut state = lock_state();
                state.filter_mode = ResourceFilter::All;
                state.status = "显示全部资源。".into();
                state.error = None;
            }
            render_current();
        }
        "browser.filter:images" => {
            {
                let mut state = lock_state();
                state.filter_mode = ResourceFilter::Images;
                state.status = "仅显示图片资源。".into();
                state.error = None;
            }
            render_current();
        }
        "browser.filter:replaced" => {
            {
                let mut state = lock_state();
                state.filter_mode = ResourceFilter::Replaced;
                state.status = "显示已替换的资源清单。".into();
                state.error = None;
            }
            render_current();
        }
        "browser.parent" => {
            {
                let mut state = lock_state();
                state.search_query.clear();
                if state.filter_mode == ResourceFilter::Replaced {
                    state.filter_mode = ResourceFilter::All;
                }
                if let Some((parent, _)) = state.current_dir.rsplit_once('/') {
                    state.current_dir = parent.to_string();
                } else {
                    state.current_dir.clear();
                }
                state.status = if state.current_dir.is_empty() {
                    "已返回资源根目录。".into()
                } else {
                    format!("正在浏览 /resource/{}/", state.current_dir)
                };
                state.error = None;
            }
            render_current();
        }
        "icons.canopus.png" => begin_icon_pick(None, IconPick::Png).await,
        "icons.canopus.bin" => begin_icon_pick(None, IconPick::Binary).await,
        "icons.canopus.undo" => {
            let mut state = lock_state();
            state.canopus = None;
            state
                .mappings
                .retain(|mapping| mapping.source != app_icons::CANOPUS_SOURCE);
            state.canopus_destination = app_icons::CANOPUS_DESTINATION.into();
            state.status = "已撤销 Canopus 图标替换。".into();
            state.error = None;
            drop(state);
            render_current();
        }
        "icons.quickapp.add" => add_quickapp(),
        _ if event_id.starts_with("icons.quickapp.") => {
            if let Some((action, package)) = event_id["icons.quickapp.".len()..].split_once(':') {
                match action {
                    "png" => begin_icon_pick(Some(package), IconPick::Png).await,
                    "bin" => begin_icon_pick(Some(package), IconPick::Binary).await,
                    "template" => begin_icon_pick(Some(package), IconPick::Template).await,
                    "remove" => remove_quickapp(package),
                    _ => {}
                }
            }
        }
        "replace.png" => begin_png_pick().await,
        "replace.binary" => begin_binary_pick().await,
        "replace.restore" => restore_selected(),
        "resize.filter:lanczos3" => {
            {
                let mut state = lock_state();
                state.resize_filter = lvgl::ResizeFilter::Lanczos3;
                state.status = "已将缩放采样算法切换为 Lanczos3（平滑抗锯齿）。".into();
                state.error = None;
            }
            render_current();
        }
        "resize.filter:nearest" => {
            {
                let mut state = lock_state();
                state.resize_filter = lvgl::ResizeFilter::Nearest;
                state.status = "已将缩放采样算法切换为 Nearest（最近邻/像素风）。".into();
                state.error = None;
            }
            render_current();
        }
        _ if event_id.starts_with("browser.open:") => {
            let path = &event_id["browser.open:".len()..];
            {
                let mut state = lock_state();
                if path.is_empty()
                    || state
                        .firmware
                        .as_ref()
                        .is_some_and(|firmware| firmware.directory_exists(path))
                {
                    state.current_dir = path.to_string();
                    state.search_query.clear();
                    if state.filter_mode == ResourceFilter::Replaced {
                        state.filter_mode = ResourceFilter::All;
                    }
                    state.selected_path = None;
                    state.preview_uri = None;
                    state.status = if path.is_empty() {
                        "已返回资源根目录。".into()
                    } else {
                        format!("正在浏览 /resource/{path}/")
                    };
                    state.error = None;
                }
            }
            render_current();
        }
        _ if event_id.starts_with("browser.select:") => {
            let path = &event_id["browser.select:".len()..];
            select_file(path);
        }
        _ => {}
    }
}

fn process_change(event_id: &str, payload: &str) {
    if lock_state().busy {
        return;
    }
    let value = payload_field(payload, "value").unwrap_or_default();
    let checked = payload_field(payload, "checked").as_deref() == Some("true");
    let should_render = {
        let mut state = lock_state();
        match event_id {
            "icons.quickapp.package" => {
                state.quickapp_package = value;
                false
            }
            "browser.search" => {
                state.search_query = value;
                false
            }
            "pack.id" => {
                state.theme_id = value;
                false
            }
            "pack.name" => {
                state.pack_name = value;
                false
            }
            "pack.version" => {
                state.version = value;
                false
            }
            "pack.author" => {
                state.author = value;
                false
            }
            "pack.description" => {
                state.description = value;
                false
            }
            "pack.target" => {
                state.target = value;
                false
            }
            "pack.quantize" => {
                state.allow_quantize = checked;
                true
            }
            "pack.smooth_resize" => {
                state.resize_filter = if checked {
                    lvgl::ResizeFilter::Lanczos3
                } else {
                    lvgl::ResizeFilter::Nearest
                };
                true
            }
            _ => return,
        }
    };
    if should_render {
        render_current();
    }
}

fn apply_search() {
    {
        let mut state = lock_state();
        if state.busy {
            return;
        }
        state.status = if state.search_query.trim().is_empty() {
            "显示当前目录。".into()
        } else {
            "已按资源路径筛选。".into()
        };
    }
    render_current();
}

async fn begin_firmware_pick() {
    {
        let mut state = lock_state();
        if state.busy {
            return;
        }
        state.busy = true;
        state.error = None;
        state.status = "等待选择固件文件…".into();
    }
    render_current();

    let picked = match psys_host::dialog::pick_file(
        psys_host::dialog::PickConfig {
            read: true,
            copy_to: None,
        },
        psys_host::dialog::FilterConfig {
            multiple: false,
            extensions: vec!["bin".into(), "zip".into(), "jar".into()],
            default_directory: String::new(),
            default_file_name: String::new(),
        },
    )
    .await
    {
        Ok(picked) => picked,
        Err(error) => {
            {
                let mut state = lock_state();
                state.busy = false;
                state.status = "无法打开固件选择器。".into();
                state.error = Some(format!("选择固件失败：{error}"));
            }
            render_current();
            return;
        }
    };

    if picked.data.is_empty() {
        {
            let mut state = lock_state();
            state.busy = false;
            state.status = "没有读取到固件文件；可以重新选择。".into();
        }
        render_current();
        return;
    }

    let file_name = picked.name.clone();
    {
        let mut state = lock_state();
        state.status = format!("正在流式索引固件资源：{file_name}");
    }
    render_current();
    let result = FirmwareIndex::from_firmware(picked.data);
    match result {
        Ok(firmware) => {
            let count = firmware.file_count();
            let target = infer_target(&file_name);
            {
                let mut state = lock_state();
                state.firmware_name = file_name;
                state.firmware = Some(firmware);
                reset_firmware_resource_edits(&mut state);
                if !target.is_empty() {
                    state.target = target;
                }
                state.busy = false;
                let icon_count = icon_replacement_count(&state);
                state.status = format!(
                    "固件已解包，发现 {count} 个资源文件。原固件替换已重置；保留 {icon_count} 个第三方图标编辑。"
                );
                state.error = None;
            }
        }
        Err(error) => {
            let mut state = lock_state();
            state.busy = false;
            state.status = "固件未能加载。".into();
            state.error = Some(format!("固件解包失败：{error}"));
        }
    }
    render_current();
}

async fn begin_crpack_pick() {
    {
        let mut state = lock_state();
        if state.busy {
            return;
        }
        state.busy = true;
        state.error = None;
        state.status = "等待选择 CRPack 文件…".into();
    }
    render_current();

    let picked = match psys_host::dialog::pick_file(
        psys_host::dialog::PickConfig {
            read: true,
            copy_to: None,
        },
        psys_host::dialog::FilterConfig {
            multiple: false,
            extensions: vec!["crpack".into(), "zip".into()],
            default_directory: String::new(),
            default_file_name: String::new(),
        },
    )
    .await
    {
        Ok(picked) => picked,
        Err(error) => {
            {
                let mut state = lock_state();
                state.busy = false;
                state.status = "无法打开 CRPack 选择器。".into();
                state.error = Some(format!("选择 CRPack 失败：{error}"));
            }
            render_current();
            return;
        }
    };

    if picked.data.is_empty() {
        {
            let mut state = lock_state();
            state.busy = false;
            state.status = "没有读取到 CRPack 文件；可以重新选择。".into();
        }
        render_current();
        return;
    }

    let file_name = picked.name.clone();
    {
        let mut state = lock_state();
        state.status = format!("正在解析 CRPack：{file_name}");
    }
    render_current();

    let result = parse_ui_pack(&picked.data);
    match result {
        Ok(unpacked) => {
            let mut state = lock_state();
            state.theme_id = unpacked.theme_id;
            state.pack_name = unpacked.name;
            state.version = unpacked.version.unwrap_or_default();
            state.author = unpacked.author.unwrap_or_default();
            state.description = unpacked.description.unwrap_or_default();
            if let Some(target) = unpacked.target
                && !target.is_empty()
            {
                state.target = target;
            }

            // Import replaces the project; app assets remain outside the firmware tree.
            import_assets(
                &mut state,
                unpacked.replacements,
                unpacked.mappings,
                unpacked.quickapp_icons,
            );
            state.thumbnail_cache.clear();
            state.filter_mode = ResourceFilter::Replaced;
            state.search_query.clear();
            state.busy = false;

            state.preview_uri = state
                .selected_path
                .as_deref()
                .and_then(|path| resource_bytes(&state, path).ok())
                .and_then(|bytes| preview_from_bytes(bytes.as_slice()).0);
            state.status = import_confirmation(&state);
            state.error = None;
        }
        Err(error) => {
            let mut state = lock_state();
            state.busy = false;
            state.status = "CRPack 未能导入。".into();
            state.error = Some(format!("CRPack 导入失败：{error}；现有编辑保持不变。"));
        }
    }
    render_current();
}

fn parse_ui_pack(bytes: &[u8]) -> Result<crpack::UnpackedCrpack, String> {
    let unpacked = crpack::parse_crpack(bytes)?;
    // Check before touching metadata or assets: unsupported rules cannot be silently lost.
    validate_ui_mappings(&unpacked.mappings)?;
    let declarations = normalized_declarations(&unpacked.mappings, unpacked.quickapp_icons.clone());
    let mut templates = 0usize;
    for icon in &declarations {
        let size = unpacked
            .replacements
            .get(&icon.destination)
            .map_or(0, Vec::len);
        validate_icon_budget(size, templates, 0, true)?;
        templates += size;
    }
    Ok(unpacked)
}

fn validate_ui_mappings(mappings: &[Mapping]) -> Result<(), String> {
    for mapping in mappings {
        if mapping.source.starts_with("/resource/") || mapping.source == app_icons::CANOPUS_SOURCE {
            continue;
        }
        if let Some(package) = mapping
            .source
            .strip_prefix(app_icons::QUICKAPP_SOURCE_PREFIX)
        {
            app_icons::validate_package(package)?;
            continue;
        }
        return Err(format!(
            "此编辑器不支持映射来源 {}。仅支持 /resource/、Canopus 图标和 @quickapp-icon/；不会丢弃或转换该映射。",
            mapping.source,
        ));
    }
    Ok(())
}

fn import_confirmation(state: &UiState) -> String {
    let mut status = format!(
        "已导入「{}」，共 {} 项替换（固件资源 {}，第三方图标 {}）。",
        state.pack_name,
        replacement_count(state),
        state.replacements.len(),
        icon_replacement_count(state),
    );
    if let Some(firmware) = &state.firmware {
        let missing = state
            .replacements
            .keys()
            .filter(|path| firmware.file(path).is_none())
            .count();
        if missing > 0 {
            status.push_str(&format!("其中 {missing} 个固件资源在当前固件中未找到。"));
        }
    }
    status
}

fn icon_snapshot(
    name: &str,
    destination: &str,
    template: Option<&[u8]>,
    asset: Option<&IconAsset>,
) -> IconSnapshot {
    IconSnapshot {
        name: name.into(),
        destination: destination.into(),
        template_info: template.and_then(lvgl::inspect_image),
        size: asset.map(|asset| asset.bytes.len()),
        preview_uri: asset.and_then(|asset| asset.preview_uri.clone()),
    }
}

fn icon_destinations(state: &UiState) -> BTreeSet<&str> {
    let mut paths = BTreeSet::new();
    if state.canopus.is_some() {
        paths.insert(state.canopus_destination.as_str());
    }
    for entry in &state.quickapps {
        if entry.asset.is_some() {
            paths.insert(entry.declaration.destination.as_str());
        }
    }
    paths
}

fn replacement_sizes_by_destination(state: &UiState) -> BTreeMap<&str, usize> {
    let mut sizes = state
        .replacements
        .iter()
        .map(|(path, bytes)| (path.as_str(), bytes.len()))
        .collect::<BTreeMap<_, _>>();
    if let Some(asset) = &state.canopus {
        sizes.insert(&state.canopus_destination, asset.bytes.len());
    }
    for entry in &state.quickapps {
        if let Some(asset) = &entry.asset {
            sizes.insert(&entry.declaration.destination, asset.bytes.len());
        }
    }
    sizes
}

fn icon_replacement_count(state: &UiState) -> usize {
    icon_destinations(state).len()
}
fn replacement_count(state: &UiState) -> usize {
    replacement_sizes_by_destination(state).len()
}
fn replacement_bytes(state: &UiState) -> usize {
    replacement_sizes_by_destination(state).values().sum()
}
fn template_bytes(state: &UiState) -> usize {
    state
        .quickapps
        .iter()
        .filter_map(|entry| entry.template.as_ref())
        .map(|bytes| bytes.len())
        .sum()
}

fn validate_icon_budget(
    size: usize,
    total: usize,
    old_size: usize,
    template: bool,
) -> Result<(), String> {
    if size > MAX_ICON_BYTES {
        return Err("单个图标上传不能超过 64 MiB；现有编辑保持不变。".into());
    }
    if total.saturating_sub(old_size).saturating_add(size) > MAX_ICON_BYTES {
        return Err(if template {
            "原始 BIN 模板总大小不能超过 64 MiB；模板未更改。"
        } else {
            "替换资源总大小不能超过 CRPack v1 的 64 MiB 上限；图标未更改。"
        }
        .into());
    }
    Ok(())
}

// Loading firmware resets only the firmware workspace, never application edits/templates.
fn reset_firmware_resource_edits(state: &mut UiState) {
    state.current_dir.clear();
    state.search_query.clear();
    state.filter_mode = ResourceFilter::All;
    state.selected_path = None;
    state.preview_uri = None;
    state.replacements.clear();
    state.thumbnail_cache.clear();
    state.imported_paths.clear();
    state
        .mappings
        .retain(|mapping| mapping.source == app_icons::CANOPUS_SOURCE);
}

fn normalized_declarations(
    mappings: &[Mapping],
    mut declarations: Vec<QuickappIcon>,
) -> Vec<QuickappIcon> {
    for mapping in mappings {
        if let Some(package) = mapping
            .source
            .strip_prefix(app_icons::QUICKAPP_SOURCE_PREFIX)
            && !declarations.iter().any(|icon| icon.package == package)
        {
            declarations.push(QuickappIcon {
                package: package.into(),
                destination: mapping.destination.clone(),
            });
        }
    }
    declarations
}

fn import_assets(
    state: &mut UiState,
    mut replacements: BTreeMap<String, Vec<u8>>,
    mappings: Vec<Mapping>,
    declarations: Vec<QuickappIcon>,
) {
    state.imported_paths = replacements.keys().cloned().collect();
    let declarations = normalized_declarations(&mappings, declarations);
    let canopus_destination = mappings
        .iter()
        .find(|mapping| mapping.source == app_icons::CANOPUS_SOURCE)
        .map(|mapping| mapping.destination.clone());
    let mut app_paths = declarations
        .iter()
        .map(|icon| icon.destination.clone())
        .collect::<BTreeSet<_>>();
    if let Some(destination) = &canopus_destination {
        app_paths.insert(destination.clone());
    }
    // Reference all shared assets before removing anything. Firmware-backed paths remain in
    // the firmware workspace; app-only paths live exclusively in the application state.
    let assets = app_paths
        .iter()
        .filter_map(|path| {
            replacements
                .get(path)
                .map(|bytes| (path.clone(), Arc::new(bytes.clone())))
        })
        .collect::<BTreeMap<_, _>>();
    state.quickapps = declarations
        .into_iter()
        .map(|declaration| {
            let bytes = assets.get(&declaration.destination).cloned();
            QuickappEntry {
                declaration,
                template: bytes.clone(),
                asset: bytes.map(IconAsset::shared),
            }
        })
        .collect();
    state.canopus = canopus_destination
        .as_ref()
        .and_then(|path| assets.get(path).cloned())
        .map(IconAsset::shared);
    state.canopus_destination =
        canopus_destination.unwrap_or_else(|| app_icons::CANOPUS_DESTINATION.into());
    for path in &app_paths {
        let firmware_backed = mappings.iter().any(|mapping| {
            mapping.source.starts_with("/resource/") && mapping_covers(mapping, path)
        });
        if !firmware_backed {
            replacements.remove(path);
        }
    }
    state.mappings = mappings
        .into_iter()
        .filter(|mapping| {
            !mapping
                .source
                .starts_with(app_icons::QUICKAPP_SOURCE_PREFIX)
        })
        .collect();
    state.replacements = replacements;
    state.quickapp_package.clear();
}

fn mapping_covers(mapping: &Mapping, path: &str) -> bool {
    path == mapping.destination
        || (mapping.destination.ends_with('/') && path.starts_with(&mapping.destination))
}

type ExportAssets = (BTreeMap<String, Vec<u8>>, Vec<Mapping>, Vec<QuickappIcon>);

fn export_assets(state: &UiState) -> Result<ExportAssets, String> {
    validate_ui_mappings(&state.mappings)?;
    let mut replacements = state.replacements.clone();
    let mut declarations = Vec::new();
    let mut insert = |destination: &str, asset: &IconAsset| -> Result<(), String> {
        if let Some(existing) = replacements.get(destination) {
            if existing.as_slice() != asset.bytes.as_slice() {
                return Err(format!("共享图标目标内容不一致：{destination}"));
            }
            return Ok(());
        }
        replacements.insert(destination.into(), asset.bytes.as_ref().clone());
        Ok(())
    };
    if let Some(asset) = &state.canopus {
        insert(&state.canopus_destination, asset)?;
    }
    for entry in &state.quickapps {
        let asset = entry.asset.as_ref().ok_or_else(|| {
            format!(
                "快应用 {} 尚未替换图标；请上传 PNG/BIN 或移除该项。模板不会导出。",
                entry.declaration.package,
            )
        })?;
        insert(&entry.declaration.destination, asset)?;
        declarations.push(entry.declaration.clone());
    }
    let mut mappings = state
        .mappings
        .iter()
        .filter(|mapping| {
            if mapping.source == app_icons::CANOPUS_SOURCE && state.canopus.is_none() {
                return false;
            }
            replacements.keys().any(|path| {
                path == &mapping.destination
                    || (mapping.destination.ends_with('/')
                        && path.starts_with(&mapping.destination))
            })
        })
        .cloned()
        .collect::<Vec<_>>();
    if state.canopus.is_some()
        && !mappings
            .iter()
            .any(|mapping| mapping.source == app_icons::CANOPUS_SOURCE)
    {
        mappings.push(Mapping {
            source: app_icons::CANOPUS_SOURCE.into(),
            destination: state.canopus_destination.clone(),
        });
    }
    let app_paths = icon_destinations(state);
    let new_paths = state.replacements.keys().filter(|path| {
        !state.imported_paths.contains(*path)
            && !app_paths.contains(path.as_str())
            && !mappings.iter().any(|mapping| mapping_covers(mapping, path))
    });
    let inferred = if state.imported_paths.is_empty() {
        crpack::firmware_mappings(new_paths)?
    } else {
        // Grouping new siblings could accidentally activate an authoritative unused file.
        new_paths
            .map(|path| Mapping {
                source: format!("/resource/{path}"),
                destination: path.clone(),
            })
            .collect()
    };
    mappings.extend(inferred);
    Ok((replacements, mappings, declarations))
}

fn build_project_pack(state: &UiState) -> Result<Vec<u8>, String> {
    let (replacements, mappings, declarations) = export_assets(state)?;
    crpack::build_crpack_with_mappings(
        &PackOptions {
            theme_id: &state.theme_id,
            name: &state.pack_name,
            version: (!state.version.is_empty()).then_some(state.version.as_str()),
            author: (!state.author.is_empty()).then_some(state.author.as_str()),
            description: (!state.description.is_empty()).then_some(state.description.as_str()),
            target: (!state.target.is_empty()).then_some(state.target.as_str()),
            replacements: &replacements,
        },
        &mappings,
        &declarations,
    )
}

fn insert_quickapp(state: &mut UiState) -> Result<(), String> {
    let package = state.quickapp_package.clone();
    app_icons::validate_package(&package)?;
    if state
        .quickapps
        .iter()
        .any(|entry| entry.declaration.package == package)
    {
        return Err("此快应用标识已添加。".into());
    }
    let destination = app_icons::destination(&package);
    if icon_destinations(state).contains(destination.as_str())
        || state.replacements.contains_key(&destination)
    {
        return Err("生成的图标路径已被占用；请先移除冲突项。".into());
    }
    state.quickapps.push(QuickappEntry {
        declaration: QuickappIcon {
            destination,
            package,
        },
        template: None,
        asset: None,
    });
    state.quickapp_package.clear();
    Ok(())
}

fn add_quickapp() {
    let mut state = lock_state();
    match insert_quickapp(&mut state) {
        Ok(()) => {
            state.status = "已添加快应用。".into();
            state.error = None;
        }
        Err(error) => state.error = Some(format!("无法添加快应用：{error}")),
    }
    drop(state);
    render_current();
}

fn remove_quickapp(package: &str) {
    let mut state = lock_state();
    if let Some(index) = state
        .quickapps
        .iter()
        .position(|entry| entry.declaration.package == package)
    {
        state.quickapps.remove(index);
        // The destination may also back another app or an explicit firmware rule.
        // Canonical QuickApp rules were normalized to declarations during import.
        state.status = format!("已移除快应用 {package} 的图标与声明。");
        state.error = None;
    }
    drop(state);
    render_current();
}

#[derive(Clone, Copy)]
enum IconPick {
    Png,
    Binary,
    Template,
}

fn icon_destination<'a>(state: &'a UiState, package: Option<&str>) -> &'a str {
    package
        .and_then(|package| {
            state
                .quickapps
                .iter()
                .find(|entry| entry.declaration.package == package)
                .map(|entry| entry.declaration.destination.as_str())
        })
        .unwrap_or(&state.canopus_destination)
}

fn validate_icon_input(
    state: &UiState,
    package: Option<&str>,
    mode: IconPick,
    size: usize,
) -> Result<(), String> {
    // PNG output size depends on the layout; input still has an independent single-file cap.
    validate_icon_budget(size, 0, 0, matches!(mode, IconPick::Template))?;
    if !matches!(mode, IconPick::Png) {
        validate_icon_update(state, package, mode, size)?;
    }
    Ok(())
}

fn validate_icon_update(
    state: &UiState,
    package: Option<&str>,
    mode: IconPick,
    size: usize,
) -> Result<(), String> {
    if matches!(mode, IconPick::Template) {
        let old_size = package
            .and_then(|package| {
                state
                    .quickapps
                    .iter()
                    .find(|entry| entry.declaration.package == package)
            })
            .and_then(|entry| entry.template.as_ref())
            .map_or(0, |bytes| bytes.len());
        validate_icon_budget(size, template_bytes(state), old_size, true)
    } else {
        let old_size = replacement_sizes_by_destination(state)
            .get(icon_destination(state, package))
            .copied()
            .unwrap_or(0);
        validate_icon_budget(size, replacement_bytes(state), old_size, false)
    }
}

fn set_shared_icon_asset(
    state: &mut UiState,
    destination: &str,
    bytes: Vec<u8>,
    create_canopus: bool,
) {
    let asset = IconAsset::new(bytes);
    for entry in &mut state.quickapps {
        if entry.declaration.destination == destination {
            entry.asset = Some(asset.clone());
        }
    }
    if state.canopus_destination == destination && (create_canopus || state.canopus.is_some()) {
        state.canopus = Some(asset.clone());
    }
    if let Some(firmware_bytes) = state.replacements.get_mut(destination) {
        *firmware_bytes = asset.bytes.as_ref().clone();
        state.thumbnail_cache.remove(destination);
    }
}

fn set_resource_replacement(state: &mut UiState, path: &str, bytes: Vec<u8>) {
    if icon_destinations(state).contains(path) {
        set_shared_icon_asset(state, path, bytes, false);
    } else {
        state.replacements.insert(path.to_string(), bytes);
    }
}

async fn begin_icon_pick(package: Option<&str>, mode: IconPick) {
    let (template, allow_quantize, resize_filter) = {
        let mut state = lock_state();
        if state.busy {
            return;
        }
        let template = if let Some(package) = package {
            let Some(entry) = state
                .quickapps
                .iter()
                .find(|entry| entry.declaration.package == package)
            else {
                return;
            };
            if matches!(mode, IconPick::Png) {
                entry.template.clone()
            } else {
                None
            }
        } else {
            None
        };
        state.busy = true;
        state.error = None;
        state.status = match mode {
            IconPick::Png => "等待选择图标 PNG…",
            IconPick::Binary => "等待选择 LVGL 图标 BIN…",
            IconPick::Template => "等待选择原始 LVGL BIN 模板（仅用于转换，不导出）…",
        }
        .into();
        (template, state.allow_quantize, state.resize_filter)
    };
    render_current();
    let picked = psys_host::dialog::pick_file(
        psys_host::dialog::PickConfig {
            read: true,
            copy_to: None,
        },
        psys_host::dialog::FilterConfig {
            multiple: false,
            extensions: vec![
                if matches!(mode, IconPick::Png) {
                    "png"
                } else {
                    "bin"
                }
                .into(),
            ],
            default_directory: String::new(),
            default_file_name: String::new(),
        },
    )
    .await;
    let result = match picked {
        Ok(picked) if picked.data.is_empty() => Ok(None),
        Ok(picked) => {
            // The host already owns the selected bytes; bound them before decode/encode
            // can allocate or clone even when a tiny valid header has huge trailing data.
            let preflight = validate_icon_input(&lock_state(), package, mode, picked.data.len());
            preflight
                .and_then(|()| {
                    if matches!(mode, IconPick::Template) {
                        app_icons::inspect_bin(&picked.data)?;
                        Ok(picked.data)
                    } else {
                        app_icons::encode_with_filter(
                            &picked.data,
                            template.as_ref().map(|bytes| bytes.as_slice()),
                            package.is_none(),
                            matches!(mode, IconPick::Binary),
                            allow_quantize,
                            resize_filter,
                        )
                    }
                })
                .map(Some)
        }
        Err(error) => Err(format!("选择图标失败：{error}")),
    };
    let mut state = lock_state();
    state.busy = false;
    match result {
        Ok(Some(bytes)) => {
            if let Err(error) = validate_icon_update(&state, package, mode, bytes.len()) {
                state.status = "图标未更改。".into();
                state.error = Some(error);
            } else {
                let size = bytes.len();
                if matches!(mode, IconPick::Template) {
                    if let Some(entry) = state
                        .quickapps
                        .iter_mut()
                        .find(|entry| Some(entry.declaration.package.as_str()) == package)
                    {
                        entry.template = Some(Arc::new(bytes));
                    }
                } else {
                    let destination = icon_destination(&state, package).to_string();
                    set_shared_icon_asset(&mut state, &destination, bytes, package.is_none());
                }
                state.status = if matches!(mode, IconPick::Template) {
                    format!("已加载模板（{}）。", format_bytes(size))
                } else {
                    format!("已替换图标（{}）。", format_bytes(size))
                };
                state.error = None;
            }
        }
        Ok(None) => state.status = "未选择图标文件；现有编辑保持不变。".into(),
        Err(error) => {
            state.status = "图标未更改。".into();
            state.error = Some(error);
        }
    }
    drop(state);
    render_current();
}

async fn begin_png_pick() {
    let (path, template, allow_quantize, resize_filter) = {
        let mut state = lock_state();
        if state.busy {
            return;
        }
        let Some(path) = state.selected_path.clone() else {
            state.error = Some("请先从文件树选择一个图片资源。".into());
            drop(state);
            render_current();
            return;
        };
        let template = match state
            .firmware
            .as_ref()
            .map(|firmware| firmware.file_bytes(&path))
        {
            Some(Ok(Some(template))) => template,
            Some(Ok(None)) | None => {
                state.error = Some("无法读取原始图片模板。".into());
                drop(state);
                render_current();
                return;
            }
            Some(Err(error)) => {
                state.error = Some(format!("无法读取原始图片模板：{error}"));
                drop(state);
                render_current();
                return;
            }
        };
        if lvgl::inspect_image(template.as_slice()).is_none() {
            state.error = Some("此文件不是受支持的图片资源。".into());
            drop(state);
            render_current();
            return;
        }
        state.busy = true;
        state.status = "等待选择 PNG 图片…".into();
        state.error = None;
        (path, template, state.allow_quantize, state.resize_filter)
    };
    render_current();

    let picked = match psys_host::dialog::pick_file(
        psys_host::dialog::PickConfig {
            read: true,
            copy_to: None,
        },
        psys_host::dialog::FilterConfig {
            multiple: false,
            extensions: vec!["png".into()],
            default_directory: String::new(),
            default_file_name: String::new(),
        },
    )
    .await
    {
        Ok(picked) => picked,
        Err(error) => {
            {
                let mut state = lock_state();
                state.busy = false;
                state.status = "PNG 选择未完成。".into();
                state.error = Some(format!("选择 PNG 失败：{error}；原资源未更改。"));
            }
            render_current();
            return;
        }
    };

    if !picked.data.is_empty() {
        let format_name = lvgl::inspect_image(template.as_slice())
            .map(|info| info.format.display_name())
            .unwrap_or("目标格式");
        let result = lvgl::encode_png_to_template_with_filter(
            &picked.data,
            template.as_slice(),
            allow_quantize,
            resize_filter,
        );
        match result {
            Ok(encoded) => {
                let size = encoded.bytes.len();
                let quantize_status = if encoded.lossy_quantization {
                    "；已进行有损量化"
                } else {
                    ""
                };
                let (preview_uri, preview_error) = preview_from_bytes(&encoded.bytes);
                let mut state = lock_state();
                set_resource_replacement(&mut state, &path, encoded.bytes);
                state.thumbnail_cache.remove(&path);
                state.selected_path = Some(path.clone());
                state.preview_uri = preview_uri;
                state.busy = false;
                state.status = format!(
                    "已将 PNG 转换为 {format_name}（{}{quantize_status}）。",
                    format_bytes(size)
                );
                state.error = preview_error;
            }
            Err(error) => {
                let mut state = lock_state();
                state.busy = false;
                state.status = "PNG 转换未完成，原资源未更改。".into();
                state.error = Some(format!("PNG 转换失败：{error}"));
            }
        }
    } else {
        let mut state = lock_state();
        state.busy = false;
        state.status = "没有读取到 PNG 文件；原资源未更改。".into();
    }
    render_current();
}

async fn begin_binary_pick() {
    let path = {
        let mut state = lock_state();
        if state.busy {
            return;
        }
        let Some(path) = state.selected_path.clone() else {
            state.error = Some("请先从文件树选择要替换的资源。".into());
            drop(state);
            render_current();
            return;
        };
        state.busy = true;
        state.status = "等待选择替换文件…".into();
        state.error = None;
        path
    };
    render_current();

    let picked = match psys_host::dialog::pick_file(
        psys_host::dialog::PickConfig {
            read: true,
            copy_to: None,
        },
        psys_host::dialog::FilterConfig {
            multiple: false,
            extensions: Vec::new(),
            default_directory: String::new(),
            default_file_name: String::new(),
        },
    )
    .await
    {
        Ok(picked) => picked,
        Err(error) => {
            {
                let mut state = lock_state();
                state.busy = false;
                state.status = "文件替换未完成。".into();
                state.error = Some(format!("打开文件选择器失败：{error}"));
            }
            render_current();
            return;
        }
    };

    if !picked.name.is_empty() {
        let mut state = lock_state();
        let picked_size = picked.data.len();
        let new_total = replacement_bytes(&state)
            .saturating_sub(state.replacements.get(&path).map_or(0, Vec::len))
            .saturating_add(picked_size);
        if new_total > 64 * 1024 * 1024 {
            state.busy = false;
            state.status = "替换文件没有应用。".into();
            state.error = Some("替换资源总大小不能超过 CRPack v1 的 64 MiB 上限。".into());
        } else {
            let (preview_uri, preview_error) = preview_from_bytes(&picked.data);
            set_resource_replacement(&mut state, &path, picked.data);
            state.thumbnail_cache.remove(&path);
            state.selected_path = Some(path);
            state.preview_uri = preview_uri;
            state.busy = false;
            state.status = format!(
                "已替换为 {}（{}；当前资源总量 {}）。",
                picked.name,
                format_bytes(picked_size),
                format_bytes(new_total)
            );
            state.error = preview_error;
        }
    } else {
        let mut state = lock_state();
        state.busy = false;
        state.status = "没有读取到替换文件；原资源未更改。".into();
    }
    render_current();
}

fn restore_selected() {
    {
        let mut state = lock_state();
        let Some(path) = state.selected_path.clone() else {
            return;
        };
        state.replacements.remove(&path);
        state.thumbnail_cache.remove(&path);
        let original = state
            .firmware
            .as_ref()
            .filter(|firmware| {
                firmware
                    .file(&path)
                    .is_some_and(|file| file.image.is_some())
            })
            .map(|firmware| firmware.file_bytes(&path));
        match original {
            Some(Ok(Some(bytes))) => {
                let (preview_uri, preview_error) = preview_from_bytes(bytes.as_slice());
                state.preview_uri = preview_uri;
                state.error = preview_error;
            }
            Some(Err(error)) => {
                state.preview_uri = None;
                state.error = Some(format!("无法读取固件原始资源：{error}"));
            }
            _ => {
                state.preview_uri = None;
                state.error = None;
            }
        }
        state.status = "已恢复固件中的原始资源。".into();
    }
    render_current();
}

fn select_file(path: &str) {
    let (bytes, read_error) = {
        let mut state = lock_state();
        if state.busy {
            return;
        }
        let Some((file_size, template_is_image)) = state
            .firmware
            .as_ref()
            .and_then(|firmware| firmware.file(path))
            .map(|file| (file.size, file.image.is_some()))
        else {
            return;
        };

        let mut read_error = None;
        let replacement = state.replacements.get(path);
        let bytes = if let Some(replacement) = replacement {
            lvgl::inspect_i8(replacement).map(|_| Arc::new(replacement.clone()))
        } else if template_is_image {
            match state
                .firmware
                .as_ref()
                .map(|firmware| firmware.file_bytes(path))
            {
                Some(Ok(Some(bytes))) => Some(bytes),
                Some(Ok(None)) | None => None,
                Some(Err(error)) => {
                    read_error = Some(format!("无法读取固件原始资源：{error}"));
                    None
                }
            }
        } else {
            None
        };
        state.selected_path = Some(path.to_string());
        state.status = format!("已选择 /resource/{path} · {}", format_bytes(file_size));
        state.error = read_error.clone();
        (bytes, read_error)
    };
    let (preview_uri, preview_error) = bytes
        .as_deref()
        .map_or((None, None), |data| preview_from_bytes(data.as_slice()));
    {
        let mut state = lock_state();
        state.preview_uri = preview_uri;
        state.error = read_error.or(preview_error);
    }
    render_current();
}

async fn begin_export() {
    {
        let mut state = lock_state();
        if state.busy {
            return;
        }
        if replacement_count(&state) == 0 {
            state.error = Some("请先替换至少一个资源或第三方应用图标。".into());
            drop(state);
            render_current();
            return;
        }
        state.busy = true;
        state.status = "正在生成并校验 CRPack…".into();
        state.error = None;
    }
    render_current();

    let package = {
        let state = lock_state();
        build_project_pack(&state)
    };

    let package = match package {
        Ok(package) => package,
        Err(error) => {
            {
                let mut state = lock_state();
                state.busy = false;
                state.status = "CRPack 未导出；请修正信息后重试。".into();
                state.error = Some(format!("导出校验失败：{error}"));
            }
            render_current();
            return;
        }
    };

    let (theme_id, pack_name) = {
        let state = lock_state();
        (state.theme_id.clone(), state.pack_name.clone())
    };
    let file_name = format!("{}.crpack", safe_file_stem(&theme_id));
    let session = psys_host::dialog::save_file_start(psys_host::dialog::FilterConfig {
        multiple: false,
        extensions: vec!["crpack".into()],
        default_directory: String::new(),
        default_file_name: file_name,
    })
    .await;

    let session = match session {
        Ok(session) => session,
        Err(error) => {
            let mut state = lock_state();
            state.busy = false;
            state.status = "无法打开保存对话框。".into();
            state.error = Some(format!("保存文件失败：{error}"));
            drop(state);
            render_current();
            return;
        }
    };

    let mut write_error = false;
    for chunk in package.chunks(SAVE_CHUNK_BYTES) {
        if psys_host::dialog::save_file_write_chunk(session.session_id, chunk.to_vec())
            .await
            .is_err()
        {
            write_error = true;
            break;
        }
    }

    if write_error {
        psys_host::dialog::save_file_abort(session.session_id).await;
        let mut state = lock_state();
        state.busy = false;
        state.status = "CRPack 写入失败。".into();
        state.error = Some("未能完整写入导出文件；请检查目标位置并重试。".into());
        drop(state);
        render_current();
        return;
    }

    if psys_host::dialog::save_file_finish(session.session_id)
        .await
        .is_err()
    {
        psys_host::dialog::save_file_abort(session.session_id).await;
        let mut state = lock_state();
        state.busy = false;
        state.status = "CRPack 写入失败。".into();
        state.error = Some("未能完成导出文件。".into());
        drop(state);
        render_current();
        return;
    }

    {
        let mut state = lock_state();
        state.busy = false;
        state.status = format!("已导出「{pack_name}」· {}。", format_bytes(package.len()));
        state.error = None;
    }
    render_current();
}

async fn begin_resource_extract(as_png: bool) {
    let path = {
        let mut state = lock_state();
        if state.busy {
            return;
        }
        let Some(path) = state.selected_path.clone() else {
            state.error = Some("请先从文件树选择要提取的资源。".into());
            drop(state);
            render_current();
            return;
        };
        state.busy = true;
        state.error = None;
        state.status = if as_png {
            "正在将当前资源转换为 PNG…".into()
        } else {
            "正在准备提取当前资源…".into()
        };
        path
    };
    render_current();

    let bytes = {
        let state = lock_state();
        resource_bytes(&state, &path)
    };
    let bytes = match bytes {
        Ok(bytes) => bytes,
        Err(error) => {
            let mut state = lock_state();
            state.busy = false;
            state.status = "资源提取未完成。".into();
            state.error = Some(format!("无法读取资源：{error}"));
            drop(state);
            render_current();
            return;
        }
    };
    if as_png && lvgl::inspect_image(bytes.as_slice()).is_none() {
        let mut state = lock_state();
        state.busy = false;
        state.status = "资源提取未完成。".into();
        state.error = Some("此资源不是受支持的图片，无法转换为 PNG。".into());
        drop(state);
        render_current();
        return;
    }

    let resource_name = resource_file_name(&path);
    let file_name = if as_png {
        png_file_name(&resource_name)
    } else {
        resource_name
    };
    let extension = if as_png {
        Some("png".to_string())
    } else {
        file_extension(&file_name)
    };
    let result = if as_png {
        match lvgl::decode_image_png(bytes.as_slice()) {
            Ok((_, png)) => save_resource_file(&png, &file_name, extension.as_deref())
                .await
                .map(|()| png.len()),
            Err(error) => Err(format!("图片转 PNG 失败：{error}")),
        }
    } else {
        save_resource_file(bytes.as_slice(), &file_name, extension.as_deref())
            .await
            .map(|()| bytes.len())
    };

    let mut state = lock_state();
    state.busy = false;
    match result {
        Ok(size) => {
            state.status = format!("已提取当前资源「{}」· {}。", file_name, format_bytes(size));
            state.error = None;
        }
        Err(error) => {
            state.status = "资源提取未完成。".into();
            state.error = Some(error);
        }
    }
    drop(state);
    render_current();
}

fn resource_bytes(state: &UiState, path: &str) -> Result<Arc<Vec<u8>>, String> {
    if let Some(bytes) = state.replacements.get(path) {
        return Ok(Arc::new(bytes.clone()));
    }
    state
        .firmware
        .as_ref()
        .ok_or_else(|| "尚未加载固件。".to_string())?
        .file_bytes(path)?
        .ok_or_else(|| "所选资源已不存在。".to_string())
}

async fn save_resource_file(
    bytes: &[u8],
    file_name: &str,
    extension: Option<&str>,
) -> Result<(), String> {
    let session = psys_host::dialog::save_file_start(psys_host::dialog::FilterConfig {
        multiple: false,
        extensions: extension.map_or_else(Vec::new, |value| vec![value.to_string()]),
        default_directory: String::new(),
        default_file_name: file_name.to_string(),
    })
    .await
    .map_err(|error| format!("无法打开保存对话框：{error}"))?;

    for chunk in bytes.chunks(SAVE_CHUNK_BYTES) {
        if let Err(error) =
            psys_host::dialog::save_file_write_chunk(session.session_id, chunk.to_vec()).await
        {
            psys_host::dialog::save_file_abort(session.session_id).await;
            return Err(format!("未能完整写入提取文件：{error}"));
        }
    }

    if let Err(error) = psys_host::dialog::save_file_finish(session.session_id).await {
        psys_host::dialog::save_file_abort(session.session_id).await;
        return Err(format!("未能完成提取文件：{error}"));
    }
    Ok(())
}

fn resource_file_name(path: &str) -> String {
    path.rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or("resource.bin")
        .to_string()
}

fn file_extension(file_name: &str) -> Option<String> {
    file_name
        .rsplit_once('.')
        .filter(|(stem, extension)| !stem.is_empty() && !extension.is_empty())
        .map(|(_, extension)| extension.to_ascii_lowercase())
}

fn png_file_name(file_name: &str) -> String {
    let stem = file_name
        .rsplit_once('.')
        .filter(|(stem, _)| !stem.is_empty())
        .map_or(file_name, |(stem, _)| stem);
    format!("{stem}.png")
}

fn thumbnail_from_bytes(bytes: &[u8]) -> Option<String> {
    let (_, png) = lvgl::decode_i8_thumbnail_png(bytes, FILE_THUMBNAIL_SIZE).ok()?;
    Some(thumbnail_uri_from_png(png))
}

fn thumbnail_uri_from_png(png: Vec<u8>) -> String {
    format!("data:image/png;base64,{}", STANDARD.encode(png))
}

fn preview_from_bytes(bytes: &[u8]) -> (Option<String>, Option<String>) {
    match lvgl::decode_i8_png(bytes) {
        Ok((_, png)) => (
            Some(format!("data:image/png;base64,{}", STANDARD.encode(png))),
            None,
        ),
        Err(_) => (None, None),
    }
}

fn infer_target(file_name: &str) -> String {
    let name = file_name.to_ascii_lowercase();
    if name.contains("4.100.155") {
        "xiaomi-band-11-4.100.155".into()
    } else if name.contains("4.100.139") {
        "xiaomi-band-11-4.100.139".into()
    } else {
        String::new()
    }
}

fn safe_file_stem(value: &str) -> String {
    let value = value
        .bytes()
        .filter(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-' || *byte == b'_'
        })
        .map(char::from)
        .collect::<String>();
    if value.is_empty() {
        "conora".into()
    } else {
        value
    }
}

fn payload_field(payload: &str, field: &str) -> Option<String> {
    serde_json::from_str::<Value>(payload)
        .ok()?
        .get(field)?
        .as_str()
        .map(str::to_string)
        .or_else(|| {
            serde_json::from_str::<Value>(payload)
                .ok()?
                .get(field)?
                .as_bool()
                .map(|value| value.to_string())
        })
}

fn format_bytes(bytes: usize) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut amount = bytes as f64;
    let mut unit = 0;
    while amount >= 1024.0 && unit < UNITS.len() - 1 {
        amount /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{amount:.1} {}", UNITS[unit])
    }
}

fn lock_state() -> std::sync::MutexGuard<'static, UiState> {
    ui_state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn render_current() {
    let (root_id, tree) = {
        let mut state = lock_state();
        let Some(root_id) = state.root_element_id.clone() else {
            return;
        };
        (root_id, build_main_ui(snapshot(&mut state)))
    };
    psys_host::ui::render(&root_id, tree);
}

pub fn render_main_ui(element_id: &str) {
    let tree = {
        let mut state = lock_state();
        state.root_element_id = Some(element_id.to_string());
        build_main_ui(snapshot(&mut state))
    };
    psys_host::ui::render(element_id, tree);
}

fn build_main_ui(state: UiSnapshot) -> ui::Element {
    let mut root = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .width_full()
        .padding(12)
        .gap(12);

    if state.firmware_name.is_empty() {
        let empty = ui::Element::new(ui::ElementType::Card, None)
            .prop("variant", "surface")
            .prop("size", "3")
            .radius(24)
            .padding(24)
            .flex()
            .flex_direction(ui::FlexDirection::Column)
            .align_start()
            .gap(12)
            .child(text("从固件资源开始", 20))
            .child(text(
                "选择官方 OTA 固件，插件会解包 vela_resource.bin 并建立只读资源树。替换项保留原始路径，完成后只导出修改过的文件。",
                14,
            ))
            .child(button("选择手环固件", "firmware.upload", "solid", "accent").disabled_if(state.busy))
            .child(text("第三方应用图标无需固件。导入 CRPack 会覆盖当前编辑；更换固件仅重置固件资源替换，保留第三方图标。", 12))
            .child(
                ui::Element::new(ui::ElementType::Div, None).flex().gap(8)
                    .child(button("导入 .crpack", "pack.import", "soft", "gray").disabled_if(state.busy))
                    .child(button("导出 .crpack", "pack.export", "solid", "accent").disabled_if(state.busy || state.replacement_count == 0))
                    .child(badge(&format!("已替换 {}（第三方图标 {}）", state.replacement_count, state.icon_replacement_count), "accent")),
            );
        root = root.child(empty);
    } else {
        let upload = button(
            if state.busy {
                "处理中…"
            } else {
                "更换固件"
            },
            "firmware.upload",
            "soft",
            "gray",
        )
        .disabled_if(state.busy)
        .width_full();

        let import = button(
            if state.busy {
                "处理中…"
            } else {
                "导入 .crpack"
            },
            "pack.import",
            "soft",
            "gray",
        )
        .disabled_if(state.busy)
        .width_full();

        let export = button(
            if state.busy {
                "处理中…"
            } else {
                "导出 .crpack"
            },
            "pack.export",
            "solid",
            "accent",
        )
        .disabled_if(state.busy || state.replacement_count == 0)
        .width_full();

        let actions = ui::Element::new(ui::ElementType::Grid, None)
            .grid_template_columns("repeat(3, 1fr)")
            .gap(8)
            .width_full()
            .child(upload)
            .child(import)
            .child(export);

        let display_name = format_firmware_name(&state.firmware_name);
        let summary = ui::Element::new(ui::ElementType::Div, None)
            .flex()
            .flex_direction(ui::FlexDirection::Row)
            .width_full()
            .align_center()
            .gap(8)
            .child(
                ui::Element::new(ui::ElementType::Div, None)
                    .flex()
                    .flex_direction(ui::FlexDirection::Row)
                    .align_center()
                    .gap(6)
                    .flex_grow(1.0)
                    .child(span("📦", 14))
                    .child(span(&display_name, 14).prop("title", &state.firmware_name)),
            )
            .child(badge(&format!("{} 个文件", state.file_count), "gray"))
            .child(badge(
                &format!(
                    "已替换 {}（图标 {}）",
                    state.replacement_count, state.icon_replacement_count
                ),
                if state.replacement_count > 0 {
                    "accent"
                } else {
                    "gray"
                },
            ));

        let toolbar = ui::Element::new(ui::ElementType::Div, None)
            .flex()
            .flex_direction(ui::FlexDirection::Column)
            .width_full()
            .gap(8)
            .child(summary)
            .child(actions)
            .child(text(
                "导入 CRPack 会覆盖当前编辑；更换固件仅重置固件资源替换，保留第三方图标。",
                12,
            ));

        root = root.child(toolbar);

        let browser = build_browser(&state);
        let inspector = build_inspector(&state);
        let workspace = ui::Element::new(ui::ElementType::Grid, None)
            .grid_template_columns("repeat(auto-fit, minmax(min(100%, 340px), 1fr))")
            .gap(12)
            .width_full()
            .child(browser)
            .child(inspector);
        root = root.child(workspace);
    }

    root = root.child(build_app_icons(&state));
    if state.firmware_name.is_empty() {
        root = root.child(build_inspector(&state));
    }
    root.child(build_status(&state))
}

fn build_app_icons(state: &UiSnapshot) -> ui::Element {
    let mut icons = ui::Element::new(ui::ElementType::Grid, None)
        .grid_template_columns("repeat(auto-fit, minmax(min(100%, 280px), 1fr))")
        .width_full()
        .gap(10)
        .child(build_icon_card(&state.canopus, true, state.busy));
    for icon in &state.quickapps {
        icons = icons.child(build_icon_card(icon, false, state.busy));
    }
    let add = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Row)
        .align_end()
        .width_full()
        .gap(8)
        .child(
            field(
                "快应用包名",
                &state.quickapp_package,
                "icons.quickapp.package",
                "输入包名",
            )
            .flex_grow(1.0),
        )
        .child(button("添加", "icons.quickapp.add", "soft", "accent").disabled_if(state.busy));
    let smooth_resize = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .align_center()
        .gap(8)
        .child(
            ui::Element::new(ui::ElementType::Checkbox, None)
                .prop(
                    "checked",
                    if state.resize_filter == lvgl::ResizeFilter::Lanczos3 {
                        "true"
                    } else {
                        "false"
                    },
                )
                .prop("size", "2")
                .on(ui::Event::Change, "pack.smooth_resize")
                .disabled_if(state.busy),
        )
        .child(text(
            "启用平滑抗锯齿缩放（Lanczos3，未勾选时为像素最近邻）",
            13,
        ));
    let quantize = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .align_center()
        .gap(8)
        .child(
            ui::Element::new(ui::ElementType::Checkbox, None)
                .prop(
                    "checked",
                    if state.allow_quantize {
                        "true"
                    } else {
                        "false"
                    },
                )
                .prop("size", "2")
                .on(ui::Event::Change, "pack.quantize")
                .disabled_if(state.busy),
        )
        .child(text("允许调色板颜色超限时进行有损量化（PNG 转换）", 13));
    ui::Element::new(ui::ElementType::Card, None)
        .prop("variant", "surface")
        .prop("size", "2")
        .radius(24)
        .padding(14)
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .align_start()
        .gap(10)
        .child(text("第三方应用图标", 17))
        .child(add)
        .child(smooth_resize)
        .child(quantize)
        .child(icons)
}

fn build_icon_card(icon: &IconSnapshot, canopus: bool, busy: bool) -> ui::Element {
    let event = |action: &str| {
        if canopus {
            format!("icons.canopus.{action}")
        } else {
            format!("icons.quickapp.{action}:{}", icon.name)
        }
    };
    let mut content = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .width_full()
        .gap(8)
        .child(text(&icon.name, 15))
        .child(text(&icon.destination, 12));
    if let Some(info) = icon.template_info {
        content = content.child(text(
            &format!(
                "{}：{}×{} {}{}",
                if canopus { "预设" } else { "原始模板" },
                info.width,
                info.height,
                info.format.display_name(),
                if canopus { "" } else { "（仅转换用）" },
            ),
            12,
        ));
    }
    let preview = ui::Element::new(ui::ElementType::Div, None)
        .width_full()
        .height(140)
        .flex()
        .justify_center()
        .align_center();
    content = content.child(if let Some(uri) = &icon.preview_uri {
        preview.child(
            ui::Element::new(ui::ElementType::Image, Some(uri))
                .max_width(132)
                .max_height(132)
                .prop("alt", &icon.name)
                .prop("draggable", "false"),
        )
    } else {
        preview.child(text(
            if icon.size.is_some() {
                "此图标暂不可预览。"
            } else {
                "尚未替换图标"
            },
            13,
        ))
    });
    if let Some(size) = icon.size {
        content = content.child(badge(&format!("已替换 · {}", format_bytes(size)), "accent"));
    }
    let mut actions = ui::Element::new(ui::ElementType::Grid, None)
        .grid_template_columns("repeat(2, 1fr)")
        .width_full()
        .gap(8)
        .child(button("用 PNG 替换", &event("png"), "soft", "accent").disabled_if(busy))
        .child(button("用 BIN 替换", &event("bin"), "soft", "gray").disabled_if(busy));
    if canopus {
        actions = actions.child(
            button("撤销替换", &event("undo"), "ghost", "gray")
                .disabled_if(busy || icon.size.is_none()),
        );
    } else {
        actions =
            actions.child(button("移除", &event("remove"), "ghost", "gray").disabled_if(busy));
    }
    content = content.child(actions);
    ui::Element::new(ui::ElementType::Card, None)
        .prop("variant", "surface")
        .prop("size", "1")
        .radius(12)
        .padding(12)
        .child(content)
}

fn build_browser(state: &UiSnapshot) -> ui::Element {
    let pills = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Row)
        .align_center()
        .gap(4)
        .child(
            button(
                &format!("全部 {}", state.file_count),
                "browser.filter:all",
                if state.filter_mode == ResourceFilter::All {
                    "solid"
                } else {
                    "ghost"
                },
                if state.filter_mode == ResourceFilter::All {
                    "accent"
                } else {
                    "gray"
                },
            )
            .prop("size", "1")
            .disabled_if(state.busy),
        )
        .child(
            button(
                &format!("图片 {}", state.image_count),
                "browser.filter:images",
                if state.filter_mode == ResourceFilter::Images {
                    "solid"
                } else {
                    "ghost"
                },
                if state.filter_mode == ResourceFilter::Images {
                    "accent"
                } else {
                    "gray"
                },
            )
            .prop("size", "1")
            .disabled_if(state.busy),
        )
        .child(
            button(
                &format!("已替换 {}", state.firmware_replacement_count),
                "browser.filter:replaced",
                if state.filter_mode == ResourceFilter::Replaced {
                    "solid"
                } else if state.firmware_replacement_count > 0 {
                    "soft"
                } else {
                    "ghost"
                },
                if state.filter_mode == ResourceFilter::Replaced
                    || state.firmware_replacement_count > 0
                {
                    "accent"
                } else {
                    "gray"
                },
            )
            .prop("size", "1")
            .disabled_if(state.busy),
        );

    let heading = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Row)
        .width_full()
        .align_center()
        .child(text("资源浏览", 16).flex_grow(1.0))
        .child(pills);

    let breadcrumbs = build_breadcrumbs(state);

    let search = ui::Element::new(ui::ElementType::Input, Some(state.search_query.as_str()))
        .prop("placeholder", "搜索路径或文件名…")
        .prop("size", "2")
        .prop("variant", "surface")
        .prop("radius", "medium")
        .flex_grow(1.0)
        .on(ui::Event::Change, "browser.search")
        .on(ui::Event::KeyDown, "browser.search.key");

    let mut search_row = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Row)
        .width_full()
        .align_center()
        .gap(6)
        .child(search)
        .child(button("筛选", "browser.apply-search", "soft", "gray").disabled_if(state.busy));

    if !state.search_query.trim().is_empty() {
        search_row = search_row
            .child(button("清空", "browser.clear-search", "ghost", "gray").disabled_if(state.busy));
    }

    let mut list = ui::Element::new(ui::ElementType::ScrollArea, None)
        .prop("type", "auto")
        .prop("scrollbars", "vertical")
        .height(440)
        .width_full()
        .radius(12)
        .padding(4)
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .gap(4);

    if state.entries.is_empty() {
        let empty_tip = if state.filter_mode == ResourceFilter::Replaced {
            "暂无已替换资源。可从文件树选择文件，在右侧替换 PNG 或二进制。"
        } else if state.filter_mode == ResourceFilter::Images {
            "当前目录下没有图片资源。"
        } else if !state.search_query.is_empty() {
            "没有找到匹配的资源。"
        } else {
            "此目录没有可浏览的文件。"
        };
        list = list.child(
            ui::Element::new(ui::ElementType::Div, None)
                .padding(16)
                .flex()
                .justify_center()
                .align_center()
                .child(text(empty_tip, 13)),
        );
    } else {
        let show_full_path =
            !state.search_query.is_empty() || state.filter_mode == ResourceFilter::Replaced;
        for entry in &state.entries {
            let row = if entry.is_directory {
                build_directory_row(entry, state.busy)
            } else {
                let is_selected = state.selected_path.as_deref() == Some(entry.path.as_str());
                let current_size = state
                    .replacement_sizes
                    .get(&entry.path)
                    .copied()
                    .unwrap_or(entry.size);
                build_file_row(
                    entry,
                    state.thumbnail_uris.get(&entry.path).map(String::as_str),
                    is_selected,
                    current_size,
                    show_full_path,
                    state.busy,
                )
            };
            list = list.child(row);
        }
    }

    if state.hidden_entries > 0 {
        list = list.child(
            ui::Element::new(ui::ElementType::Div, None)
                .padding_top(6)
                .padding_bottom(6)
                .flex()
                .justify_center()
                .child(text(
                    &format!("还有 {} 项未显示；使用搜索框筛选。", state.hidden_entries),
                    12,
                )),
        );
    }

    let browser_content = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .width_full()
        .gap(8)
        .child(heading)
        .child(breadcrumbs)
        .child(search_row)
        .child(list);

    ui::Element::new(ui::ElementType::Card, None)
        .prop("variant", "surface")
        .prop("size", "2")
        .radius(24)
        .padding(14)
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .align_start()
        .gap(10)
        .child(browser_content)
}

fn build_breadcrumbs(state: &UiSnapshot) -> ui::Element {
    if state.filter_mode == ResourceFilter::Replaced {
        return ui::Element::new(ui::ElementType::Div, None)
            .flex()
            .flex_direction(ui::FlexDirection::Row)
            .width_full()
            .align_center()
            .gap(6)
            .child(
                ui::Element::new(ui::ElementType::Div, None)
                    .flex()
                    .flex_direction(ui::FlexDirection::Row)
                    .align_center()
                    .gap(6)
                    .flex_grow(1.0)
                    .child(span("✏️", 13))
                    .child(span("全部已修改资源清单", 13))
                    .child(badge(
                        &format!("{} 项", state.firmware_replacement_count),
                        "accent",
                    )),
            )
            .child(
                button("返回目录浏览", "browser.filter:all", "ghost", "gray")
                    .prop("size", "1")
                    .disabled_if(state.busy),
            );
    }

    if !state.search_query.trim().is_empty() {
        return ui::Element::new(ui::ElementType::Div, None)
            .flex()
            .flex_direction(ui::FlexDirection::Row)
            .width_full()
            .align_center()
            .gap(6)
            .child(
                ui::Element::new(ui::ElementType::Div, None)
                    .flex()
                    .flex_direction(ui::FlexDirection::Row)
                    .align_center()
                    .gap(6)
                    .flex_grow(1.0)
                    .child(span("🔍", 13))
                    .child(span(
                        &format!("搜索: \"{}\"", state.search_query.trim()),
                        13,
                    ))
                    .child(badge(&format!("{} 项", state.entries.len()), "gray")),
            )
            .child(
                button("清空搜索", "browser.clear-search", "ghost", "gray")
                    .prop("size", "1")
                    .disabled_if(state.busy),
            );
    }

    let mut trail = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Row)
        .align_center()
        .gap(4)
        .flex_grow(1.0);

    let is_root = state.current_dir.is_empty();
    trail = trail.child(
        button(
            "🏠 resource",
            "browser.open:",
            if is_root { "soft" } else { "ghost" },
            if is_root { "accent" } else { "gray" },
        )
        .prop("size", "1")
        .disabled_if(state.busy),
    );

    if !is_root {
        let mut accumulated = String::new();
        let segments = state
            .current_dir
            .split('/')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>();
        let total = segments.len();
        for (idx, segment) in segments.into_iter().enumerate() {
            if !accumulated.is_empty() {
                accumulated.push('/');
            }
            accumulated.push_str(segment);
            let is_last = idx + 1 == total;
            trail = trail.child(span("/", 11));
            trail = trail.child(
                button(
                    segment,
                    &format!("browser.open:{accumulated}"),
                    if is_last { "soft" } else { "ghost" },
                    if is_last { "accent" } else { "gray" },
                )
                .prop("size", "1")
                .disabled_if(state.busy),
            );
        }
    }

    let mut bar = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Row)
        .width_full()
        .align_center()
        .gap(6)
        .child(trail);

    if !state.current_dir.is_empty() {
        bar = bar.child(
            button("← 上级", "browser.parent", "ghost", "gray")
                .prop("size", "1")
                .disabled_if(state.busy),
        );
    }

    bar
}

fn build_directory_row(entry: &BrowserEntry, busy: bool) -> ui::Element {
    let left = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Row)
        .align_center()
        .gap(8)
        .flex_grow(1.0)
        .child(span("📁", 13))
        .child(span(&format!("{}/", entry.name), 13));

    let mut right = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Row)
        .align_center()
        .gap(6)
        .flex_shrink(0.0);

    if entry.has_replacements {
        right = right.child(badge("● 含修改", "accent"));
    }
    if entry.child_count > 0 {
        right = right.child(span(&format!("{} 项", entry.child_count), 12));
    }
    right = right.child(span("›", 13));

    let row = ui::Element::new(ui::ElementType::Button, None)
        .prop("variant", "ghost")
        .prop("color", "gray")
        .prop("size", "2")
        .prop("radius", "medium")
        .width_full()
        .flex()
        .flex_direction(ui::FlexDirection::Row)
        .align_center()
        .padding_left(8)
        .padding_right(8)
        .on(ui::Event::Click, &format!("browser.open:{}", entry.path))
        .disabled_if(busy);

    row.child(left).child(right)
}

fn build_file_row(
    entry: &BrowserEntry,
    thumbnail_uri: Option<&str>,
    selected: bool,
    current_size: usize,
    show_full_path: bool,
    busy: bool,
) -> ui::Element {
    let leading = if entry.image.is_some() {
        let thumbnail = if let Some(uri) = thumbnail_uri {
            ui::Element::new(ui::ElementType::Image, Some(uri))
                .max_width(FILE_THUMBNAIL_SIZE)
                .max_height(FILE_THUMBNAIL_SIZE)
                .prop("alt", &entry.name)
                .prop("draggable", "false")
        } else {
            ui::Element::new(ui::ElementType::Div, None)
                .width(FILE_THUMBNAIL_SIZE)
                .height(FILE_THUMBNAIL_SIZE)
                .radius(4)
                .border(1, "gray")
        };
        ui::Element::new(ui::ElementType::Div, None)
            .width(32)
            .height(32)
            .flex()
            .flex_shrink(0.0)
            .justify_center()
            .align_center()
            .child(thumbnail)
    } else {
        let ext = file_extension(&entry.name).unwrap_or_default();
        let icon = if ext == "bin" || ext == "dat" {
            "📦"
        } else {
            "📄"
        };
        span(icon, 13)
    };

    let display_name = if show_full_path {
        &entry.path
    } else {
        &entry.name
    };

    let left = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Row)
        .align_center()
        .gap(8)
        .flex_grow(1.0)
        .child(leading)
        .child(span(display_name, 13));

    let mut right = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Row)
        .align_center()
        .gap(6)
        .flex_shrink(0.0);

    if let Some(info) = entry.image {
        right = right.child(badge(&format!("{}×{}", info.width, info.height), "gray"));
    } else if let Some(ext) = file_extension(&entry.name) {
        right = right.child(badge(&ext.to_ascii_uppercase(), "gray"));
    }

    if entry.has_replacements {
        right = right.child(badge("已替换", "accent"));
    }

    right = right.child(span(&format_bytes(current_size), 12));

    let mut row = ui::Element::new(ui::ElementType::Button, None)
        .prop("size", "2")
        .prop("radius", "medium")
        .prop(
            "variant",
            if selected {
                "soft"
            } else if entry.has_replacements {
                "surface"
            } else {
                "ghost"
            },
        );

    if !selected && !entry.has_replacements {
        row = row.prop("color", "gray");
    }

    row.width_full()
        .flex()
        .flex_direction(ui::FlexDirection::Row)
        .align_center()
        .padding_left(8)
        .padding_right(8)
        .on(ui::Event::Click, &format!("browser.select:{}", entry.path))
        .disabled_if(busy)
        .child(left)
        .child(right)
}

fn build_inspector(state: &UiSnapshot) -> ui::Element {
    let mut content = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .width_full()
        .gap(10)
        .child(text(
            if state.firmware_name.is_empty() {
                "资源包信息"
            } else {
                "预览与替换"
            },
            17,
        ));

    if let Some(path) = state.selected_path.as_deref() {
        let image_info = state.selected_image;
        let mut selection = ui::Element::new(ui::ElementType::Div, None)
            .flex()
            .flex_direction(ui::FlexDirection::Column)
            .width_full()
            .gap(4)
            .child(text(path, 14))
            .child(text(
                &format!(
                    "{} · {}{}",
                    state
                        .selected_size
                        .map(format_bytes)
                        .unwrap_or_else(|| "未知大小".into()),
                    if state.selected_replaced {
                        "已替换"
                    } else {
                        "固件原始资源"
                    },
                    image_info
                        .map(|info| {
                            if info.width > 0 && info.height > 0 {
                                format!(
                                    " · {}×{} {}",
                                    info.width,
                                    info.height,
                                    info.format.display_name()
                                )
                            } else {
                                format!(" · {}", info.format.display_name())
                            }
                        })
                        .unwrap_or_default(),
                ),
                12,
            ));
        if let Some(preview_uri) = &state.preview_uri {
            let image = ui::Element::new(ui::ElementType::Image, Some(preview_uri.as_str()))
                .width(280)
                .max_height(300)
                .prop("alt", path)
                .prop("draggable", "false");
            selection = selection.child(
                ui::Element::new(ui::ElementType::Div, None)
                    .width_full()
                    .height(320)
                    .flex()
                    .justify_center()
                    .align_center()
                    .child(image),
            );
        } else {
            selection = selection.child(text(
                if state.selected_replaced {
                    "此替换文件不是可预览的图片。"
                } else {
                    "此资源不是受支持的图片格式。"
                },
                13,
            ));
        }
        content = content.child(selection);

        let extract_buttons = ui::Element::new(ui::ElementType::Div, None)
            .flex()
            .flex_direction(ui::FlexDirection::Row)
            .gap(8)
            .child(
                button("提取当前文件", "resource.extract", "soft", "gray").disabled_if(state.busy),
            )
            .child(
                button("转换并提取 PNG", "resource.extract.png", "soft", "accent")
                    .disabled_if(state.busy || image_info.is_none()),
            );
        content = content
            .child(extract_buttons)
            .child(text("若资源已替换，将提取替换后的版本。", 12));

        let image_buttons = ui::Element::new(ui::ElementType::Div, None)
            .flex()
            .flex_direction(ui::FlexDirection::Row)
            .gap(8)
            .child(
                button("用 PNG 替换", "replace.png", "soft", "accent")
                    .disabled_if(state.busy || !state.selected_template_image),
            )
            .child(
                button("替换任意文件", "replace.binary", "soft", "gray").disabled_if(state.busy),
            );
        content = content.child(image_buttons);

        if state.selected_template_image {
            let smooth_checkbox = ui::Element::new(ui::ElementType::Checkbox, None)
                .prop(
                    "checked",
                    if state.resize_filter == lvgl::ResizeFilter::Lanczos3 {
                        "true"
                    } else {
                        "false"
                    },
                )
                .prop("size", "2")
                .on(ui::Event::Change, "pack.smooth_resize")
                .disabled_if(state.busy);
            content = content.child(
                ui::Element::new(ui::ElementType::Div, None)
                    .flex()
                    .flex_direction(ui::FlexDirection::Row)
                    .align_center()
                    .gap(8)
                    .child(smooth_checkbox)
                    .child(text(
                        "启用平滑抗锯齿缩放（Lanczos3，未勾选时为像素最近邻）",
                        13,
                    )),
            );

            let checkbox = ui::Element::new(ui::ElementType::Checkbox, None)
                .prop(
                    "checked",
                    if state.allow_quantize {
                        "true"
                    } else {
                        "false"
                    },
                )
                .prop("size", "2")
                .on(ui::Event::Change, "pack.quantize");
            content = content.child(
                ui::Element::new(ui::ElementType::Div, None)
                    .flex()
                    .flex_direction(ui::FlexDirection::Row)
                    .align_center()
                    .gap(8)
                    .child(checkbox)
                    .child(text("允许调色板颜色超限时进行有损量化", 13)),
            );
        }
        if state.selected_replaced {
            content = content.child(
                button("恢复固件原文件", "replace.restore", "soft", "gray").disabled_if(state.busy),
            );
        }
    } else if !state.firmware_name.is_empty() {
        content = content.child(text("选择左侧文件；图片资源会显示预览。", 14));
    }

    if !state.firmware_name.is_empty() {
        content = content.child(ui::Element::new(ui::ElementType::Separator, None));
        content = content.child(text("资源包信息", 15));
    }
    let metadata = ui::Element::new(ui::ElementType::Grid, None)
        .grid_template_columns("repeat(auto-fit, minmax(min(100%, 180px), 1fr))")
        .gap(8)
        .width_full()
        .child(field("包标识", &state.theme_id, "pack.id", "conora"))
        .child(field(
            "包名称",
            &state.pack_name,
            "pack.name",
            "Conora Resource Pack",
        ))
        .child(field("版本", &state.version, "pack.version", "1.0.0"))
        .child(field("作者（可选）", &state.author, "pack.author", ""))
        .child(field(
            "目标设备（可选）",
            &state.target,
            "pack.target",
            "xiaomi-band-11-4.100.155",
        ));
    content = content.child(metadata);
    content = content.child(field(
        "描述（可选）",
        &state.description,
        "pack.description",
        "",
    ));

    ui::Element::new(ui::ElementType::Card, None)
        .prop("variant", "surface")
        .prop("size", "2")
        .radius(24)
        .padding(14)
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .align_start()
        .gap(10)
        .child(content)
}

fn build_status(state: &UiSnapshot) -> ui::Element {
    let mut status = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Row)
        .width_full()
        .align_center()
        .child(text(&state.status, 13));
    if let Some(error) = &state.error {
        status = status.child(badge(error, "red"));
    }
    status
}

fn field(label: &str, value: &str, event_id: &str, placeholder: &str) -> ui::Element {
    let input = ui::Element::new(ui::ElementType::Input, Some(value))
        .prop("placeholder", placeholder)
        .prop("size", "2")
        .prop("variant", "surface")
        .prop("radius", "medium")
        .width_full()
        .on(ui::Event::Change, event_id);
    ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .width_full()
        .gap(4)
        .child(text(label, 12))
        .child(input)
}

fn format_firmware_name(name: &str) -> String {
    const MAX_LEN: usize = 28;
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= MAX_LEN {
        return name.to_string();
    }
    let prefix: String = chars[..14].iter().collect();
    let suffix: String = chars[chars.len() - 12..].iter().collect();
    format!("{prefix}…{suffix}")
}

fn text(content: &str, size: u32) -> ui::Element {
    ui::Element::new(ui::ElementType::P, Some(content)).size(size)
}

fn span(content: &str, size: u32) -> ui::Element {
    ui::Element::new(ui::ElementType::Span, Some(content)).size(size)
}

fn badge(content: &str, color: &str) -> ui::Element {
    let element = ui::Element::new(ui::ElementType::Badge, Some(content))
        .prop("variant", "soft")
        .prop("size", "1");
    if color == "accent" {
        element
    } else {
        element.prop("color", color)
    }
}

fn button(content: &str, event_id: &str, variant: &str, color: &str) -> ui::Element {
    let element = ui::Element::new(ui::ElementType::Button, Some(content))
        .prop("variant", variant)
        .prop("size", "2")
        .prop("radius", "medium");
    let element = if color == "accent" {
        element
    } else {
        element.prop("color", color)
    };
    element.on(ui::Event::Click, event_id)
}

trait DisabledElement {
    fn disabled_if(self, disabled: bool) -> Self;
}

impl DisabledElement for ui::Element {
    fn disabled_if(self, disabled: bool) -> Self {
        if disabled { self.disabled() } else { self }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_filter_defaults_to_lanczos3_and_can_toggle() {
        let mut state = UiState::default();
        assert_eq!(state.resize_filter, lvgl::ResizeFilter::Lanczos3);

        let snap = snapshot(&mut state);
        assert_eq!(snap.resize_filter, lvgl::ResizeFilter::Lanczos3);

        state.resize_filter = lvgl::ResizeFilter::Nearest;
        let snap = snapshot(&mut state);
        assert_eq!(snap.resize_filter, lvgl::ResizeFilter::Nearest);
    }

    #[test]
    fn smooth_resize_checkbox_toggles_filter() {
        process_change("pack.smooth_resize", r#"{"checked":"false"}"#);
        assert_eq!(lock_state().resize_filter, lvgl::ResizeFilter::Nearest);
        process_change("pack.smooth_resize", r#"{"checked":"true"}"#);
        assert_eq!(lock_state().resize_filter, lvgl::ResizeFilter::Lanczos3);
    }

    #[test]
    fn quickapp_png_exports_without_firmware_or_original_template() {
        let mut state = UiState {
            quickapp_package: "my-app".into(),
            ..UiState::default()
        };
        insert_quickapp(&mut state).unwrap();
        let (_, png) = lvgl::decode_image_png(&app_icons::canopus_template()).unwrap();
        let bytes = app_icons::encode(&png, None, false, false, false).unwrap();
        let destination = state.quickapps[0].declaration.destination.clone();
        set_shared_icon_asset(&mut state, &destination, bytes, false);
        assert!(state.quickapps[0].template.is_none());
        let pack = crpack::parse_crpack(&build_project_pack(&state).unwrap()).unwrap();
        assert_eq!(pack.quickapp_icons[0].package, "my-app");
        assert_eq!(pack.quickapp_icons[0].destination, destination);
        assert_eq!(destination.len(), "quickapp-icons/".len() + 16 + 4);
        let info = app_icons::inspect_bin(&pack.replacements[&destination]).unwrap();
        assert_eq!(info.format, lvgl::ImageFormatKind::Lvgl9Argb8888);
        assert_eq!((info.width, info.height), (117, 117));
    }

    #[test]
    fn new_quickapp_rejects_a_generated_path_already_owned_by_an_imported_icon() {
        let mut state = UiState {
            quickapp_package: "new-package".into(),
            ..UiState::default()
        };
        let mut imported = quickapp("existing-package", true);
        imported.declaration.destination = app_icons::destination("new-package");
        state.quickapps.push(imported);
        assert!(insert_quickapp(&mut state).is_err());
        assert_eq!(state.quickapps.len(), 1);
        assert_eq!(state.quickapp_package, "new-package");
    }

    #[test]
    fn entered_quickapp_identifiers_are_never_trimmed_or_interpreted_as_paths() {
        let mut state = UiState::default();
        for package in ["", " ", " 快应用/ ", "../", r"a:b\c", "a..b", "a\u{0085}"] {
            state.quickapp_package = package.into();
            insert_quickapp(&mut state).unwrap();
            let entry = state.quickapps.last_mut().unwrap();
            assert_eq!(entry.declaration.package, package);
            assert_eq!(
                entry.declaration.destination,
                app_icons::destination(package)
            );
            entry.asset = Some(IconAsset::new(app_icons::canopus_template()));
        }
        let bytes = build_project_pack(&state).unwrap();
        let imported = parse_ui_pack(&bytes).unwrap();
        assert_eq!(imported.quickapp_icons.len(), state.quickapps.len());
        for (icon, entry) in imported.quickapp_icons.iter().zip(&state.quickapps) {
            assert_eq!(icon, &entry.declaration);
        }
    }

    fn quickapp(package: &str, with_asset: bool) -> QuickappEntry {
        let bytes = app_icons::canopus_template();
        QuickappEntry {
            declaration: QuickappIcon {
                package: package.into(),
                destination: app_icons::destination(package),
            },
            template: Some(Arc::new(bytes.clone())),
            asset: with_asset.then(|| IconAsset::new(bytes)),
        }
    }

    #[test]
    fn standalone_app_icons_count_and_export_without_firmware() {
        let mut state = UiState {
            canopus: Some(IconAsset::new(app_icons::canopus_template())),
            quickapps: vec![quickapp("ng.example.app", true)],
            ..UiState::default()
        };
        assert!(state.firmware.is_none());
        assert!(state.replacements.is_empty());
        let snap = snapshot(&mut state);
        assert_eq!(snap.replacement_count, 2);
        assert_eq!(snap.firmware_replacement_count, 0);
        assert!(snap.canopus.preview_uri.is_some());
        assert!(snap.quickapps[0].preview_uri.is_some());
        let unpacked = crpack::parse_crpack(&build_project_pack(&state).unwrap()).unwrap();
        assert_eq!(unpacked.replacements.len(), 2);
        assert_eq!(unpacked.quickapp_icons.len(), 1);
        assert_eq!(
            unpacked.mappings,
            vec![Mapping {
                source: app_icons::CANOPUS_SOURCE.into(),
                destination: app_icons::CANOPUS_DESTINATION.into(),
            }]
        );
        assert_eq!(unpacked.quickapp_icons[0].package, "ng.example.app");
    }

    #[test]
    fn header_snapshots_and_import_confirmation_include_icon_edits() {
        let mut state = UiState {
            canopus: Some(IconAsset::new(app_icons::canopus_template())),
            quickapps: vec![quickapp("ng.example.app", true)],
            ..UiState::default()
        };
        assert!(import_confirmation(&state).contains("共 2 项替换（固件资源 0，第三方图标 2）"));
        // Host UI Elements require AstroBox; exercise the pure snapshots feeding both headers.
        let empty = snapshot(&mut state);
        assert_eq!(empty.replacement_count, 2);
        assert_eq!(empty.icon_replacement_count, 2);
        state.firmware_name = "firmware.bin".into();
        let loaded = snapshot(&mut state);
        assert_eq!(loaded.replacement_count, 2);
        assert_eq!(loaded.icon_replacement_count, 2);
    }

    #[test]
    fn unsupported_non_resource_mappings_are_rejected_not_dropped() {
        let replacements = BTreeMap::from([("custom.bin".into(), vec![1, 2, 3])]);
        let mapping = Mapping {
            source: "/data/custom/icon.bin".into(),
            destination: "custom.bin".into(),
        };
        let package = crpack::build_crpack_with_icons(
            &PackOptions {
                theme_id: "conora",
                name: "Custom",
                version: None,
                author: None,
                description: None,
                target: None,
                replacements: &replacements,
            },
            std::slice::from_ref(&mapping),
            &[],
        )
        .unwrap();
        assert!(crpack::parse_crpack(&package).is_ok());
        let error = parse_ui_pack(&package).unwrap_err();
        assert!(error.contains(&mapping.source));
        assert!(error.contains("不支持映射来源"));
        let mut state = UiState::default();
        state.mappings.push(mapping);
        // Even a rule with no surviving destination must error, rather than be filtered out.
        assert!(
            export_assets(&state)
                .unwrap_err()
                .contains("不支持映射来源")
        );
    }

    #[test]
    fn templates_are_not_replacements_and_incomplete_declarations_cannot_export() {
        let mut state = UiState::default();
        state.quickapps.push(quickapp("ng.example.app", false));
        assert_eq!(replacement_count(&state), 0);
        assert_eq!(replacement_bytes(&state), 0);
        assert!(
            build_project_pack(&state)
                .unwrap_err()
                .contains("尚未替换图标")
        );
        state.quickapps.clear();
        assert!(export_assets(&state).unwrap().0.is_empty());
    }

    #[test]
    fn imported_app_declarations_and_custom_canopus_mapping_round_trip() {
        let mut state = UiState::default();
        let bytes = app_icons::canopus_template();
        let canopus = Mapping {
            source: app_icons::CANOPUS_SOURCE.into(),
            destination: "custom/manager.bin".into(),
        };
        let declaration = QuickappIcon {
            package: "ng.example.app".into(),
            destination: "custom/quick.bin".into(),
        };
        import_assets(
            &mut state,
            BTreeMap::from([
                (canopus.destination.clone(), bytes.clone()),
                (declaration.destination.clone(), bytes.clone()),
                ("image.bin".into(), bytes),
            ]),
            vec![
                canopus.clone(),
                Mapping {
                    source: "/resource/image.bin".into(),
                    destination: "image.bin".into(),
                },
            ],
            vec![declaration.clone()],
        );
        assert_eq!(state.replacements.len(), 1);
        assert_eq!(replacement_count(&state), 3);
        assert!(state.quickapps[0].template.is_some());
        let unpacked = crpack::parse_crpack(&build_project_pack(&state).unwrap()).unwrap();
        assert_eq!(unpacked.quickapp_icons, vec![declaration]);
        assert!(unpacked.mappings.contains(&canopus));
        assert!(
            unpacked
                .mappings
                .iter()
                .any(|mapping| mapping.source == "/resource/image.bin")
        );
        assert!(
            !unpacked
                .mappings
                .iter()
                .any(|mapping| mapping.source == "/resource/custom/quick.bin")
        );
    }

    #[test]
    fn canonical_quickapp_mapping_import_normalizes_once() {
        let mut state = UiState::default();
        let declaration = QuickappIcon {
            package: "ng.example.app".into(),
            destination: "icons/example.bin".into(),
        };
        import_assets(
            &mut state,
            BTreeMap::from([(
                declaration.destination.clone(),
                app_icons::canopus_template(),
            )]),
            vec![Mapping {
                source: format!("@quickapp-icon/{}", declaration.package),
                destination: declaration.destination.clone(),
            }],
            vec![declaration.clone()],
        );
        assert_eq!(state.quickapps.len(), 1);
        assert!(state.mappings.is_empty());
        assert!(state.replacements.is_empty());
        let unpacked = crpack::parse_crpack(&build_project_pack(&state).unwrap()).unwrap();
        assert!(unpacked.mappings.is_empty());
        assert_eq!(unpacked.quickapp_icons, vec![declaration]);
    }

    #[test]
    fn mapping_only_quickapp_pack_becomes_one_declaration_on_import() {
        let declaration = QuickappIcon {
            package: "ng.example.app".into(),
            destination: "icons/example.bin".into(),
        };
        let replacements = BTreeMap::from([(
            declaration.destination.clone(),
            app_icons::canopus_template(),
        )]);
        let package = crpack::build_crpack_with_icons(
            &PackOptions {
                theme_id: "conora",
                name: "Icons",
                version: None,
                author: None,
                description: None,
                target: None,
                replacements: &replacements,
            },
            &[Mapping {
                source: format!("@quickapp-icon/{}", declaration.package),
                destination: declaration.destination.clone(),
            }],
            &[],
        )
        .unwrap();
        let unpacked = crpack::parse_crpack(&package).unwrap();
        let mut state = UiState::default();
        import_assets(
            &mut state,
            unpacked.replacements,
            unpacked.mappings,
            unpacked.quickapp_icons,
        );
        assert_eq!(state.quickapps.len(), 1);
        assert_eq!(replacement_count(&state), 1);
        let unpacked = crpack::parse_crpack(&build_project_pack(&state).unwrap()).unwrap();
        assert_eq!(unpacked.quickapp_icons, vec![declaration]);
        assert!(unpacked.mappings.is_empty());
    }

    #[test]
    fn firmware_workspace_reset_retains_icon_assets_templates_and_mappings() {
        let mut state = UiState::default();
        let bytes = app_icons::canopus_template();
        state.replacements.insert("image.bin".into(), bytes.clone());
        state.canopus = Some(IconAsset::new(bytes));
        state.quickapps.push(quickapp("ng.example.app", true));
        state.mappings.push(Mapping {
            source: app_icons::CANOPUS_SOURCE.into(),
            destination: state.canopus_destination.clone(),
        });
        reset_firmware_resource_edits(&mut state);
        assert!(state.replacements.is_empty());
        assert_eq!(replacement_count(&state), 2);
        assert!(state.quickapps[0].template.is_some());
        assert_eq!(state.mappings.len(), 1);
        assert!(build_project_pack(&state).is_ok());
    }

    #[test]
    fn shared_app_destinations_clone_once_and_export_all_declarations() {
        let mut state = UiState::default();
        let bytes = app_icons::canopus_template();
        let destination = "shared/icon.bin".to_string();
        let canopus = Mapping {
            source: app_icons::CANOPUS_SOURCE.into(),
            destination: destination.clone(),
        };
        let declarations = ["ng.example.one", "ng.example.two"]
            .map(|package| QuickappIcon {
                package: package.into(),
                destination: destination.clone(),
            })
            .to_vec();
        import_assets(
            &mut state,
            BTreeMap::from([(destination.clone(), bytes.clone())]),
            vec![canopus.clone()],
            declarations.clone(),
        );
        assert!(state.replacements.is_empty());
        assert_eq!(replacement_count(&state), 1);
        assert_eq!(replacement_bytes(&state), bytes.len());
        assert_eq!(icon_replacement_count(&state), 1);
        let first = state.quickapps[0].asset.as_ref().unwrap();
        let second = state.quickapps[1].asset.as_ref().unwrap();
        assert!(Arc::ptr_eq(&first.bytes, &second.bytes));
        assert!(Arc::ptr_eq(
            &first.bytes,
            &state.canopus.as_ref().unwrap().bytes
        ));
        let unpacked = parse_ui_pack(&build_project_pack(&state).unwrap()).unwrap();
        assert_eq!(unpacked.replacements.len(), 1);
        assert_eq!(unpacked.quickapp_icons, declarations);
        assert_eq!(unpacked.mappings, vec![canopus]);
    }

    #[test]
    fn shared_application_file_backed_by_firmware_mapping_is_preserved_and_updated() {
        let mut state = UiState::default();
        let bytes = app_icons::canopus_template();
        let destination = "shared.bin".to_string();
        let mappings = vec![
            Mapping {
                source: "/resource/original.bin".into(),
                destination: destination.clone(),
            },
            Mapping {
                source: app_icons::CANOPUS_SOURCE.into(),
                destination: destination.clone(),
            },
        ];
        import_assets(
            &mut state,
            BTreeMap::from([(destination.clone(), bytes)]),
            mappings.clone(),
            vec![QuickappIcon {
                package: "ng.example.app".into(),
                destination: destination.clone(),
            }],
        );
        assert!(state.replacements.contains_key(&destination));
        assert_eq!(replacement_count(&state), 1);
        let snap = snapshot(&mut state);
        assert_eq!(snap.replacement_count, 1);
        assert_eq!(snap.firmware_replacement_count, 1);
        assert_eq!(snap.icon_replacement_count, 1);
        let mut edited = app_icons::canopus_template();
        edited[12] = 255;
        set_shared_icon_asset(&mut state, &destination, edited.clone(), false);
        assert_eq!(state.replacements[&destination], edited);
        assert_eq!(
            state.quickapps[0].asset.as_ref().unwrap().bytes.as_slice(),
            edited
        );
        let unpacked = parse_ui_pack(&build_project_pack(&state).unwrap()).unwrap();
        assert_eq!(unpacked.replacements.len(), 1);
        assert_eq!(unpacked.mappings, mappings);
        assert_eq!(unpacked.replacements[&destination], edited);
    }

    #[test]
    fn unused_imported_files_remain_unmapped_even_with_new_sibling_replacements() {
        let mut state = UiState::default();
        let mapped = Mapping {
            source: "/resource/original.bin".into(),
            destination: "icons/mapped.bin".into(),
        };
        import_assets(
            &mut state,
            BTreeMap::from([
                ("icons/unused.bin".into(), vec![1]),
                ("icons/mapped.bin".into(), vec![2]),
            ]),
            vec![mapped.clone()],
            vec![],
        );
        let unchanged = parse_ui_pack(&build_project_pack(&state).unwrap()).unwrap();
        assert_eq!(unchanged.mappings, vec![mapped.clone()]);
        assert_eq!(unchanged.replacements.len(), 2);
        state.replacements.insert("icons/a.bin".into(), vec![3]);
        state.replacements.insert("icons/b.bin".into(), vec![4]);
        let edited = parse_ui_pack(&build_project_pack(&state).unwrap()).unwrap();
        assert_eq!(edited.replacements.len(), 4);
        assert!(edited.mappings.contains(&mapped));
        assert_eq!(edited.mappings.len(), 3);
        assert!(
            edited
                .mappings
                .iter()
                .any(|mapping| mapping.source == "/resource/icons/a.bin")
        );
        assert!(
            edited
                .mappings
                .iter()
                .any(|mapping| mapping.source == "/resource/icons/b.bin")
        );
        assert!(
            !edited
                .mappings
                .iter()
                .any(|mapping| mapping_covers(mapping, "icons/unused.bin"))
        );
    }

    #[test]
    fn firmware_load_clears_stale_imported_resource_rules_before_new_edits() {
        let mut state = UiState::default();
        let canopus = Mapping {
            source: app_icons::CANOPUS_SOURCE.into(),
            destination: "canopus.bin".into(),
        };
        import_assets(
            &mut state,
            BTreeMap::from([
                ("image.bin".into(), vec![1]),
                ("canopus.bin".into(), app_icons::canopus_template()),
            ]),
            vec![
                Mapping {
                    source: "/resource/stale.bin".into(),
                    destination: "image.bin".into(),
                },
                canopus.clone(),
            ],
            vec![],
        );
        reset_firmware_resource_edits(&mut state);
        assert_eq!(state.mappings, vec![canopus.clone()]);
        assert!(state.imported_paths.is_empty());
        state.replacements.insert("image.bin".into(), vec![2]);
        let unpacked = parse_ui_pack(&build_project_pack(&state).unwrap()).unwrap();
        assert!(unpacked.mappings.contains(&canopus));
        assert!(
            unpacked
                .mappings
                .iter()
                .any(|mapping| mapping.source == "/resource/image.bin")
        );
        assert!(
            !unpacked
                .mappings
                .iter()
                .any(|mapping| mapping.source == "/resource/stale.bin")
        );
    }

    #[test]
    fn icon_input_size_is_rejected_before_decoding_valid_bin_with_oversized_tail() {
        let state = UiState::default();
        let mut input = app_icons::canopus_template();
        input.resize(MAX_ICON_BYTES + 1, 0);
        assert!(
            validate_icon_input(&state, None, IconPick::Binary, input.len())
                .unwrap_err()
                .contains("单个图标上传")
        );
        assert!(validate_icon_input(&state, None, IconPick::Png, input.len()).is_err());
        assert!(validate_icon_input(&state, None, IconPick::Template, input.len()).is_err());
    }

    #[test]
    fn template_and_replacement_budgets_are_independent_and_subtract_previous_size() {
        let mut state = UiState {
            quickapps: vec![
                quickapp("ng.example.one", false),
                quickapp("ng.example.two", false),
            ],
            canopus: Some(IconAsset::new(vec![0; 8])),
            ..UiState::default()
        };
        // Small payloads make the arithmetic boundary test cheap; format validation occurs
        // only after this preflight in the upload path.
        state.quickapps[0].template = Some(Arc::new(vec![0; 32]));
        state.quickapps[1].template = Some(Arc::new(vec![0; 64]));
        state.replacements.insert("resource.bin".into(), vec![0; 4]);
        assert_eq!(template_bytes(&state), 96);
        assert_eq!(replacement_bytes(&state), 12);
        assert!(
            validate_icon_update(
                &state,
                Some("ng.example.one"),
                IconPick::Template,
                MAX_ICON_BYTES - 64
            )
            .is_ok()
        );
        assert!(
            validate_icon_update(
                &state,
                Some("ng.example.one"),
                IconPick::Template,
                MAX_ICON_BYTES - 63
            )
            .unwrap_err()
            .contains("模板总大小")
        );
        assert!(validate_icon_update(&state, None, IconPick::Binary, MAX_ICON_BYTES - 4).is_ok());
        assert!(validate_icon_update(&state, None, IconPick::Binary, MAX_ICON_BYTES - 3).is_err());
        assert!(validate_icon_input(&state, None, IconPick::Png, MAX_ICON_BYTES).is_ok());
    }

    #[test]
    fn extracted_names_use_the_resource_leaf_and_png_extension() {
        assert_eq!(resource_file_name("app/icons/confirm.bin"), "confirm.bin");
        assert_eq!(png_file_name("confirm.bin"), "confirm.png");
        assert_eq!(png_file_name("icon"), "icon.png");
    }

    #[test]
    fn file_extension_ignores_dotfiles_and_normalizes_case() {
        assert_eq!(file_extension("confirm.BIN"), Some("bin".into()));
        assert_eq!(file_extension(".hidden"), None);
    }

    #[test]
    fn format_firmware_name_truncates_long_names_with_ellipsis() {
        assert_eq!(format_firmware_name("short.bin"), "short.bin");
        let exact_28 = "1234567890123456789012345678";
        assert_eq!(format_firmware_name(exact_28), exact_28);

        let long_name = "miwear.watch.p67tc_v3.101.043_full_a4ce8564.bin";
        assert_eq!(
            format_firmware_name(long_name),
            "miwear.watch.p…a4ce8564.bin"
        );
    }
}
