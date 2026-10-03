use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, OnceLock};

use astrobox_ng_wit::astrobox::psys_host_v4::{self as psys_host, ui};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::Value;

use crate::crpack::{self, Mapping, PackOptions, QuickappIcon};
use crate::firmware::{BrowserEntry, FirmwareIndex};
use crate::lvgl;
use corona_core::{app_icons, runtime};

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

#[derive(Clone)]
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

#[derive(Clone, Default)]
struct CustomDraft {
    source: String,
    destination: String,
    asset_path: String,
    editing: Option<usize>,
    advanced: bool,
    bytes: Option<Arc<Vec<u8>>>,
    template: Option<Arc<Vec<u8>>>,
    template_disabled: bool,
    upload_name: String,
    preview_uri: Option<String>,
    displayed_size: Option<usize>,
    displayed_image: Option<lvgl::I8Info>,
    feedback: Option<String>,
}

struct RuleUndo {
    mappings: Vec<Mapping>,
    assets: BTreeMap<String, Option<Vec<u8>>>,
    replacement_paths: BTreeSet<String>,
    templates: BTreeMap<String, Arc<Vec<u8>>>,
    authored_paths: BTreeSet<String>,
}

#[derive(Clone)]
struct RuleSnapshot {
    index: usize,
    source: String,
    destination: String,
    size: usize,
    members: Vec<(String, usize)>,
    verification: String,
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
    authored_paths: BTreeSet<String>,
    custom_templates: BTreeMap<String, Arc<Vec<u8>>>,
    custom_draft: CustomDraft,
    rule_undo: Option<RuleUndo>,
    canopus_destination: String,
    canopus: Option<IconAsset>,
    quickapps: Vec<QuickappEntry>,
    quickapp_package: String,
    theme_id: String,
    pack_name: String,
    version: String,
    version_code: String,
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
            authored_paths: BTreeSet::new(),
            custom_templates: BTreeMap::new(),
            custom_draft: CustomDraft::default(),
            rule_undo: None,
            canopus_destination: app_icons::CANOPUS_DESTINATION.into(),
            canopus: None,
            quickapps: Vec::new(),
            quickapp_package: String::new(),
            theme_id: "corona".into(),
            pack_name: "Corona Resource Pack".into(),
            version: "1.0.0".into(),
            version_code: "1".into(),
            author: String::new(),
            description: String::new(),
            target: String::new(),
            allow_quantize: false,
            resize_filter: lvgl::ResizeFilter::default(),
            busy: false,
            status: "自定义规则和第三方图标无需固件；也可选择固件浏览资源。".into(),
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
    custom_rules: Vec<RuleSnapshot>,
    unmapped_assets: Vec<(String, usize)>,
    custom_draft: CustomDraft,
    can_undo_rule: bool,
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
    version_code: String,
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
                for file in firmware.files() {
                    let path = &file.path;
                    let Some(bytes) = resource_replacement(state, path) else {
                        continue;
                    };
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
                    let image =
                        lvgl::inspect_i8(bytes).or_else(|| original.and_then(|file| file.image));
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
                            entry.has_replacements = firmware.files().iter().any(|file| {
                                file.path.starts_with(&prefix)
                                    && resource_replacement(state, &file.path).is_some()
                            });
                            if is_images_only {
                                firmware.dir_has_images(&entry.path)
                            } else {
                                true
                            }
                        } else {
                            entry.has_replacements =
                                resource_replacement(state, &entry.path).is_some();
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
                            has_replacements: resource_replacement(state, &file.path).is_some(),
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
                && resource_replacement(state, &entry.path).is_none()
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
            let thumbnail_uri = resource_replacement(state, &entry.path)
                .and_then(thumbnail_from_bytes)
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
        resource_replacement(state, path)
            .map(<[u8]>::len)
            .or_else(|| selected.map(|file| file.size))
    });
    let selected_replaced = state
        .selected_path
        .as_ref()
        .is_some_and(|path| resource_replacement(state, path).is_some());
    let selected_image = state.selected_path.as_ref().and_then(|path| {
        if selected_replaced {
            resource_replacement(state, path).and_then(lvgl::inspect_image)
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
        custom_rules: state
            .mappings
            .iter()
            .enumerate()
            .filter(|(_, rule)| ordinary_mapping(rule))
            .map(|(index, rule)| RuleSnapshot {
                index,
                source: rule.source.clone(),
                destination: rule.destination.clone(),
                size: replacement_sizes_by_destination(state)
                    .iter()
                    .filter(|(path, _)| mapping_covers(rule, path))
                    .map(|(_, size)| *size)
                    .sum(),
                members: if rule.destination.ends_with('/') {
                    replacement_sizes_by_destination(state)
                        .iter()
                        .filter(|(path, _)| mapping_covers(rule, path))
                        .map(|(path, size)| ((*path).to_string(), *size))
                        .collect()
                } else {
                    Vec::new()
                },
                verification: rule_verification(state, rule),
            })
            .collect(),
        unmapped_assets: state
            .replacements
            .iter()
            .filter(|(path, _)| {
                !state.mappings.iter().any(|rule| mapping_covers(rule, path))
                    && !icon_destinations(state).contains(path.as_str())
            })
            .map(|(path, bytes)| (path.clone(), bytes.len()))
            .collect(),
        custom_draft: state.custom_draft.clone(),
        can_undo_rule: state.rule_undo.is_some(),
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
        firmware_replacement_count: state.firmware.as_ref().map_or(0, |firmware| {
            firmware
                .files()
                .iter()
                .filter(|file| resource_replacement(state, &file.path).is_some())
                .count()
        }),
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
            .firmware
            .as_ref()
            .map(|firmware| {
                firmware
                    .files()
                    .iter()
                    .filter_map(|file| {
                        resource_replacement(state, &file.path)
                            .map(|bytes| (file.path.clone(), bytes.len()))
                    })
                    .collect()
            })
            .unwrap_or_default(),
        theme_id: state.theme_id.clone(),
        pack_name: state.pack_name.clone(),
        version: state.version.clone(),
        version_code: state.version_code.clone(),
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
        "custom.pick.image" => begin_custom_pick(CustomPick::Image).await,
        "custom.pick.raw" => begin_custom_pick(CustomPick::Raw).await,
        "custom.pick.template" => begin_custom_pick(CustomPick::Template).await,
        "custom.save" => {
            let mut state = lock_state();
            match commit_custom_rule(&mut state) {
                Ok(()) => {
                    state.status = "规则已保存；运行时路径未验证。可撤销本次修改。".into();
                    state.error = None;
                }
                Err(error) => {
                    state.status = "规则未保存，现有文件保持不变。".into();
                    state.error = Some(error);
                }
            }
            drop(state);
            render_current();
        }
        "custom.cancel" => {
            let mut state = lock_state();
            state.custom_draft = CustomDraft::default();
            state.error = None;
            state.status = "已取消草稿；现有规则和文件保持不变。".into();
            drop(state);
            render_current();
        }
        "custom.advanced" => {
            let mut state = lock_state();
            state.custom_draft.advanced = !state.custom_draft.advanced;
            drop(state);
            render_current();
        }
        "custom.template.clear" => {
            let mut state = lock_state();
            state.custom_draft.template = None;
            state.custom_draft.template_disabled = true;
            state.custom_draft.bytes = None;
            state.custom_draft.upload_name.clear();
            cache_custom_preview(&mut state);
            state.status = "模板已从草稿移除；请重新选择 PNG/BIN。现有文件保持不变。".into();
            drop(state);
            render_current();
        }
        "custom.undo" => {
            undo_custom_rule(&mut lock_state());
            render_current();
        }
        _ if event_id.starts_with("custom.member:") => {
            if let Some((index, path)) = event_id["custom.member:".len()..].split_once(':')
                && let Ok(index) = index.parse()
            {
                edit_custom_member(&mut lock_state(), index, path);
                render_current();
            }
        }
        _ if event_id.starts_with("custom.edit:") => {
            if let Ok(index) = event_id["custom.edit:".len()..].parse() {
                edit_custom_rule(&mut lock_state(), index);
                render_current();
            }
        }
        _ if event_id.starts_with("custom.delete:") => {
            if let Ok(index) = event_id["custom.delete:".len()..].parse() {
                delete_custom_rule(&mut lock_state(), index);
                render_current();
            }
        }
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
            state.rule_undo = None;
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
            "custom.source" => {
                state.custom_draft.source = value;
                false
            }
            "custom.destination" => change_custom_target(&mut state, value, false),
            "custom.asset_path" => change_custom_target(&mut state, value, true),
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
            "pack.version_code" => {
                state.version_code = value;
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

fn change_custom_target(state: &mut UiState, value: String, member: bool) -> bool {
    let template = original_template(state, &value);
    let template_changed = template.as_deref() != state.custom_draft.template.as_deref();
    let invalidated = template_changed && state.custom_draft.bytes.is_some();
    if template_changed {
        state.custom_draft.bytes = None;
        state.custom_draft.upload_name.clear();
    }
    state.custom_draft.template = template;
    state.custom_draft.template_disabled = false;
    if member {
        state.custom_draft.asset_path = value;
    } else {
        state.custom_draft.destination = value;
    }
    if state.custom_draft.bytes.is_none() {
        cache_custom_preview(state);
    }
    if invalidated {
        let feedback = "模板已改变，已取消暂存文件。请重新选择 PNG 进行转换，或重新选择 BIN / 原始文件；当前已保存文件未更改。".to_string();
        state.custom_draft.feedback = Some(feedback.clone());
        state.status = feedback;
        state.error = None;
    }
    // Invalidation must render immediately; otherwise the host keeps showing the
    // obsolete staged-file badge until the next unrelated action.
    invalidated
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
                if state.target.is_empty() && !target.is_empty() {
                    state.target = target;
                }
                state.busy = false;
                let icon_count = icon_replacement_count(&state);
                state.status = format!(
                    "固件已解包，发现 {count} 个资源文件。全部规则和替换已保留（第三方图标 {icon_count}）；旧固件编辑作为待验证规则保留。"
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
            state.version_code = unpacked
                .version_code
                .map(|v| v.to_string())
                .unwrap_or_default();
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
    for (path, bytes) in &unpacked.replacements {
        let app_only = (declarations.iter().any(|icon| &icon.destination == path)
            || unpacked
                .mappings
                .iter()
                .any(|rule| rule.source == app_icons::CANOPUS_SOURCE && &rule.destination == path))
            && !unpacked
                .mappings
                .iter()
                .any(|rule| ordinary_mapping(rule) && mapping_covers(rule, path));
        if !app_only && app_icons::inspect_bin(bytes).is_ok() {
            validate_icon_budget(bytes.len(), templates, 0, true)?;
            templates += bytes.len();
        }
    }
    Ok(unpacked)
}

fn validate_ui_mappings(mappings: &[Mapping]) -> Result<(), String> {
    for mapping in mappings {
        crpack::validate_relative_destination(&mapping.destination)?;
        if mapping.source == app_icons::CANOPUS_SOURCE {
            continue;
        }
        if let Some(package) = mapping
            .source
            .strip_prefix(app_icons::QUICKAPP_SOURCE_PREFIX)
        {
            app_icons::validate_package(package)?;
            continue;
        }
        crpack::validate_absolute_source(&mapping.source)?;
        if mapping.source.ends_with('/') != mapping.destination.ends_with('/') {
            return Err("目录规则的来源和目标都必须以 / 结尾。".into());
        }
    }
    Ok(())
}

fn import_confirmation(state: &UiState) -> String {
    let mut status = format!(
        "已导入「{}」，共 {} 项替换（规则/文件 {}，第三方图标 {}）。",
        state.pack_name,
        replacement_count(state),
        state.replacements.len(),
        icon_replacement_count(state),
    );
    if let Some(firmware) = &state.firmware {
        let missing = state
            .mappings
            .iter()
            .filter_map(|rule| rule.source.strip_prefix("/resource/"))
            .filter(|path| {
                if path.ends_with('/') {
                    !firmware.directory_exists(path.trim_end_matches('/'))
                } else {
                    firmware.file(path).is_none()
                }
            })
            .count();
        if missing > 0 {
            status.push_str(&format!(
                "其中 {missing} 条来源规则未关联当前固件；运行时路径未验证，不影响导入与导出。"
            ));
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
        .custom_templates
        .values()
        .map(|bytes| bytes.len())
        .sum::<usize>()
        + state
            .quickapps
            .iter()
            .filter_map(|entry| entry.template.as_ref())
            .map(|bytes| bytes.len())
            .sum::<usize>()
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

// Attaching/switching firmware changes only the read-only browser. All edits remain
// pending/authoritative until the user explicitly edits or deletes a rule.
fn reset_firmware_resource_edits(state: &mut UiState) {
    state.current_dir.clear();
    state.search_query.clear();
    state.filter_mode = ResourceFilter::All;
    state.selected_path = None;
    state.preview_uri = None;
    state.thumbnail_cache.clear();
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
    state.authored_paths.clear();
    state.custom_draft = CustomDraft::default();
    state.rule_undo = None;
    state.custom_templates = replacements
        .iter()
        .filter(|(path, bytes)| {
            let app_only = (declarations.iter().any(|icon| &icon.destination == *path)
                || mappings.iter().any(|rule| {
                    rule.source == app_icons::CANOPUS_SOURCE && &rule.destination == *path
                }))
                && !mappings
                    .iter()
                    .any(|rule| ordinary_mapping(rule) && mapping_covers(rule, path));
            !app_only && app_icons::inspect_bin(bytes).is_ok()
        })
        .map(|(path, bytes)| (path.clone(), Arc::new(bytes.clone())))
        .collect();
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
        let firmware_backed = mappings
            .iter()
            .any(|mapping| ordinary_mapping(mapping) && mapping_covers(mapping, path));
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

fn ordinary_mapping(mapping: &Mapping) -> bool {
    mapping.source.starts_with('/') && mapping.source != app_icons::CANOPUS_SOURCE
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
    // Ordered imported/custom rules are authoritative, even when currently missing
    // files. Core export validation reports incomplete rules without discarding them.
    let mut mappings = state.mappings.clone();
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
    // Never infer resource rules from archive files. Firmware edits create their
    // explicit rule when committed; unreferenced assets remain unreferenced.
    Ok((replacements, mappings, declarations))
}

fn rule_verification(state: &UiState, rule: &Mapping) -> String {
    if let Some(path) = rule.source.strip_prefix("/resource/")
        && let Some(firmware) = &state.firmware
        && (firmware.file(path).is_some() || firmware.directory_exists(path.trim_end_matches('/')))
    {
        return "来源在当前固件中；仍需设备验证".into();
    }
    "运行时路径未验证（无需固件）".into()
}

fn destination_bytes<'a>(state: &'a UiState, destination: &str) -> Option<&'a [u8]> {
    state
        .replacements
        .get(destination)
        .map(Vec::as_slice)
        .or_else(|| {
            state
                .canopus
                .as_ref()
                .filter(|_| state.canopus_destination == destination)
                .map(|asset| asset.bytes.as_slice())
        })
        .or_else(|| {
            state
                .quickapps
                .iter()
                .find(|entry| entry.declaration.destination == destination)
                .and_then(|entry| entry.asset.as_ref())
                .map(|asset| asset.bytes.as_slice())
        })
}

fn original_template(state: &UiState, path: &str) -> Option<Arc<Vec<u8>>> {
    state.custom_templates.get(path).cloned().or_else(|| {
        state
            .quickapps
            .iter()
            .find(|entry| entry.declaration.destination == path)
            .and_then(|entry| entry.template.clone())
    })
}

fn draft_destination(draft: &CustomDraft) -> String {
    if draft.destination.is_empty() {
        runtime::destination(&draft.source)
    } else {
        draft.destination.clone()
    }
}

fn draft_asset_path(draft: &CustomDraft, destination: &str) -> Result<String, String> {
    if destination.ends_with('/') {
        if draft.asset_path.is_empty() {
            return Err("目录上传需要填写归档文件路径，例如 custom/icons/home.bin。".into());
        }
        crpack::validate_relative_path(&draft.asset_path)?;
        if !draft.asset_path.starts_with(destination) {
            return Err("目录文件路径必须位于规则的归档目标目录内。".into());
        }
        Ok(draft.asset_path.clone())
    } else {
        Ok(destination.into())
    }
}

fn validate_draft_budget(
    state: &UiState,
    bytes: usize,
    template: usize,
    path: &str,
) -> Result<(), String> {
    let previous = replacement_sizes_by_destination(state)
        .get(path)
        .copied()
        .unwrap_or(0);
    validate_icon_budget(bytes, replacement_bytes(state), previous, false)?;
    let previous_template = state
        .custom_templates
        .get(path)
        .map_or(0, |bytes| bytes.len());
    // Count both immutable originals and staged templates, not just output bytes.
    validate_icon_budget(template, template_bytes(state), previous_template, true)
}

fn save_rule_undo(state: &mut UiState, paths: &BTreeSet<String>) {
    state.rule_undo = Some(RuleUndo {
        mappings: state.mappings.clone(),
        assets: paths
            .iter()
            .map(|path| {
                (
                    path.clone(),
                    destination_bytes(state, path).map(<[u8]>::to_vec),
                )
            })
            .collect(),
        replacement_paths: state.replacements.keys().cloned().collect(),
        templates: state.custom_templates.clone(),
        authored_paths: state.authored_paths.clone(),
    });
}

fn path_referenced(state: &UiState, path: &str) -> bool {
    state.mappings.iter().any(|rule| mapping_covers(rule, path))
        || icon_destinations(state).contains(path)
}

fn remove_unreferenced_authored_assets(state: &mut UiState) {
    let unused = state
        .authored_paths
        .iter()
        .filter(|path| !state.imported_paths.contains(*path) && !path_referenced(state, path))
        .cloned()
        .collect::<Vec<_>>();
    for path in unused {
        state.replacements.remove(&path);
        state.custom_templates.remove(&path);
        state.authored_paths.remove(&path);
        state.thumbnail_cache.remove(&path);
    }
}

fn commit_custom_rule(state: &mut UiState) -> Result<(), String> {
    let draft = &state.custom_draft;
    let destination = draft_destination(draft);
    let mapping = Mapping {
        source: draft.source.clone(),
        destination: destination.clone(),
    };
    // Generic editor never accepts application pseudo-keys; they have dedicated cards.
    if !ordinary_mapping(&mapping) {
        return Err("第三方应用图标请使用专用图标编辑区；自定义规则需要普通绝对路径。".into());
    }
    crpack::validate_absolute_source(&mapping.source)?;
    validate_ui_mappings(std::slice::from_ref(&mapping))?;
    if state
        .mappings
        .iter()
        .enumerate()
        .any(|(index, existing)| Some(index) != draft.editing && existing.source == mapping.source)
    {
        return Err(
            "此来源路径已有规则；请编辑原规则或使用其他来源。现有规则和文件保持不变。".into(),
        );
    }
    if let Some(index) = draft.editing
        && state
            .mappings
            .get(index)
            .is_none_or(|rule| !ordinary_mapping(rule))
    {
        return Err("此规则已改变；请取消草稿后重新选择。".into());
    }
    let path = if draft.bytes.is_some()
        || draft.template.is_some()
        || (destination.ends_with('/') && !draft.asset_path.is_empty())
    {
        draft_asset_path(draft, &destination)?
    } else {
        destination.clone()
    };
    if !destination.ends_with('/')
        && draft.bytes.is_none()
        && destination_bytes(state, &destination).is_none()
    {
        return Err("请先选择 PNG/BIN 或原始文件，再添加规则。".into());
    }
    let bytes_size = draft.bytes.as_ref().map_or_else(
        || destination_bytes(state, &path).map_or(0, <[u8]>::len),
        |bytes| bytes.len(),
    );
    let template_size = draft.template.as_ref().map_or(0, |bytes| bytes.len());
    validate_draft_budget(state, bytes_size, template_size, &path)?;
    let draft = state.custom_draft.clone();
    let mut affected = draft
        .editing
        .and_then(|index| state.mappings.get(index))
        .map(|rule| {
            state
                .authored_paths
                .iter()
                .filter(|path| mapping_covers(rule, path))
                .cloned()
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    affected.insert(path.clone());
    save_rule_undo(state, &affected);
    if let Some(index) = draft.editing {
        state.mappings[index] = mapping;
    } else {
        state.mappings.push(mapping);
    }
    if let Some(bytes) = draft.bytes {
        // Shared app destinations and aliases always receive the same bytes.
        set_shared_icon_asset(state, &path, bytes.as_ref().clone(), false);
        state
            .replacements
            .insert(path.clone(), bytes.as_ref().clone());
        if !state.imported_paths.contains(&path) {
            state.authored_paths.insert(path.clone());
        }
    }
    if let Some(template) = draft.template {
        state.custom_templates.insert(path.clone(), template);
    } else if draft.template_disabled || !destination.ends_with('/') {
        state.custom_templates.remove(&path);
    }
    state.thumbnail_cache.clear();
    remove_unreferenced_authored_assets(state);
    state.custom_draft = CustomDraft::default();
    Ok(())
}

// Decode only when selecting/changing a draft asset. Snapshots and renders reuse
// the URI, so unrelated metadata edits and rendering a rule list never decode images.
fn cache_custom_preview(state: &mut UiState) {
    let destination = draft_destination(&state.custom_draft);
    let path = if destination.ends_with('/') {
        state.custom_draft.asset_path.as_str()
    } else {
        destination.as_str()
    };
    let bytes = state
        .custom_draft
        .bytes
        .as_ref()
        .map(|bytes| bytes.as_slice())
        .or_else(|| destination_bytes(state, path));
    let size = bytes.map(<[u8]>::len);
    let image = bytes.and_then(lvgl::inspect_image);
    let uri = bytes
        .filter(|_| image.is_some())
        .and_then(|bytes| preview_from_bytes(bytes).0);
    state.custom_draft.preview_uri = uri;
    state.custom_draft.displayed_size = size;
    state.custom_draft.displayed_image = image;
}

fn edit_custom_rule(state: &mut UiState, index: usize) {
    if let Some(rule) = state
        .mappings
        .get(index)
        .filter(|rule| ordinary_mapping(rule))
    {
        state.custom_draft = CustomDraft {
            source: rule.source.clone(),
            destination: rule.destination.clone(),
            editing: Some(index),
            advanced: true,
            template: original_template(state, &rule.destination),
            ..CustomDraft::default()
        };
        cache_custom_preview(state);
        state.status = "正在编辑草稿；保存前不会改变规则或共享文件。".into();
        state.error = None;
    }
}

fn edit_custom_member(state: &mut UiState, index: usize, path: &str) {
    if !state.mappings.get(index).is_some_and(|rule| {
        ordinary_mapping(rule) && rule.destination.ends_with('/') && mapping_covers(rule, path)
    }) {
        return;
    }
    edit_custom_rule(state, index);
    state.custom_draft.asset_path = path.into();
    state.custom_draft.template = original_template(state, path);
    cache_custom_preview(state);
}

fn custom_upload_path(draft: &CustomDraft, name: &str, input: &[u8], mode: CustomPick) -> String {
    let destination = draft_destination(draft);
    if !destination.ends_with('/') {
        return destination;
    }
    if !draft.asset_path.is_empty() {
        return draft.asset_path.clone();
    }
    let leaf = resource_file_name(name);
    let png = matches!(mode, CustomPick::Image)
        && (file_extension(&leaf).as_deref() == Some("png")
            || input.starts_with(b"\x89PNG\r\n\x1a\n"));
    let leaf = if png {
        png_file_name(&leaf).trim_end_matches(".png").to_string() + ".bin"
    } else {
        leaf
    };
    format!("{destination}{leaf}")
}

fn delete_custom_rule(state: &mut UiState, index: usize) {
    if state
        .mappings
        .get(index)
        .is_none_or(|rule| !ordinary_mapping(rule))
    {
        return;
    }
    let affected = state
        .authored_paths
        .iter()
        .filter(|path| mapping_covers(&state.mappings[index], path))
        .cloned()
        .collect();
    save_rule_undo(state, &affected);
    state.mappings.remove(index);
    remove_unreferenced_authored_assets(state);
    state.custom_draft = CustomDraft::default();
    state.status = "已删除规则；仅清理无人引用的自建文件。导入文件保留，可撤销。".into();
    state.error = None;
}

fn undo_custom_rule(state: &mut UiState) {
    let Some(undo) = state.rule_undo.take() else {
        return;
    };
    state.mappings = undo.mappings;
    for (path, bytes) in undo.assets {
        if let Some(bytes) = bytes {
            set_shared_icon_asset(state, &path, bytes.clone(), false);
            if undo.replacement_paths.contains(&path) {
                state.replacements.insert(path, bytes);
            } else {
                state.replacements.remove(&path);
            }
        } else {
            state.replacements.remove(&path);
            for entry in &mut state.quickapps {
                if entry.declaration.destination == path {
                    entry.asset = None;
                }
            }
            if state.canopus_destination == path {
                state.canopus = None;
            }
        }
    }
    state.custom_templates = undo.templates;
    state.authored_paths = undo.authored_paths;
    state.custom_draft = CustomDraft::default();
    state.thumbnail_cache.clear();
    state.status = "已撤销上一次规则修改。".into();
    state.error = None;
}

#[derive(Clone, Copy)]
enum CustomPick {
    Image,
    Raw,
    Template,
}

type CustomUpload = (Arc<Vec<u8>>, Option<Arc<Vec<u8>>>);

fn prepare_custom_upload(
    state: &UiState,
    input: &[u8],
    name: &str,
    mode: CustomPick,
) -> Result<CustomUpload, String> {
    validate_icon_budget(input.len(), 0, 0, matches!(mode, CustomPick::Template))?;
    let draft = &state.custom_draft;
    // Predict directory member paths before conversion, so imported originals
    // remain the default template even when PNG selection supplies the filename.
    let path = custom_upload_path(draft, name, input, mode);
    if matches!(mode, CustomPick::Template) {
        app_icons::inspect_bin(input)?;
        validate_icon_budget(
            input.len(),
            template_bytes(state),
            state
                .custom_templates
                .get(&path)
                .map_or(0, |bytes| bytes.len()),
            true,
        )?;
        return Ok((Arc::new(input.to_vec()), None));
    }
    let png =
        input.starts_with(b"\x89PNG\r\n\x1a\n") || file_extension(name).as_deref() == Some("png");
    if matches!(mode, CustomPick::Image) && !png {
        app_icons::inspect_bin(input)?;
    }
    let template = draft.template.clone().or_else(|| {
        if draft.template_disabled {
            None
        } else {
            original_template(state, &path)
        }
    });
    let encoded = runtime::encode(
        input,
        template.as_ref().map(|bytes| bytes.as_slice()),
        matches!(mode, CustomPick::Raw) || !png,
        state.allow_quantize,
        state.resize_filter,
    )?;
    let template = if template.is_some() {
        template
    } else if !draft.template_disabled
        && !png
        && !matches!(mode, CustomPick::Raw)
        && lvgl::inspect_image(input).is_some()
    {
        Some(Arc::new(input.to_vec()))
    } else {
        None
    };
    validate_draft_budget(
        state,
        encoded.bytes.len(),
        template.as_ref().map_or(0, |bytes| bytes.len()),
        &path,
    )?;
    Ok((Arc::new(encoded.bytes), template))
}

async fn begin_custom_pick(mode: CustomPick) {
    {
        let mut state = lock_state();
        state.busy = true;
        state.error = None;
        state.status = "等待选择文件；只更新草稿，保存后才生效。".into();
    }
    render_current();
    let picked = psys_host::dialog::pick_file(
        psys_host::dialog::PickConfig {
            read: true,
            copy_to: None,
        },
        psys_host::dialog::FilterConfig {
            multiple: false,
            extensions: match mode {
                CustomPick::Image => vec!["png".into(), "bin".into()],
                CustomPick::Template => vec!["bin".into()],
                CustomPick::Raw => Vec::new(),
            },
            default_directory: String::new(),
            default_file_name: String::new(),
        },
    )
    .await;
    let mut state = lock_state();
    state.busy = false;
    match picked {
        Ok(picked) if picked.name.is_empty() => {
            state.status = "已取消文件选择；草稿及现有编辑保持不变。".into();
        }
        Ok(picked) => {
            match prepare_custom_upload(&state, &picked.data, &picked.name, mode) {
                Ok((bytes, template)) => {
                    if matches!(mode, CustomPick::Template) {
                        state.custom_draft.template = Some(bytes);
                        state.custom_draft.template_disabled = false;
                        // A staged PNG was encoded with its old template. Require re-pick
                        // rather than presenting a template that did not produce the output.
                        state.custom_draft.bytes = None;
                        state.custom_draft.upload_name.clear();
                        state.status = "模板已暂存；请选择 PNG/BIN 后保存。模板本身不导出。".into();
                    } else {
                        state.custom_draft.bytes = Some(bytes);
                        state.custom_draft.template = template;
                        state.custom_draft.upload_name = picked.name.clone();
                        if state.custom_draft.destination.ends_with('/')
                            && state.custom_draft.asset_path.is_empty()
                        {
                            state.custom_draft.asset_path = custom_upload_path(
                                &state.custom_draft,
                                &picked.name,
                                &picked.data,
                                mode,
                            );
                        }
                        state.status = "文件已暂存；PNG 已转换，BIN/原始文件逐字节保留。点击添加/保存规则生效。".into();
                    }
                    cache_custom_preview(&mut state);
                    state.custom_draft.feedback = None;
                    state.error = None;
                }
                Err(error) => {
                    state.status = "文件未应用；草稿和现有文件保持不变。".into();
                    state.error = Some(error);
                }
            }
        }
        Err(error) => {
            state.status = "文件未应用；现有编辑保持不变。".into();
            state.error = Some(format!("无法选择文件：{error}"));
        }
    }
    drop(state);
    render_current();
}

fn parse_version_code(raw: &str) -> Result<Option<u64>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let code: u64 = trimmed
        .parse()
        .map_err(|_| "版本号 (versionCode) 必须是非负安全整数（0–9007199254740991）".to_string())?;
    crpack::validate_version_code(code)?;
    Ok(Some(code))
}

fn build_project_pack(state: &UiState) -> Result<Vec<u8>, String> {
    let (replacements, mappings, declarations) = export_assets(state)?;
    let version_code = parse_version_code(&state.version_code)?;
    crpack::build_crpack_with_mappings(
        &PackOptions {
            theme_id: &state.theme_id,
            name: &state.pack_name,
            version: (!state.version.is_empty()).then_some(state.version.as_str()),
            version_code,
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
        state.rule_undo = None;
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

fn resource_destination(state: &UiState, path: &str) -> String {
    let source = format!("/resource/{path}");
    for mapping in &state.mappings {
        if mapping.source == source {
            return mapping.destination.clone();
        }
        if mapping.source.ends_with('/') && source.starts_with(&mapping.source) {
            return format!("{}{}", mapping.destination, &source[mapping.source.len()..]);
        }
    }
    path.into()
}

fn resource_replacement<'a>(state: &'a UiState, path: &str) -> Option<&'a [u8]> {
    let source = format!("/resource/{path}");
    // An unmapped archive asset is not implicitly a firmware replacement.
    if !state.mappings.iter().any(|mapping| {
        mapping.source == source
            || (mapping.source.ends_with('/') && source.starts_with(&mapping.source))
    }) {
        return None;
    }
    destination_bytes(state, &resource_destination(state, path))
}

fn set_resource_replacement(state: &mut UiState, path: &str, bytes: Vec<u8>) {
    state.rule_undo = None;
    let destination = resource_destination(state, path);
    let source = format!("/resource/{path}");
    if !state.mappings.iter().any(|mapping| {
        mapping.source == source
            || (mapping.source.ends_with('/') && source.starts_with(&mapping.source))
    }) {
        state.mappings.push(Mapping {
            source,
            destination: destination.clone(),
        });
    }
    set_shared_icon_asset(state, &destination, bytes.clone(), false);
    state.replacements.insert(destination.clone(), bytes);
    if !state.imported_paths.contains(&destination) {
        state.authored_paths.insert(destination);
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
                state.rule_undo = None;
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
        let result = validate_icon_budget(picked.data.len(), 0, 0, false)
            .and_then(|()| {
                runtime::encode(
                    &picked.data,
                    Some(template.as_slice()),
                    false,
                    allow_quantize,
                    resize_filter,
                )
            })
            .and_then(|encoded| {
                let state = lock_state();
                let destination = resource_destination(&state, &path);
                let old_size = destination_bytes(&state, &destination).map_or(0, <[u8]>::len);
                validate_icon_budget(
                    encoded.bytes.len(),
                    replacement_bytes(&state),
                    old_size,
                    false,
                )?;
                Ok(encoded)
            });
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
            .saturating_sub(
                destination_bytes(&state, &resource_destination(&state, &path))
                    .map_or(0, <[u8]>::len),
            )
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
        let source = format!("/resource/{path}");
        let destination = resource_destination(&state, &path);
        if !state.authored_paths.contains(&destination)
            || !state.mappings.iter().any(|rule| rule.source == source)
        {
            state.error =
                Some("导入或目录规则请在自定义规则中编辑/删除；现有文件保持不变。".into());
            drop(state);
            render_current();
            return;
        }
        let affected = state.authored_paths.clone();
        save_rule_undo(&mut state, &affected);
        state.mappings.retain(|rule| rule.source != source);
        remove_unreferenced_authored_assets(&mut state);
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
        state.status = "已撤销此文件规则；共享文件保留。可在自定义规则中撤销本次操作。".into();
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
        let replacement = resource_replacement(&state, path);
        let bytes = if let Some(replacement) = replacement {
            lvgl::inspect_image(replacement).map(|_| Arc::new(replacement.to_vec()))
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
    if let Some(bytes) = resource_replacement(state, path) {
        return Ok(Arc::new(bytes.to_vec()));
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
        "corona".into()
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
            .child(text("编辑资源包", 20))
            .child(text(
                "直接添加运行时文件规则或第三方图标，无需固件。选择官方 OTA 固件可建立只读资源树，辅助查找原始资源和图片模板。",
                14,
            ))
            .child(button("选择手环固件", "firmware.upload", "solid", "accent").disabled_if(state.busy))
            .child(text("导入 CRPack 会替换当前项目；加载或更换固件保留全部规则与文件，原固件编辑作为待验证规则保留。", 12))
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
                "导入 CRPack 会替换当前项目；更换固件保留全部编辑，旧资源规则仍需设备验证。",
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

    root = root.child(build_custom_rules(&state));
    root = root.child(build_app_icons(&state));
    if state.firmware_name.is_empty() {
        root = root.child(build_inspector(&state));
    }
    root.child(build_status(&state))
}

fn build_custom_rules(state: &UiSnapshot) -> ui::Element {
    let draft = &state.custom_draft;
    let editing = draft.editing.is_some();
    let mut content = ui::Element::new(ui::ElementType::Card, None)
        .prop("variant", "surface").prop("size", "2").radius(24).padding(14)
        .flex().flex_direction(ui::FlexDirection::Column).width_full().gap(10)
        .child(
            ui::Element::new(ui::ElementType::Div, None).flex().align_center().gap(8)
                .child(text("自定义规则", 17).flex_grow(1.0))
                .child(badge(&format!("{} 条规则", state.custom_rules.len()), "gray"))
                .child(button("撤销上次修改", "custom.undo", "ghost", "gray").disabled_if(state.busy || !state.can_undo_rule)),
        )
        .child(text("填写设备上的绝对文件路径，选择文件，再添加规则。无需固件；路径是否存在仅能在设备运行时验证。", 13))
        .child(field("运行时来源路径", &draft.source, "custom.source", "/data/example/icon.bin"))
        .child(
            ui::Element::new(ui::ElementType::Grid, None)
                .grid_template_columns("repeat(auto-fit, minmax(min(100%, 150px), 1fr))").gap(8).width_full()
                .child(button("选择 PNG / BIN", "custom.pick.image", "soft", "accent").disabled_if(state.busy))
                .child(button("选择原始文件", "custom.pick.raw", "soft", "gray").disabled_if(state.busy))
                .child(button(if editing { "保存规则" } else { "添加规则" }, "custom.save", "solid", "accent").disabled_if(state.busy)),
        )
        .child(text("PNG 会转换为 LVGL BIN：新文件默认 v9 ARGB8888，保留原尺寸和透明度。BIN / 原始文件保持字节不变。", 12));
    if let Some(bytes) = &draft.bytes {
        content = content.child(badge(
            &format!(
                "草稿：{} · {}（尚未保存）",
                draft.upload_name,
                format_bytes(bytes.len())
            ),
            "accent",
        ));
    } else if editing {
        content = content.child(text(
            "未选择新文件时保留已有文件。编辑共享目标会同步所有引用。",
            12,
        ));
    }
    if let Some(feedback) = &draft.feedback {
        content = content.child(text(feedback, 13));
    }
    if let Some(size) = draft.displayed_size {
        if let Some(info) = draft.displayed_image {
            content = content.child(text(
                &format!(
                    "{} {}×{} · {} · {}",
                    if draft.bytes.is_some() {
                        "草稿输出"
                    } else {
                        "当前文件"
                    },
                    info.width,
                    info.height,
                    info.format.display_name(),
                    format_bytes(size)
                ),
                12,
            ));
            if let Some(uri) = &draft.preview_uri {
                content = content.child(
                    ui::Element::new(ui::ElementType::Div, None)
                        .width_full()
                        .height(220)
                        .flex()
                        .justify_center()
                        .align_center()
                        .child(
                            ui::Element::new(ui::ElementType::Image, Some(uri))
                                .max_width(280)
                                .max_height(220)
                                .prop("alt", &format!("自定义资源预览：{}", draft.source))
                                .prop("draggable", "false"),
                        ),
                );
            } else {
                content = content.child(text("此文件暂不可预览；原始文件字节仍会保留。", 12));
            }
        } else {
            content = content.child(text(
                &format!(
                    "原始文件 · {}（不作为图片转换，按原始字节保存）",
                    format_bytes(size)
                ),
                12,
            ));
        }
    }
    content = content.child(
        ui::Element::new(ui::ElementType::Div, None)
            .flex()
            .gap(8)
            .child(
                button(
                    if draft.advanced {
                        "收起高级选项"
                    } else {
                        "高级：归档目标 / 模板 / 目录"
                    },
                    "custom.advanced",
                    "ghost",
                    "gray",
                )
                .disabled_if(state.busy),
            )
            .child(button("取消草稿", "custom.cancel", "ghost", "gray").disabled_if(state.busy)),
    );
    if draft.advanced {
        content = content
            .child(field("归档目标（留空自动生成）", &draft.destination, "custom.destination", "custom/<hash>.bin"))
            .child(text("目录规则：来源与目标都以 / 结尾。下方填写每次上传的归档文件路径；同一目录规则可多次上传。规则按清单顺序保留。", 12))
            .child(field("目录内归档文件路径（单文件无需填写）", &draft.asset_path, "custom.asset_path", "custom/icons/home.bin"))
            .child(
                ui::Element::new(ui::ElementType::Div, None).flex().gap(8)
                    .child(button("选择原始 BIN 模板", "custom.pick.template", "soft", "gray").disabled_if(state.busy))
                    .child(button("不使用模板", "custom.template.clear", "ghost", "gray").disabled_if(state.busy || draft.template.is_none())),
            );
        content = content.child(if let Some(info) = draft.template.as_ref().and_then(|bytes| lvgl::inspect_image(bytes)) {
            text(&format!("原始模板 {}×{} · {}（仅转换，不导出）。选择模板后需重新选择 PNG。", info.width, info.height, info.format.display_name()), 12)
        } else { text("无模板：PNG 保留尺寸和透明度。导入的受支持 BIN 自动保留不可变原始模板；替换不会改变模板。", 12) });
        if custom_conversion_controls_visible(state) {
            content = content.child(build_template_conversion_controls(state));
        }
    }
    let mut list = ui::Element::new(ui::ElementType::ScrollArea, None)
        .prop("type", "auto")
        .prop("scrollbars", "vertical")
        .height(280)
        .width_full()
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .gap(8);
    if state.custom_rules.is_empty() && state.unmapped_assets.is_empty() {
        list = list.child(
            text(
                "还没有自定义规则。上方添加第一条文件规则，或导入已有 CRPack。",
                13,
            )
            .padding(12),
        );
    }
    for rule in &state.custom_rules {
        let mut row = ui::Element::new(ui::ElementType::Div, None)
            .flex()
            .flex_direction(ui::FlexDirection::Column)
            .gap(4)
            .width_full()
            .padding(8)
            .child(text(
                &format!("{} -> {}", rule.source, rule.destination),
                13,
            ))
            .child(text(
                &format!(
                    "{} · {} · {}",
                    if rule.source.ends_with('/') {
                        "目录规则"
                    } else {
                        "文件规则"
                    },
                    format_bytes(rule.size),
                    rule.verification
                ),
                12,
            ))
            .child(
                ui::Element::new(ui::ElementType::Div, None)
                    .flex()
                    .gap(8)
                    .child(
                        button(
                            "编辑",
                            &format!("custom.edit:{}", rule.index),
                            "soft",
                            "gray",
                        )
                        .disabled_if(state.busy),
                    )
                    .child(
                        button(
                            "删除规则",
                            &format!("custom.delete:{}", rule.index),
                            "ghost",
                            "gray",
                        )
                        .disabled_if(state.busy),
                    ),
            );
        for (path, size) in &rule.members {
            row = row.child(
                ui::Element::new(ui::ElementType::Div, None)
                    .flex()
                    .align_center()
                    .gap(8)
                    .child(text(&format!("{path} · {}", format_bytes(*size)), 12).flex_grow(1.0))
                    .child(
                        button(
                            "编辑文件",
                            &format!("custom.member:{}:{path}", rule.index),
                            "ghost",
                            "gray",
                        )
                        .disabled_if(state.busy),
                    ),
            );
        }
        list = list.child(row);
    }
    for (path, size) in &state.unmapped_assets {
        list = list.child(
            text(
                &format!(
                    "未映射文件：{path} · {}（保留导出，不自动生成规则）",
                    format_bytes(*size)
                ),
                12,
            )
            .padding(8),
        );
    }
    content.child(list)
}

fn custom_conversion_controls_visible(state: &UiSnapshot) -> bool {
    state.custom_draft.advanced && state.custom_draft.template.is_some()
}

fn build_template_conversion_controls(state: &UiSnapshot) -> ui::Element {
    let setting = |label: &str, handler: &str, checked: bool| {
        ui::Element::new(ui::ElementType::Div, None)
            .flex()
            .align_center()
            .gap(8)
            .child(
                ui::Element::new(ui::ElementType::Checkbox, None)
                    .prop("checked", if checked { "true" } else { "false" })
                    .prop("size", "2")
                    .on(ui::Event::Change, handler)
                    .disabled_if(state.busy),
            )
            .child(text(label, 13))
    };
    ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .gap(8)
        .child(text("模板 PNG 转换选项（与第三方图标共享设置）", 12))
        .child(setting(
            "启用平滑抗锯齿缩放（Lanczos3；关闭时为最近邻）",
            "pack.smooth_resize",
            state.resize_filter == lvgl::ResizeFilter::Lanczos3,
        ))
        .child(setting(
            "允许调色板超限时有损量化（默认关闭，会改变图片颜色）",
            "pack.quantize",
            state.allow_quantize,
        ))
        .child(text(
            "选项改变后请重新选择 PNG；不影响已保存文件或原始 BIN 模板。",
            12,
        ))
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
    if !canopus {
        actions = actions.child(
            button("选择原始 BIN 模板", &event("template"), "ghost", "gray").disabled_if(busy),
        );
    }
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
        .child(field("包标识", &state.theme_id, "pack.id", "corona"))
        .child(field(
            "包名称",
            &state.pack_name,
            "pack.name",
            "Corona Resource Pack",
        ))
        .child(field("版本名", &state.version, "pack.version", "1.0.0"))
        .child(field(
            "版本号（数字）",
            &state.version_code,
            "pack.version_code",
            "1",
        ))
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

    fn test_firmware(contents: &[u8]) -> FirmwareIndex {
        let mut data = vec![0u8; 128 + contents.len()];
        data[..8].copy_from_slice(b"-rom1fs-");
        let size = data.len() as u32;
        data[8..12].copy_from_slice(&size.to_be_bytes());
        data[16..24].copy_from_slice(b"resource");
        data[32..36].copy_from_slice(&1u32.to_be_bytes());
        data[36..40].copy_from_slice(&64u32.to_be_bytes());
        data[48..51].copy_from_slice(b"app");
        data[64..68].copy_from_slice(&1u32.to_be_bytes());
        data[68..72].copy_from_slice(&96u32.to_be_bytes());
        data[80..85].copy_from_slice(b"icons");
        data[96..100].copy_from_slice(&2u32.to_be_bytes());
        data[104..108].copy_from_slice(&(contents.len() as u32).to_be_bytes());
        data[112..120].copy_from_slice(b"test.bin");
        data[128..].copy_from_slice(contents);
        FirmwareIndex::from_firmware(data).unwrap()
    }

    fn transparent_bin() -> Vec<u8> {
        vec![
            0x19, 0x10, 0, 0, 2, 0, 1, 0, 8, 0, 0, 0, 33, 22, 11, 0, 90, 80, 70, 127,
        ]
    }

    fn stage_custom(state: &mut UiState, input: &[u8], name: &str, mode: CustomPick) {
        let (bytes, template) = prepare_custom_upload(state, input, name, mode).unwrap();
        state.custom_draft.bytes = Some(bytes);
        state.custom_draft.template = template;
        state.custom_draft.upload_name = name.into();
        if state.custom_draft.destination.ends_with('/') && state.custom_draft.asset_path.is_empty()
        {
            state.custom_draft.asset_path =
                custom_upload_path(&state.custom_draft, name, input, mode);
        }
        cache_custom_preview(state);
        state.custom_draft.feedback = None;
    }

    #[test]
    fn custom_template_controls_work_without_firmware_and_quantization_is_opt_in() {
        let mut state = UiState::default();
        assert!(!custom_conversion_controls_visible(&snapshot(&mut state)));
        let mut template = vec![0u8; 12 + 256 * 4 + 257];
        template[..12].copy_from_slice(&[0x19, 0x0a, 0, 0, 1, 1, 1, 0, 1, 1, 0, 0]);
        for entry in template[12..12 + 256 * 4].as_chunks_mut::<4>().0 {
            entry[3] = 255;
        }
        let mut colorful = vec![0x19, 0x10, 0, 0, 1, 1, 1, 0, 4, 4, 0, 0];
        for x in 0..257u16 {
            colorful.extend_from_slice(&[77, (x >> 8) as u8, x as u8, 255]);
        }
        let (_, png) = lvgl::decode_image_png(&colorful).unwrap();
        state.custom_draft = CustomDraft {
            source: "/system/palette.bin".into(),
            advanced: true,
            template: Some(Arc::new(template.clone())),
            ..CustomDraft::default()
        };
        assert!(state.firmware.is_none());
        assert!(!state.allow_quantize);
        assert!(custom_conversion_controls_visible(&snapshot(&mut state)));
        assert!(prepare_custom_upload(&state, &png, "colorful.png", CustomPick::Image).is_err());
        state.allow_quantize = true;
        stage_custom(&mut state, &png, "colorful.png", CustomPick::Image);
        assert!(state.custom_draft.preview_uri.is_some());
        commit_custom_rule(&mut state).unwrap();
        let parsed = parse_ui_pack(&build_project_pack(&state).unwrap()).unwrap();
        assert_eq!(
            lvgl::inspect_image(parsed.replacements.values().next().unwrap())
                .unwrap()
                .format,
            lvgl::ImageFormatKind::Lvgl9I8
        );
        assert_eq!(
            state.custom_templates.values().next().unwrap().as_slice(),
            template
        );
    }

    #[test]
    fn custom_draft_and_existing_image_preview_without_firmware_survive_failed_upload() {
        let mut state = UiState::default();
        state.custom_draft.source = "/data/image.bin".into();
        let original = transparent_bin();
        let (_, png) = lvgl::decode_image_png(&original).unwrap();
        stage_custom(&mut state, &png, "image.png", CustomPick::Image);
        let preview = state.custom_draft.preview_uri.clone().unwrap();
        assert!(preview.starts_with("data:image/png;base64,"));
        assert_eq!(state.custom_draft.displayed_size, Some(original.len()));
        assert_eq!(state.custom_draft.displayed_image.unwrap().width, 2);
        assert!(state.firmware.is_none());
        assert_eq!(
            snapshot(&mut state).custom_draft.preview_uri.as_deref(),
            Some(preview.as_str())
        );
        assert!(
            prepare_custom_upload(&state, b"invalid", "broken.png", CustomPick::Image).is_err()
        );
        assert_eq!(
            state.custom_draft.preview_uri.as_deref(),
            Some(preview.as_str())
        );
        commit_custom_rule(&mut state).unwrap();
        edit_custom_rule(&mut state, 0);
        assert!(state.custom_draft.bytes.is_none());
        assert_eq!(
            state.custom_draft.preview_uri.as_deref(),
            Some(preview.as_str())
        );
        assert!(
            prepare_custom_upload(&state, b"invalid", "broken.bin", CustomPick::Image).is_err()
        );
        assert_eq!(
            state.custom_draft.preview_uri.as_deref(),
            Some(preview.as_str())
        );
        stage_custom(&mut state, b"raw file", "raw.dat", CustomPick::Raw);
        assert!(state.custom_draft.preview_uri.is_none());
        assert!(state.custom_draft.displayed_image.is_none());
        assert_eq!(state.custom_draft.displayed_size, Some(8));
    }

    #[test]
    fn duplicate_source_add_and_edit_preserve_assets_and_previous_undo() {
        let mut state = UiState::default();
        state.custom_draft.source = "/data/first.bin".into();
        stage_custom(&mut state, b"first", "first.dat", CustomPick::Raw);
        commit_custom_rule(&mut state).unwrap();
        let first = export_assets(&state).unwrap();
        let undo_rules = state.rule_undo.as_ref().unwrap().mappings.clone();
        let undo_assets = state.rule_undo.as_ref().unwrap().assets.clone();
        state.custom_draft.source = "/data/first.bin".into();
        stage_custom(&mut state, b"overwrite", "other.dat", CustomPick::Raw);
        assert!(
            commit_custom_rule(&mut state)
                .unwrap_err()
                .contains("已有规则")
        );
        assert_eq!(export_assets(&state).unwrap(), first);
        assert_eq!(state.rule_undo.as_ref().unwrap().mappings, undo_rules);
        assert_eq!(state.rule_undo.as_ref().unwrap().assets, undo_assets);
        state.custom_draft = CustomDraft {
            source: "/data/second.bin".into(),
            ..CustomDraft::default()
        };
        stage_custom(&mut state, b"second", "second.dat", CustomPick::Raw);
        commit_custom_rule(&mut state).unwrap();
        let both = export_assets(&state).unwrap();
        let undo_rules = state.rule_undo.as_ref().unwrap().mappings.clone();
        let undo_assets = state.rule_undo.as_ref().unwrap().assets.clone();
        edit_custom_rule(&mut state, 1);
        state.custom_draft.source = "/data/first.bin".into();
        stage_custom(&mut state, b"overwrite", "other.dat", CustomPick::Raw);
        assert!(
            commit_custom_rule(&mut state)
                .unwrap_err()
                .contains("已有规则")
        );
        assert_eq!(export_assets(&state).unwrap(), both);
        assert_eq!(state.rule_undo.as_ref().unwrap().mappings, undo_rules);
        assert_eq!(state.rule_undo.as_ref().unwrap().assets, undo_assets);
        undo_custom_rule(&mut state);
        assert_eq!(export_assets(&state).unwrap(), first);
    }

    #[test]
    fn destination_and_member_change_events_invalidate_staged_file_with_inline_feedback() {
        let original = transparent_bin();
        let mut changed = original.clone();
        changed[15] = 255;
        let (_, png) = lvgl::decode_image_png(&original).unwrap();
        {
            let mut state = lock_state();
            state.replacements = BTreeMap::from([
                ("custom/first.bin".into(), original.clone()),
                ("custom/second.bin".into(), changed.clone()),
            ]);
            state.custom_templates = state
                .replacements
                .iter()
                .map(|(path, bytes)| (path.clone(), Arc::new(bytes.clone())))
                .collect();
            state.custom_draft = CustomDraft {
                source: "/data/image.bin".into(),
                destination: "custom/first.bin".into(),
                template: original_template(&state, "custom/first.bin"),
                ..CustomDraft::default()
            };
            stage_custom(&mut state, &png, "image.png", CustomPick::Image);
        }
        process_change("custom.destination", r#"{"value":"custom/second.bin"}"#);
        {
            let mut state = lock_state();
            assert!(state.custom_draft.bytes.is_none());
            assert!(state.custom_draft.upload_name.is_empty());
            assert!(
                state
                    .custom_draft
                    .feedback
                    .as_deref()
                    .unwrap()
                    .contains("重新选择 PNG")
            );
            assert_eq!(state.replacements["custom/first.bin"], original);
            assert_eq!(state.replacements["custom/second.bin"], changed);
            state.custom_draft = CustomDraft {
                source: "/data/icons/".into(),
                destination: "custom/".into(),
                asset_path: "custom/first.bin".into(),
                template: original_template(&state, "custom/first.bin"),
                ..CustomDraft::default()
            };
            stage_custom(&mut state, &png, "image.png", CustomPick::Image);
        }
        process_change("custom.asset_path", r#"{"value":"custom/second.bin"}"#);
        {
            let mut state = lock_state();
            assert!(state.custom_draft.bytes.is_none());
            assert!(state.custom_draft.feedback.is_some());
            assert_eq!(state.replacements["custom/first.bin"], original);
            assert_eq!(state.replacements["custom/second.bin"], changed);
            state.custom_draft.asset_path = "custom/first.bin".into();
            state.custom_draft.template = original_template(&state, "custom/first.bin");
            stage_custom(&mut state, &png, "image.png", CustomPick::Image);
            // The bool feeds process_change's immediate render branch.
            assert!(change_custom_target(
                &mut state,
                "custom/second.bin".into(),
                true
            ));
            state.custom_draft = CustomDraft::default();
            state.custom_templates.clear();
            state.replacements.clear();
        }
    }

    #[test]
    fn imported_directory_members_are_visible_and_use_immutable_original_templates() {
        let original = transparent_bin();
        let destination = "custom/icons/transparent.bin";
        let mut state = UiState::default();
        import_assets(
            &mut state,
            BTreeMap::from([(destination.into(), original.clone())]),
            vec![Mapping {
                source: "/data/icons/".into(),
                destination: "custom/icons/".into(),
            }],
            vec![],
        );
        let snap = snapshot(&mut state);
        assert_eq!(
            snap.custom_rules[0].members,
            vec![(destination.into(), original.len())]
        );
        edit_custom_member(&mut state, 0, destination);
        assert_eq!(state.custom_draft.asset_path, destination);
        assert_eq!(
            state.custom_draft.template.as_ref().unwrap().as_slice(),
            original
        );
        edit_custom_rule(&mut state, 0);
        assert!(state.custom_draft.asset_path.is_empty());
        let (_, png) = lvgl::decode_image_png(&original).unwrap();
        stage_custom(&mut state, &png, "transparent.png", CustomPick::Image);
        assert_eq!(state.custom_draft.asset_path, destination);
        assert_eq!(
            state.custom_draft.template.as_ref().unwrap().as_slice(),
            original
        );
        commit_custom_rule(&mut state).unwrap();
        assert_eq!(state.replacements.len(), 1);
        assert_eq!(state.custom_templates[destination].as_slice(), original);
        assert_eq!(state.mappings.len(), 1);
    }

    #[test]
    fn custom_png_without_firmware_exports_v9_native_dimensions_and_alpha() {
        let original = transparent_bin();
        let (_, png) = lvgl::decode_image_png(&original).unwrap();
        let mut state = UiState::default();
        state.custom_draft.source = "/data/my-app/transparent.bin".into();
        stage_custom(&mut state, &png, "transparent.png", CustomPick::Image);
        assert!(state.replacements.is_empty());
        assert!(state.mappings.is_empty());
        assert!(state.custom_draft.template.is_none());
        commit_custom_rule(&mut state).unwrap();
        assert!(state.firmware.is_none());
        let destination = runtime::destination("/data/my-app/transparent.bin");
        let parsed = parse_ui_pack(&build_project_pack(&state).unwrap()).unwrap();
        assert_eq!(
            parsed.mappings,
            vec![Mapping {
                source: "/data/my-app/transparent.bin".into(),
                destination: destination.clone()
            }]
        );
        let info = lvgl::inspect_image(&parsed.replacements[&destination]).unwrap();
        assert_eq!(info.format, lvgl::ImageFormatKind::Lvgl9Argb8888);
        assert_eq!((info.width, info.height), (2, 1));
        assert_eq!(parsed.replacements[&destination], original);
        let snap = snapshot(&mut state);
        assert_eq!(snap.replacement_count, 1);
        assert_eq!(snap.custom_rules.len(), 1);
        assert_eq!(snap.firmware_replacement_count, 0);
    }

    #[test]
    fn imported_bin_original_template_survives_later_png_and_bin_updates() {
        let original = transparent_bin();
        let mut state = UiState::default();
        let destination = "custom/original.bin".to_string();
        import_assets(
            &mut state,
            BTreeMap::from([(destination.clone(), original.clone())]),
            vec![Mapping {
                source: "/data/app/icon.bin".into(),
                destination: destination.clone(),
            }],
            vec![],
        );
        let stored = state.custom_templates[&destination].clone();
        edit_custom_rule(&mut state, 0);
        let mut changed = original.clone();
        changed[15] = 255;
        stage_custom(&mut state, &changed, "changed.bin", CustomPick::Image);
        commit_custom_rule(&mut state).unwrap();
        assert_eq!(state.replacements[&destination], changed);
        assert_eq!(state.custom_templates[&destination].as_slice(), original);
        assert!(Arc::ptr_eq(&stored, &state.custom_templates[&destination]));
        edit_custom_rule(&mut state, 0);
        let (_, png) = lvgl::decode_image_png(&changed).unwrap();
        stage_custom(&mut state, &png, "changed.png", CustomPick::Image);
        commit_custom_rule(&mut state).unwrap();
        assert!(Arc::ptr_eq(&stored, &state.custom_templates[&destination]));
        let parsed = parse_ui_pack(&build_project_pack(&state).unwrap()).unwrap();
        assert_eq!(parsed.replacements[&destination], changed);
        assert_eq!(parsed.replacements.len(), 1);
    }

    #[test]
    fn generic_aliases_directories_and_unmapped_raw_assets_round_trip_in_order() {
        let rules = vec![
            Mapping {
                source: "/data/first.bin".into(),
                destination: "shared.bin".into(),
            },
            Mapping {
                source: "/system/fonts/".into(),
                destination: "fonts/".into(),
            },
            Mapping {
                source: "/resource/not-in-firmware.bin".into(),
                destination: "shared.bin".into(),
            },
            Mapping {
                source: "/data/alias.bin".into(),
                destination: "shared.bin".into(),
            },
        ];
        let assets = BTreeMap::from([
            ("shared.bin".into(), vec![0, 255, 128]),
            ("fonts/nested/font.dat".into(), vec![1, 0, 2, 0]),
            ("unused/opaque.bin".into(), vec![9, 8]),
        ]);
        let mut state = UiState::default();
        import_assets(&mut state, assets.clone(), rules.clone(), vec![]);
        state.firmware = Some(test_firmware(b"firmware"));
        let snap = snapshot(&mut state);
        assert_eq!(snap.custom_rules.len(), 4);
        assert_eq!(snap.unmapped_assets, vec![("unused/opaque.bin".into(), 2)]);
        assert!(snap.custom_rules[2].verification.contains("未验证"));
        assert!(!import_confirmation(&state).contains("不兼容"));
        let parsed = parse_ui_pack(&build_project_pack(&state).unwrap()).unwrap();
        assert_eq!(parsed.mappings, rules);
        assert_eq!(parsed.replacements, assets);
        // A directory edit can stage an arbitrary member, without decoding it.
        edit_custom_rule(&mut state, 1);
        state.custom_draft.asset_path = "fonts/new/raw.bin".into();
        stage_custom(
            &mut state,
            b"\x89PNG\r\n\x1a\nopaque",
            "raw.bin",
            CustomPick::Raw,
        );
        commit_custom_rule(&mut state).unwrap();
        assert_eq!(state.mappings, rules);
        assert_eq!(
            state.replacements["fonts/new/raw.bin"],
            b"\x89PNG\r\n\x1a\nopaque"
        );
    }

    #[test]
    fn failed_custom_edits_and_uploads_preserve_rules_bytes_templates_and_undo() {
        let mut state = UiState::default();
        state.custom_draft.source = "/system/file.bin".into();
        stage_custom(
            &mut state,
            &transparent_bin(),
            "valid.bin",
            CustomPick::Image,
        );
        commit_custom_rule(&mut state).unwrap();
        let before = export_assets(&state).unwrap();
        let templates = state.custom_templates.clone();
        edit_custom_rule(&mut state, 0);
        assert!(
            prepare_custom_upload(&state, b"invalid PNG", "invalid.png", CustomPick::Image)
                .is_err()
        );
        assert!(
            prepare_custom_upload(&state, b"not a BIN", "invalid.bin", CustomPick::Image).is_err()
        );
        state.custom_draft.source = "/system/../escape.bin".into();
        state.custom_draft.bytes = Some(Arc::new(vec![7, 8]));
        assert!(commit_custom_rule(&mut state).is_err());
        assert_eq!(export_assets(&state).unwrap(), before);
        assert_eq!(state.custom_templates, templates);
        assert!(state.rule_undo.is_some());
        state.custom_draft.source = "/system/file.bin".into();
        state.custom_draft.destination = "../unsafe.bin".into();
        assert!(commit_custom_rule(&mut state).is_err());
        state.custom_draft = CustomDraft::default();
        assert_eq!(export_assets(&state).unwrap(), before);
        assert_eq!(state.custom_templates, templates);
    }

    #[test]
    fn rule_delete_cleans_only_unreferenced_authored_files_and_is_undoable() {
        let mut state = UiState::default();
        state.custom_draft.source = "/data/first.bin".into();
        stage_custom(&mut state, b"opaque", "any.file", CustomPick::Raw);
        commit_custom_rule(&mut state).unwrap();
        let destination = state.mappings[0].destination.clone();
        state.custom_draft = CustomDraft {
            source: "/data/alias.bin".into(),
            destination: destination.clone(),
            ..CustomDraft::default()
        };
        commit_custom_rule(&mut state).unwrap();
        delete_custom_rule(&mut state, 0);
        assert_eq!(state.replacements[&destination], b"opaque");
        delete_custom_rule(&mut state, 0);
        assert!(state.replacements.is_empty());
        undo_custom_rule(&mut state);
        assert_eq!(state.replacements[&destination], b"opaque");
        assert_eq!(state.mappings[0].source, "/data/alias.bin");
        state.imported_paths.insert(destination.clone());
        delete_custom_rule(&mut state, 0);
        assert_eq!(state.replacements[&destination], b"opaque");
        assert!(state.mappings.is_empty());
        let parsed = parse_ui_pack(&build_project_pack(&state).unwrap()).unwrap();
        assert!(parsed.mappings.is_empty());
    }

    #[test]
    fn generic_shared_application_destinations_update_aliases_and_undo_consistently() {
        let original = transparent_bin();
        let destination = "custom/shared.bin".to_string();
        let mut state = UiState::default();
        let rules = vec![
            Mapping {
                source: "/data/icon.bin".into(),
                destination: destination.clone(),
            },
            Mapping {
                source: "/system/alias.bin".into(),
                destination: destination.clone(),
            },
        ];
        import_assets(
            &mut state,
            BTreeMap::from([(destination.clone(), original.clone())]),
            rules.clone(),
            vec![QuickappIcon {
                package: "my-app".into(),
                destination: destination.clone(),
            }],
        );
        edit_custom_rule(&mut state, 0);
        let mut changed = original.clone();
        changed[15] = 255;
        stage_custom(&mut state, &changed, "new.bin", CustomPick::Image);
        commit_custom_rule(&mut state).unwrap();
        assert_eq!(
            state.quickapps[0].asset.as_ref().unwrap().bytes.as_slice(),
            changed
        );
        assert_eq!(state.replacements[&destination], changed);
        assert_eq!(state.mappings, rules);
        undo_custom_rule(&mut state);
        assert_eq!(
            state.quickapps[0].asset.as_ref().unwrap().bytes.as_slice(),
            original
        );
        assert_eq!(state.replacements[&destination], original);
        assert_eq!(state.mappings, rules);
    }

    #[test]
    fn resource_alias_browser_resolves_by_source_never_firmware_destination() {
        let mut state = UiState::default();
        let replacement = transparent_bin();
        state.firmware = Some(test_firmware(b"firmware original"));
        import_assets(
            &mut state,
            BTreeMap::from([("custom/alias.bin".into(), replacement.clone())]),
            vec![Mapping {
                source: "/resource/app/icons/test.bin".into(),
                destination: "custom/alias.bin".into(),
            }],
            vec![],
        );
        state.filter_mode = ResourceFilter::Replaced;
        state.selected_path = Some("app/icons/test.bin".into());
        let snap = snapshot(&mut state);
        assert_eq!(snap.entries.len(), 1);
        assert_eq!(snap.entries[0].path, "app/icons/test.bin");
        assert_eq!(snap.selected_size, Some(replacement.len()));
        assert!(snap.selected_replaced);
        assert!(snap.custom_rules[0].verification.contains("当前固件"));
        assert_eq!(
            resource_bytes(&state, "app/icons/test.bin")
                .unwrap()
                .as_slice(),
            replacement
        );
        set_resource_replacement(&mut state, "app/icons/test.bin", vec![42]);
        assert_eq!(state.replacements["custom/alias.bin"], vec![42]);
        assert_eq!(state.mappings.len(), 1);
    }

    #[test]
    fn custom_editor_rejects_application_keys_and_tracks_all_template_budgets() {
        let mut state = UiState::default();
        state.custom_draft.source = "@quickapp-icon/app".into();
        state.custom_draft.bytes = Some(Arc::new(vec![1]));
        assert!(commit_custom_rule(&mut state).is_err());
        state.custom_draft.source = app_icons::CANOPUS_SOURCE.into();
        assert!(commit_custom_rule(&mut state).is_err());
        assert!(state.mappings.is_empty());
        state
            .custom_templates
            .insert("original.bin".into(), Arc::new(vec![0; 32]));
        state.quickapps.push(quickapp("app", false));
        state.quickapps[0].template = Some(Arc::new(vec![0; 64]));
        assert_eq!(template_bytes(&state), 96);
        assert!(validate_draft_budget(&state, 1, MAX_ICON_BYTES - 64, "original.bin").is_ok());
        assert!(validate_draft_budget(&state, 1, MAX_ICON_BYTES - 63, "original.bin").is_err());
        assert!(validate_draft_budget(&state, MAX_ICON_BYTES + 1, 0, "other.bin").is_err());
    }

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
        assert!(import_confirmation(&state).contains("共 2 项替换（规则/文件 0，第三方图标 2）"));
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
    fn ordinary_custom_mappings_import_and_export_without_firmware() {
        let replacements = BTreeMap::from([("custom.bin".into(), vec![1, 2, 3])]);
        let mapping = Mapping {
            source: "/data/custom/icon.bin".into(),
            destination: "custom.bin".into(),
        };
        let package = crpack::build_crpack_with_icons(
            &PackOptions {
                theme_id: "corona",
                name: "Custom",
                version: None,
                version_code: None,
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
        let imported = parse_ui_pack(&package).unwrap();
        let mut state = UiState::default();
        import_assets(
            &mut state,
            imported.replacements,
            imported.mappings,
            imported.quickapp_icons,
        );
        let snap = snapshot(&mut state);
        assert_eq!(snap.custom_rules.len(), 1);
        assert_eq!(snap.custom_rules[0].source, mapping.source);
        assert!(snap.custom_rules[0].verification.contains("未验证"));
        assert_eq!(snap.replacement_count, 1);
        let exported = parse_ui_pack(&build_project_pack(&state).unwrap()).unwrap();
        assert_eq!(exported.mappings, vec![mapping]);
        assert_eq!(exported.replacements, replacements);
        state.replacements.clear();
        // A missing target remains visible and in the rule list; core validation errors.
        assert_eq!(export_assets(&state).unwrap().1.len(), 1);
        assert!(build_project_pack(&state).is_err());
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
                theme_id: "corona",
                name: "Icons",
                version: None,
                version_code: None,
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
    fn firmware_workspace_reset_retains_all_assets_templates_and_mappings() {
        let mut state = UiState::default();
        let bytes = app_icons::canopus_template();
        set_resource_replacement(&mut state, "image.bin", bytes.clone());
        state.canopus = Some(IconAsset::new(bytes));
        state.quickapps.push(quickapp("ng.example.app", true));
        state.mappings.push(Mapping {
            source: app_icons::CANOPUS_SOURCE.into(),
            destination: state.canopus_destination.clone(),
        });
        reset_firmware_resource_edits(&mut state);
        assert_eq!(state.replacements.len(), 1);
        assert_eq!(replacement_count(&state), 3);
        assert!(state.quickapps[0].template.is_some());
        assert_eq!(state.mappings.len(), 2);
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
        assert_eq!(snap.firmware_replacement_count, 0);
        assert_eq!(snap.custom_rules.len(), 1);
        assert_eq!(snap.custom_rules[0].source, "/resource/original.bin");
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
        state
            .replacements
            .insert("icons/unreferenced.bin".into(), vec![9]);
        assert_eq!(export_assets(&state).unwrap().1, vec![mapped.clone()]);
        set_resource_replacement(&mut state, "icons/a.bin", vec![3]);
        set_resource_replacement(&mut state, "icons/b.bin", vec![4]);
        let edited = parse_ui_pack(&build_project_pack(&state).unwrap()).unwrap();
        assert_eq!(edited.replacements.len(), 5);
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
    fn firmware_first_attach_and_reload_retain_imported_and_authored_rules() {
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
        state.firmware = Some(test_firmware(b"first"));
        reset_firmware_resource_edits(&mut state);
        assert_eq!(state.mappings.len(), 2);
        assert!(state.imported_paths.contains("image.bin"));
        assert_eq!(state.replacements["image.bin"], vec![1]);
        set_resource_replacement(&mut state, "app/icons/test.bin", vec![2]);
        state.custom_draft = CustomDraft {
            source: "/system/custom.bin".into(),
            bytes: Some(Arc::new(vec![3])),
            ..CustomDraft::default()
        };
        commit_custom_rule(&mut state).unwrap();
        let before = export_assets(&state).unwrap();
        state.firmware = Some(test_firmware(b"second"));
        reset_firmware_resource_edits(&mut state);
        let after = export_assets(&state).unwrap();
        assert_eq!(before, after);
        let unpacked = parse_ui_pack(&build_project_pack(&state).unwrap()).unwrap();
        assert!(unpacked.mappings.contains(&canopus));
        assert!(
            unpacked
                .mappings
                .iter()
                .any(|rule| rule.source == "/resource/stale.bin")
        );
        assert!(
            unpacked
                .mappings
                .iter()
                .any(|rule| rule.source == "/system/custom.bin")
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

    #[test]
    fn editor_default_builds_pack_with_default_version_code() {
        let mut state = UiState::default();
        state
            .replacements
            .insert("app/icon.bin".into(), vec![1, 2, 3]);
        let bytes = build_project_pack(&state).unwrap();
        let unpacked = parse_ui_pack(&bytes).unwrap();
        assert_eq!(unpacked.version.as_deref(), Some("1.0.0"));
        assert_eq!(unpacked.version_code, Some(1));
    }

    #[test]
    fn editor_version_code_change_and_validation() {
        let mut state = UiState::default();
        state
            .replacements
            .insert("app/icon.bin".into(), vec![1, 2, 3]);

        // Custom valid integer
        state.version_code = "42".into();
        let bytes = build_project_pack(&state).unwrap();
        let unpacked = parse_ui_pack(&bytes).unwrap();
        assert_eq!(unpacked.version_code, Some(42));

        // Empty string -> None
        state.version_code = "  ".into();
        let bytes = build_project_pack(&state).unwrap();
        let unpacked = parse_ui_pack(&bytes).unwrap();
        assert_eq!(unpacked.version_code, None);

        // Invalid cases
        for invalid in ["-1", "abc", "1.5", "9007199254740992"] {
            state.version_code = invalid.into();
            assert!(
                build_project_pack(&state).is_err(),
                "should reject invalid versionCode: {invalid}"
            );
        }
    }

    #[test]
    fn editor_import_restores_version_code() {
        let replacements = BTreeMap::from([("app/icon.bin".into(), vec![1, 2, 3])]);
        let pack = crpack::build_crpack(&PackOptions {
            theme_id: "corona",
            name: "Test",
            version: Some("2.0.0"),
            version_code: Some(123),
            author: None,
            description: None,
            target: None,
            replacements: &replacements,
        })
        .unwrap();

        let unpacked = parse_ui_pack(&pack).unwrap();
        let mut state = UiState::default();
        state.theme_id = unpacked.theme_id;
        state.pack_name = unpacked.name;
        state.version = unpacked.version.unwrap_or_default();
        state.version_code = unpacked
            .version_code
            .map(|v| v.to_string())
            .unwrap_or_default();
        assert_eq!(state.version, "2.0.0");
        assert_eq!(state.version_code, "123");
    }
}
