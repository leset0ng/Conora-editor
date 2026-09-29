use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

use astrobox_ng_wit::astrobox::psys_host_v4::{self as psys_host, ui};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::Value;

use crate::crpack::{self, PackOptions};
use crate::firmware::{BrowserEntry, FirmwareIndex};
use crate::lvgl;

const MAX_VISIBLE_ENTRIES: usize = 300;
const MAX_SEARCH_RESULTS: usize = 200;
const SAVE_CHUNK_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResourceFilter {
    All,
    Images,
    Replaced,
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
    replacements: BTreeMap<String, Vec<u8>>,
    theme_id: String,
    pack_name: String,
    version: String,
    author: String,
    description: String,
    target: String,
    allow_quantize: bool,
    busy: bool,
    status: String,
    error: Option<String>,
}

static UI_STATE: OnceLock<Mutex<UiState>> = OnceLock::new();

fn ui_state() -> &'static Mutex<UiState> {
    UI_STATE.get_or_init(|| {
        Mutex::new(UiState {
            root_element_id: None,
            firmware_name: String::new(),
            firmware: None,
            current_dir: String::new(),
            search_query: String::new(),
            filter_mode: ResourceFilter::All,
            selected_path: None,
            preview_uri: None,
            replacements: BTreeMap::new(),
            theme_id: "conora".into(),
            pack_name: "Conora Resource Pack".into(),
            version: "1.0.0".into(),
            author: String::new(),
            description: String::new(),
            target: String::new(),
            allow_quantize: false,
            busy: false,
            status: "选择手环固件以开始浏览资源。".into(),
            error: None,
        })
    })
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
    hidden_entries: usize,
    selected_path: Option<String>,
    selected_size: Option<usize>,
    selected_image: Option<lvgl::I8Info>,
    selected_template_image: bool,
    selected_replaced: bool,
    preview_uri: Option<String>,
    replacement_count: usize,
    replacement_sizes: BTreeMap<String, usize>,
    theme_id: String,
    pack_name: String,
    version: String,
    author: String,
    description: String,
    target: String,
    allow_quantize: bool,
    busy: bool,
    status: String,
    error: Option<String>,
}

