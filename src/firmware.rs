use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read};

use zip::{CompressionMethod, ZipArchive};

use crate::lvgl::{self, I8Info};

const ROMFS_MAGIC: &[u8; 8] = b"-rom1fs-";
const MAX_INPUT_BYTES: usize = 128 * 1024 * 1024;
const MAX_ROMFS_BYTES: usize = 128 * 1024 * 1024;
const MAX_OUTER_ENTRIES: usize = 256;
const MAX_ROMFS_ENTRIES: usize = 50_000;
const MAX_RESOURCE_PATH_BYTES: usize = 1024;

#[derive(Clone, Debug)]
pub struct ResourceFile {
    pub path: String,
    pub size: usize,
    pub image: Option<I8Info>,
    offset: usize,
}

#[derive(Clone, Debug)]
pub struct BrowserEntry {
    pub name: String,
    pub path: String,
    pub is_directory: bool,
    pub size: usize,
    pub image: Option<I8Info>,
}

pub struct FirmwareIndex {
    resource: Vec<u8>,
    files: Vec<ResourceFile>,
    file_lookup: HashMap<String, usize>,
    directories: HashSet<String>,
}

impl FirmwareIndex {
    pub fn from_firmware(input: &[u8]) -> Result<Self, String> {
        if input.len() > MAX_INPUT_BYTES {
            return Err(format!(
                "firmware file exceeds the {} MiB import limit",
                MAX_INPUT_BYTES / 1024 / 1024
            ));
        }
        let resource = extract_resource(input)?;
        Self::from_romfs(resource)
    }

    pub fn from_romfs(resource: Vec<u8>) -> Result<Self, String> {
        if resource.len() > MAX_ROMFS_BYTES {
            return Err("ROMFS resource image exceeds the 128 MiB limit".into());
        }
        let (files, directories) = index_romfs(&resource)?;
        let file_lookup = files
            .iter()
            .enumerate()
            .map(|(index, file)| (file.path.clone(), index))
            .collect();
        Ok(Self {
            resource,
            files,
            file_lookup,
            directories,
        })
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    pub fn files(&self) -> &[ResourceFile] {
        &self.files
    }

    pub fn file(&self, path: &str) -> Option<&ResourceFile> {
        self.file_lookup.get(path).map(|index| &self.files[*index])
    }

    pub fn file_bytes(&self, path: &str) -> Option<&[u8]> {
        let file = self.file(path)?;
        self.resource
            .get(file.offset..file.offset.checked_add(file.size)?)
    }

    pub fn entries_in_dir(&self, directory: &str) -> Vec<BrowserEntry> {
        let prefix = if directory.is_empty() {
            String::new()
        } else {
            format!("{}/", directory.trim_end_matches('/'))
        };
        let mut entries = HashMap::<String, BrowserEntry>::new();

        for path in self
            .directories
            .iter()
            .filter(|path| path.starts_with(&prefix))
        {
            let Some(rest) = path.strip_prefix(&prefix) else {
                continue;
            };
            if let Some((name, _)) = rest.split_once('/') {
                if !name.is_empty() {
                    entries
                        .entry(name.to_string())
                        .or_insert_with(|| BrowserEntry {
                            name: name.to_string(),
                            path: format!("{prefix}{name}"),
                            is_directory: true,
                            size: 0,
                            image: None,
                        });
                }
            } else if !rest.is_empty() {
                entries
                    .entry(rest.to_string())
                    .or_insert_with(|| BrowserEntry {
                        name: rest.to_string(),
                        path: format!("{prefix}{rest}"),
                        is_directory: true,
                        size: 0,
                        image: None,
                    });
            }
        }

        for file in self
            .files
            .iter()
            .filter(|file| file.path.starts_with(&prefix))
        {
            let Some(rest) = file.path.strip_prefix(&prefix) else {
                continue;
            };
            if let Some((name, _)) = rest.split_once('/') {
                if !name.is_empty() {
                    entries
                        .entry(name.to_string())
                        .or_insert_with(|| BrowserEntry {
                            name: name.to_string(),
                            path: format!("{prefix}{name}"),
                            is_directory: true,
                            size: 0,
                            image: None,
                        });
                }
            } else if !rest.is_empty() {
                entries.insert(
                    rest.to_string(),
                    BrowserEntry {
                        name: rest.to_string(),
                        path: file.path.clone(),
                        is_directory: false,
                        size: file.size,
                        image: file.image,
                    },
                );
            }
        }

        let mut entries = entries.into_values().collect::<Vec<_>>();
        entries.sort_by(|left, right| {
            right
                .is_directory
                .cmp(&left.is_directory)
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
        });
        entries
    }

    pub fn directory_exists(&self, path: &str) -> bool {
        path.is_empty() || self.directories.contains(path.trim_end_matches('/'))
    }
}

fn extract_resource(input: &[u8]) -> Result<Vec<u8>, String> {
    if input.starts_with(ROMFS_MAGIC) {
        return Ok(input.to_vec());
    }

    let mut archive = ZipArchive::new(Cursor::new(input))
        .map_err(|error| format!("firmware is not a readable ZIP/JAR archive: {error}"))?;
    if archive.is_empty() || archive.len() > MAX_OUTER_ENTRIES {
        return Err("firmware archive has an invalid entry count".into());
    }

    let mut seen = HashSet::with_capacity(archive.len());
    let mut resource_index = None;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|error| format!("could not read firmware ZIP entry: {error}"))?;
        let name = entry.name().to_string();
        if !seen.insert(name.clone()) {
            return Err(format!("firmware ZIP contains duplicate entry: {name}"));
        }
        if entry.encrypted() || entry.is_symlink() {
            return Err(format!(
                "firmware ZIP entry is encrypted or a symlink: {name}"
            ));
        }
        if !matches!(
            entry.compression(),
            CompressionMethod::Stored | CompressionMethod::Deflated
        ) {
            return Err(format!(
                "firmware ZIP uses an unsupported compression method: {name}"
            ));
        }
        if name == "vela_resource.bin" {
            if entry.is_dir() {
                return Err("vela_resource.bin is not a regular file".into());
            }
            resource_index = Some(index);
        }
    }

    let index = resource_index
        .ok_or_else(|| "firmware archive has no root vela_resource.bin".to_string())?;
    let mut entry = archive
        .by_index(index)
        .map_err(|error| format!("could not open vela_resource.bin: {error}"))?;
    if entry.size() > MAX_ROMFS_BYTES as u64 {
        return Err("vela_resource.bin exceeds the 128 MiB extraction limit".into());
    }

    let expected_size = entry.size() as usize;
    let mut resource = Vec::with_capacity(expected_size);
    entry.read_to_end(&mut resource).map_err(|error| {
        format!("could not extract vela_resource.bin (CRC/Deflate error): {error}")
    })?;
    if resource.len() != expected_size {
        return Err("vela_resource.bin length does not match its ZIP directory".into());
    }
    Ok(resource)
}

fn index_romfs(data: &[u8]) -> Result<(Vec<ResourceFile>, HashSet<String>), String> {
    if data.len() < 32 || !data.starts_with(ROMFS_MAGIC) {
        return Err("vela_resource.bin is not a ROMFS image".into());
    }
    let declared = u32::from_be_bytes(data[8..12].try_into().expect("ROMFS size")) as usize;
    if !(32..=MAX_ROMFS_BYTES).contains(&declared) || declared > data.len() {
        return Err("ROMFS header contains an invalid image size".into());
    }
    let data = &data[..declared];
    let (root_name, root_offset) = read_romfs_name(data, 16)?;
    if root_name.is_empty() || root_name.contains('/') || root_name.contains('\\') {
        return Err("ROMFS root name is invalid".into());
    }

    let mut files = Vec::new();
    let mut directories = HashSet::new();
    let mut file_paths = HashSet::new();
    let mut seen_offsets = HashSet::new();
    let mut pending = vec![(root_offset, String::new())];

    while let Some((mut offset, parent)) = pending.pop() {
        while offset != 0 {
            if !offset.is_multiple_of(16)
                || offset < 32
                || offset.checked_add(16).is_none_or(|end| end > declared)
            {
                return Err("ROMFS contains an invalid directory entry offset".into());
            }
            if !seen_offsets.insert(offset) {
                return Err("ROMFS directory entries contain a cycle or shared node".into());
            }
            if seen_offsets.len() > MAX_ROMFS_ENTRIES {
                return Err("ROMFS has too many directory entries".into());
            }

            let next = be_u32(data, offset)?;
            let spec = be_u32(data, offset + 4)? as usize;
            let size = be_u32(data, offset + 8)? as usize;
            let (name, content_offset) = read_romfs_name(data, offset + 16)?;
            let next_offset = (next & !0x0f) as usize;
            let kind = next & 0x07;

            if name != "." && name != ".." {
                validate_romfs_component(&name)?;
                let path = if parent.is_empty() {
                    name
                } else {
                    format!("{parent}/{name}")
                };
                if path.len() > MAX_RESOURCE_PATH_BYTES {
                    return Err("ROMFS contains a path longer than the supported limit".into());
                }

                match kind {
                    1 => {
                        if file_paths.contains(&path) || !directories.insert(path.clone()) {
                            return Err(format!("ROMFS contains a duplicate path: {path}"));
                        }
                        if spec != 0 {
                            if !spec.is_multiple_of(16)
                                || spec < 32
                                || spec.checked_add(16).is_none_or(|end| end > declared)
                            {
                                return Err(format!(
                                    "ROMFS directory {path} has an invalid child pointer"
                                ));
                            }
                            pending.push((spec, path));
                        }
                    }
                    2 => {
                        let end = content_offset
                            .checked_add(size)
                            .ok_or_else(|| format!("ROMFS file {path} size overflow"))?;
                        if end > declared {
                            return Err(format!("ROMFS file {path} is truncated"));
                        }
                        if directories.contains(&path) || !file_paths.insert(path.clone()) {
                            return Err(format!("ROMFS contains a duplicate path: {path}"));
                        }
                        let image = lvgl::inspect_i8(&data[content_offset..end]);
                        files.push(ResourceFile {
                            path,
                            size,
                            image,
                            offset: content_offset,
                        });
                    }
                    _ => {
                        // Symlinks and special filesystem entries are not followed or exposed.
                    }
                }
            }
            offset = next_offset;
        }
    }

    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok((files, directories))
}