fn snapshot(state: &UiState) -> UiSnapshot {
    let (file_count, image_count, entries, hidden_entries) = if let Some(firmware) = &state.firmware {
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
                    let name = path
                        .rsplit('/')
                        .next()
                        .unwrap_or(path.as_str())
                        .to_string();
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
                list.sort_by(|left, right| left.path.to_lowercase().cmp(&right.path.to_lowercase()));
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
                            entry.has_replacements = state
                                .replacements
                                .keys()
                                .any(|k| k.starts_with(&prefix));
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
                .and_then(|bytes| lvgl::inspect_i8(bytes))
        } else {
            selected.and_then(|file| file.image)
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
        hidden_entries,
        selected_path: state.selected_path.clone(),
        selected_size,
        selected_image,
        selected_template_image,
        selected_replaced,
        preview_uri: state.preview_uri.clone(),
        replacement_count: state.replacements.len(),
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
                state.status = "仅显示 LVGL 图片资源。".into();
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
        "replace.png" => begin_png_pick().await,
        "replace.binary" => begin_binary_pick().await,
        "replace.restore" => restore_selected(),
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
                state.current_dir.clear();
                state.search_query.clear();
                state.filter_mode = ResourceFilter::All;
                state.selected_path = None;
                state.preview_uri = None;
                state.replacements.clear();
                if !target.is_empty() {
                    state.target = target;
                }
                state.busy = false;
                state.status =
                    format!("固件已解包，发现 {count} 个资源文件。选择文件查看预览或替换。");
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
        if state.firmware.is_none() {
            state.error = Some("请先加载手环固件再导入 CRPack。".into());
            drop(state);
            render_current();
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

    let result = crpack::parse_crpack(&picked.data);
    let selected_refresh_path = match result {
        Ok(unpacked) => {
            let mut state = lock_state();
            state.theme_id = unpacked.theme_id;
            state.pack_name = unpacked.name;
            state.version = unpacked.version.unwrap_or_default();
            state.author = unpacked.author.unwrap_or_default();
            state.description = unpacked.description.unwrap_or_default();
            if let Some(target) = unpacked.target {
                if !target.is_empty() {
                    state.target = target;
                }
            }

            // Strategy B: completely reset existing replacements and use the imported ones
            state.replacements = unpacked.replacements;
            state.filter_mode = ResourceFilter::Replaced;
            state.search_query.clear();
            state.busy = false;

            let count = state.replacements.len();
            let missing_in_firmware_count = state.firmware.as_ref().map_or(0, |fw| {
                state
                    .replacements
                    .keys()
                    .filter(|path| fw.file(path).is_none())
                    .count()
            });

            if missing_in_firmware_count > 0 {
                state.status = format!(
                    "已导入「{}」，包含 {} 个资源（其中 {} 个在当前固件中未找到）。",
                    state.pack_name, count, missing_in_firmware_count
                );
            } else {
                state.status = format!("已导入「{}」，包含 {} 个资源。", state.pack_name, count);
            }
            state.error = None;

            state.selected_path.clone()
        }
        Err(error) => {
            let mut state = lock_state();
            state.busy = false;
            state.status = "CRPack 未能导入。".into();
            state.error = Some(format!("CRPack 解析失败：{error}"));
            None
        }
    };

    if let Some(path) = selected_refresh_path {
        select_file(&path);
    } else {
        render_current();
    }
}

async fn begin_png_pick() {
    let (path, template, allow_quantize) = {
        let mut state = lock_state();
        if state.busy {
            return;
        }
        let Some(path) = state.selected_path.clone() else {
            state.error = Some("请先从文件树选择一个 LVGL 图片。".into());
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
        if lvgl::inspect_i8(template.as_slice()).is_none() {
            state.error = Some("此文件不是受支持的 LVGL v9 I8 图片。".into());
            drop(state);
            render_current();
            return;
        }
        state.busy = true;
        state.status = "等待选择 PNG 图片…".into();
        state.error = None;
        (path, template, state.allow_quantize)
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
        let result =
            lvgl::encode_png_i8_detailed(&picked.data, template.as_slice(), allow_quantize);
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
                state.replacements.insert(path.clone(), encoded.bytes);
                state.selected_path = Some(path.clone());
                state.preview_uri = preview_uri;
                state.busy = false;
                state.status = format!(
                    "已将 PNG 转换为 LVGL I8 BIN（{}{quantize_status}）。",
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
        let new_total = state
            .replacements
            .values()
            .map(Vec::len)
            .sum::<usize>()
            .saturating_sub(state.replacements.get(&path).map_or(0, Vec::len))
            .saturating_add(picked_size);
        if new_total > 64 * 1024 * 1024 {
            state.busy = false;
            state.status = "替换文件没有应用。".into();
            state.error = Some("替换资源总大小不能超过 CRPack v1 的 64 MiB 上限。".into());
        } else {
            let (preview_uri, preview_error) = preview_from_bytes(&picked.data);
            state.replacements.insert(path.clone(), picked.data);
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
        if state.replacements.is_empty() {
            state.error = Some("请先替换至少一个固件资源。".into());
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
        crpack::build_crpack(&PackOptions {
            theme_id: &state.theme_id,
            name: &state.pack_name,
            version: (!state.version.is_empty()).then_some(state.version.as_str()),
            author: (!state.author.is_empty()).then_some(state.author.as_str()),
            description: (!state.description.is_empty()).then_some(state.description.as_str()),
            target: (!state.target.is_empty()).then_some(state.target.as_str()),
            replacements: &state.replacements,
        })
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
    if as_png && lvgl::inspect_i8(bytes.as_slice()).is_none() {
        let mut state = lock_state();
        state.busy = false;
        state.status = "资源提取未完成。".into();
        state.error = Some("此资源不是受支持的 LVGL v9 I8 图片，无法转换为 PNG。".into());
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
        match lvgl::decode_i8_png(bytes.as_slice()) {
            Ok((_, png)) => save_resource_file(&png, &file_name, extension.as_deref())
                .await
                .map(|()| png.len()),
            Err(error) => Err(format!("BIN 转 PNG 失败：{error}")),
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
        let state = lock_state();
        (
            state.root_element_id.clone(),
            build_main_ui(snapshot(&state)),
        )
    };
    if let Some(root_id) = root_id {
        psys_host::ui::render(&root_id, tree);
    }
}

pub fn render_main_ui(element_id: &str) {
    let tree = {
        let mut state = lock_state();
        state.root_element_id = Some(element_id.to_string());
        build_main_ui(snapshot(&state))
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
            .child(button("选择手环固件", "firmware.upload", "solid", "accent"));
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
        .disabled_if(state.busy);

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
        .disabled_if(state.busy);

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
        .disabled_if(state.busy || state.replacement_count == 0);

        let actions = ui::Element::new(ui::ElementType::Div, None)
            .flex()
            .flex_direction(ui::FlexDirection::Row)
            .align_center()
            .gap(8)
            .flex_shrink(0.0)
            .child(upload)
            .child(import)
            .child(export);

        let summary = ui::Element::new(ui::ElementType::Div, None)
            .flex()
            .flex_direction(ui::FlexDirection::Row)
            .flex_grow(1.0)
            .align_center()
            .gap(8)
            .child(span("📦", 14))
            .child(span(&state.firmware_name, 14))
            .child(badge(&format!("{} 个文件", state.file_count), "gray"))
            .child(badge(
                &format!("已替换 {}", state.replacement_count),
                if state.replacement_count > 0 {
                    "accent"
                } else {
                    "gray"
                },
            ));

        let toolbar = ui::Element::new(ui::ElementType::Div, None)
            .flex()
            .flex_direction(ui::FlexDirection::Row)
            .width_full()
            .align_center()
            .child(summary)
            .child(actions);

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

    root.child(build_status(&state))
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
                &format!("已替换 {}", state.replacement_count),
                "browser.filter:replaced",
                if state.filter_mode == ResourceFilter::Replaced {
                    "solid"
                } else if state.replacement_count > 0 {
                    "soft"
                } else {
                    "ghost"
                },
                if state.filter_mode == ResourceFilter::Replaced || state.replacement_count > 0 {
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
        .child(
            button("筛选", "browser.apply-search", "soft", "gray")
                .disabled_if(state.busy),
        );

    if !state.search_query.trim().is_empty() {
        search_row = search_row.child(
            button("清空", "browser.clear-search", "ghost", "gray")
                .disabled_if(state.busy),
        );
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
            "当前目录下没有 LVGL 图片资源。"
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
        let show_full_path = !state.search_query.is_empty()
            || state.filter_mode == ResourceFilter::Replaced;
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
                    .child(badge(&format!("{} 项", state.replacement_count), "accent")),
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
                    .child(span(&format!("搜索: \"{}\"", state.search_query.trim()), 13))
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
    selected: bool,
    current_size: usize,
    show_full_path: bool,
    busy: bool,
) -> ui::Element {
    let icon = if entry.image.is_some() {
        "🖼️"
    } else {
        let ext = file_extension(&entry.name).unwrap_or_default();
        if ext == "bin" || ext == "dat" {
            "📦"
        } else {
            "📄"
        }
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
        .child(span(icon, 13))
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
        .child(text("预览与替换", 17));

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
                        .map(|info| format!(" · {}×{} LVGL I8", info.width, info.height))
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
                    "此替换文件不是可预览的 LVGL I8 图片。"
                } else {
                    "此资源不是受支持的 LVGL v9 I8 图片。"
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
                    .child(text("允许超过 256 色时进行有损量化", 13)),
            );
        }
        if state.selected_replaced {
            content = content.child(
                button("恢复固件原文件", "replace.restore", "soft", "gray").disabled_if(state.busy),
            );
        }
    } else {
        content = content.child(text("选择左侧文件；LVGL I8 图片会显示预览。", 14));
    }

    content = content.child(ui::Element::new(ui::ElementType::Separator, None));
    content = content.child(text("资源包信息", 15));
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
}