fn validate_romfs_component(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains(':')
        || name.chars().any(|character| character.is_control())
    {
        return Err(format!("ROMFS contains an unsafe path component: {name:?}"));
    }
    Ok(())
}

fn read_romfs_name(data: &[u8], offset: usize) -> Result<(String, usize), String> {
    if offset >= data.len() {
        return Err("ROMFS name starts outside the image".into());
    }
    let end = data[offset..]
        .iter()
        .position(|byte| *byte == 0)
        .map(|relative| offset + relative)
        .ok_or_else(|| "ROMFS contains an unterminated name".to_string())?;
    let name = std::str::from_utf8(&data[offset..end])
        .map_err(|_| "ROMFS contains a non-UTF-8 name".to_string())?
        .to_string();
    let aligned = end
        .checked_add(16)
        .map(|value| value & !15)
        .ok_or_else(|| "ROMFS name offset overflow".to_string())?;
    if aligned > data.len() {
        return Err("ROMFS name alignment runs beyond the image".into());
    }
    Ok((name, aligned))
}

fn be_u32(data: &[u8], offset: usize) -> Result<u32, String> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or_else(|| "ROMFS integer is truncated".to_string())?;
    Ok(u32::from_be_bytes(bytes.try_into().expect("four bytes")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn synthetic_romfs() -> Vec<u8> {
        let mut data = vec![0u8; 132];
        data[..8].copy_from_slice(ROMFS_MAGIC);
        let image_size = data.len() as u32;
        data[8..12].copy_from_slice(&image_size.to_be_bytes());
        data[16..24].copy_from_slice(b"resource");

        data[32..36].copy_from_slice(&1u32.to_be_bytes());
        data[36..40].copy_from_slice(&64u32.to_be_bytes());
        data[48..51].copy_from_slice(b"app");

        data[64..68].copy_from_slice(&1u32.to_be_bytes());
        data[68..72].copy_from_slice(&96u32.to_be_bytes());
        data[80..85].copy_from_slice(b"icons");

        data[96..100].copy_from_slice(&2u32.to_be_bytes());
        data[104..108].copy_from_slice(&4u32.to_be_bytes());
        data[112..120].copy_from_slice(b"test.bin");
        data[128..132].copy_from_slice(b"test");
        data
    }

    fn zip_with_file(path: &str, contents: &[u8]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        writer.start_file(path, options).unwrap();
        writer.write_all(contents).unwrap();
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn imports_a_raw_romfs_and_lists_nested_files() {
        let image = synthetic_romfs();
        let index = FirmwareIndex::from_romfs(image).unwrap();
        assert_eq!(index.file_count(), 1);
        let file = index.file("app/icons/test.bin").unwrap();
        assert_eq!(index.file_bytes(&file.path), Some(&b"test"[..]));
        assert_eq!(index.entries_in_dir("app")[0].name, "icons");
        assert!(
            index
                .entries_in_dir("app/icons")
                .iter()
                .any(|entry| entry.name == "test.bin")
        );
    }

    #[test]
    fn extracts_the_root_resource_from_a_jar() {
        let romfs = synthetic_romfs();
        let bytes = zip_with_file("vela_resource.bin", &romfs);
        let index = FirmwareIndex::from_firmware(&bytes).unwrap();
        assert_eq!(index.file_count(), 1);
    }

    #[test]
    fn rejects_an_archive_without_the_resource_image() {
        let bytes = zip_with_file("other.bin", b"not a ROMFS");
        assert!(FirmwareIndex::from_firmware(&bytes).is_err());
    }

    #[test]
    fn parses_real_firmware_when_requested() {
        let Some(path) = std::env::var_os("CONORA_TEST_FIRMWARE") else {
            return;
        };
        let firmware = std::fs::read(path).expect("read requested firmware test fixture");
        let index = FirmwareIndex::from_firmware(&firmware).expect("parse requested firmware");
        assert!(index.file_count() > 1_000);
        let images = index
            .files()
            .iter()
            .filter(|file| file.image.is_some())
            .count();
        eprintln!(
            "Indexed {} firmware files and {} LVGL I8 images",
            index.file_count(),
            images
        );
        let confirm = index
            .file_bytes("app/common/icon/confirm.bin")
            .expect("fixture contains the sample confirm image");
        let (info, png) = lvgl::decode_i8_png(confirm).expect("decode real LVGL I8 image");
        assert_eq!((info.width, info.height), (48, 48));
        let restored = lvgl::encode_png_i8(&png, confirm, false)
            .expect("re-encode the original PNG losslessly");
        assert_eq!(restored, confirm);
    }
}
