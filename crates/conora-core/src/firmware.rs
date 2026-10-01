use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::io::{BufReader, Cursor, Read};
use std::sync::Arc;

use zip::{CompressionMethod, ZipArchive};

use crate::lvgl::{self, I8Info};

const ROMFS_MAGIC: &[u8; 8] = b"-rom1fs-";
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
    pub child_count: usize,
    pub has_replacements: bool,
}

enum FirmwareSource {
    RawRomfs(Vec<u8>),
    Archive {
        bytes: Vec<u8>,
        resource_index: usize,
    },
}

pub struct FirmwareIndex {
    source: FirmwareSource,
    files: Vec<ResourceFile>,
    file_lookup: HashMap<String, usize>,
    directories: HashSet<String>,
    last_file_bytes: RefCell<Option<(String, Arc<Vec<u8>>)>>,
}

impl FirmwareIndex {
    pub fn from_firmware(input: Vec<u8>) -> Result<Self, String> {
        if input.starts_with(ROMFS_MAGIC) {
            let resource_size = input.len() as u64;
            let (files, directories) = index_romfs(input.as_slice(), resource_size)?;
            return Ok(Self::from_parts(
                FirmwareSource::RawRomfs(input),
                files,
                directories,
            ));
        }

        let (resource_index, files, directories) = {
            let mut archive = ZipArchive::new(Cursor::new(input.as_slice()))
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
            let resource_size = entry.size();
            let (files, directories) = index_romfs(&mut entry, resource_size)?;
            (index, files, directories)
        };

        Ok(Self::from_parts(
            FirmwareSource::Archive {
                bytes: input,
                resource_index,
            },
            files,
            directories,
        ))
    }

    pub fn from_romfs(resource: Vec<u8>) -> Result<Self, String> {
        let resource_size = resource.len() as u64;
        let (files, directories) = index_romfs(resource.as_slice(), resource_size)?;
        Ok(Self::from_parts(
            FirmwareSource::RawRomfs(resource),
            files,
            directories,
        ))
    }

    fn from_parts(
        source: FirmwareSource,
        files: Vec<ResourceFile>,
        directories: HashSet<String>,
    ) -> Self {
        let file_lookup = files
            .iter()
            .enumerate()
            .map(|(index, file)| (file.path.clone(), index))
            .collect();
        Self {
            source,
            files,
            file_lookup,
            directories,
            last_file_bytes: RefCell::new(None),
        }
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

    pub fn file_bytes(&self, path: &str) -> Result<Option<Arc<Vec<u8>>>, String> {
        let Some(file) = self.file(path) else {
            return Ok(None);
        };
        if let Some((cached_path, bytes)) = self.last_file_bytes.borrow().as_ref()
            && cached_path == path
        {
            return Ok(Some(Arc::clone(bytes)));
        }

        self.last_file_bytes.borrow_mut().take();
        let bytes = Arc::new(self.source.read_range(file.offset, file.size)?);
        *self.last_file_bytes.borrow_mut() = Some((path.to_string(), Arc::clone(&bytes)));
        Ok(Some(bytes))
    }

    /// Visit each distinct requested resource in physical order, without caching it.
    /// All paths and budgets are checked before any resource is materialized or
    /// the callback is invoked. A callback error stops the traversal immediately.
    pub fn visit_file_bytes(
        &self,
        paths: &[String],
        max_file_bytes: usize,
        max_total_bytes: usize,
        mut callback: impl FnMut(&str, &[u8]) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut seen = HashSet::new();
        let mut files = Vec::new();
        let mut total = 0usize;
        let mut errors = Vec::new();
        for path in paths {
            if !seen.insert(path.as_str()) {
                continue;
            }
            let Some(file) = self.file(path) else {
                errors.push(format!("resource path does not exist: {path}"));
                continue;
            };
            if file.size > max_file_bytes {
                errors.push(format!(
                    "resource {path} exceeds the {max_file_bytes}-byte per-file limit"
                ));
            }
            match total.checked_add(file.size) {
                Some(size) => total = size,
                None => {
                    errors.push("aggregate resource size overflow".into());
                    total = usize::MAX;
                }
            }
            files.push(file);
        }
        if total > max_total_bytes {
            errors.push(format!(
                "resources exceed the {max_total_bytes}-byte aggregate limit"
            ));
        }
        if !errors.is_empty() {
            return Err(errors.join("; "));
        }
        files.sort_unstable_by_key(|file| file.offset);
        self.source.visit_ranges(&files, &mut callback)
    }

    pub fn file_thumbnail_pngs(
        &self,
        paths: &[String],
        max_dimension: u32,
    ) -> Result<HashMap<String, Vec<u8>>, String> {
        let mut files = paths
            .iter()
            .filter_map(|path| self.file(path))
            .filter(|file| file.image.is_some())
            .collect::<Vec<_>>();
        files.sort_unstable_by_key(|file| file.offset);
        self.source.thumbnail_ranges(&files, max_dimension)
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
                            child_count: 0,
                            has_replacements: false,
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
                        child_count: 0,
                        has_replacements: false,
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
                            child_count: 0,
                            has_replacements: false,
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
                        child_count: 0,
                        has_replacements: false,
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

    pub fn image_count(&self) -> usize {
        self.files.iter().filter(|f| f.image.is_some()).count()
    }

    pub fn file_count_in_dir(&self, dir: &str) -> usize {
        let prefix = if dir.is_empty() {
            String::new()
        } else {
            format!("{}/", dir.trim_end_matches('/'))
        };
        self.files
            .iter()
            .filter(|f| f.path.starts_with(&prefix))
            .count()
    }

    pub fn dir_has_images(&self, dir: &str) -> bool {
        let prefix = if dir.is_empty() {
            String::new()
        } else {
            format!("{}/", dir.trim_end_matches('/'))
        };
        self.files
            .iter()
            .any(|f| f.image.is_some() && f.path.starts_with(&prefix))
    }
}

impl FirmwareSource {
    fn visit_ranges(
        &self,
        files: &[&ResourceFile],
        callback: &mut impl FnMut(&str, &[u8]) -> Result<(), String>,
    ) -> Result<(), String> {
        if files.is_empty() {
            return Ok(());
        }
        match self {
            Self::RawRomfs(resource) => {
                for file in files {
                    let end = file
                        .offset
                        .checked_add(file.size)
                        .ok_or_else(|| "selected resource range overflow".to_string())?;
                    let data = resource
                        .get(file.offset..end)
                        .ok_or_else(|| "selected resource range is outside ROMFS".to_string())?;
                    callback(&file.path, data)?;
                }
            }
            Self::Archive {
                bytes,
                resource_index,
            } => {
                let mut archive = ZipArchive::new(Cursor::new(bytes.as_slice()))
                    .map_err(|error| format!("could not reopen firmware archive: {error}"))?;
                let mut entry = archive
                    .by_index(*resource_index)
                    .map_err(|error| format!("could not reopen vela_resource.bin: {error}"))?;
                let mut reader = ForwardReader::new(&mut entry);
                for file in files {
                    reader.advance_to(file.offset as u64)?;
                    let mut data = Vec::new();
                    data.try_reserve_exact(file.size).map_err(|error| {
                        format!(
                            "could not allocate {} bytes for {}: {error}",
                            file.size, file.path
                        )
                    })?;
                    data.resize(file.size, 0);
                    reader.read_exact(&mut data)?;
                    callback(&file.path, &data)?;
                }
            }
        }
        Ok(())
    }

    fn thumbnail_ranges(
        &self,
        files: &[&ResourceFile],
        max_dimension: u32,
    ) -> Result<HashMap<String, Vec<u8>>, String> {
        let mut thumbnails = HashMap::with_capacity(files.len());
        if files.is_empty() {
            return Ok(thumbnails);
        }

        match self {
            Self::RawRomfs(resource) => {
                for file in files {
                    let end = file
                        .offset
                        .checked_add(file.size)
                        .ok_or_else(|| "selected resource range overflow".to_string())?;
                    let source = resource
                        .get(file.offset..end)
                        .ok_or_else(|| "selected resource range is outside ROMFS".to_string())?;
                    if let Ok((_, png)) = lvgl::decode_thumbnail_png(source, max_dimension) {
                        thumbnails.insert(file.path.clone(), png);
                    }
                }
            }
            Self::Archive {
                bytes: archive_bytes,
                resource_index,
            } => {
                let mut archive = ZipArchive::new(Cursor::new(archive_bytes.as_slice()))
                    .map_err(|error| format!("could not reopen firmware archive: {error}"))?;
                let mut entry = archive
                    .by_index(*resource_index)
                    .map_err(|error| format!("could not reopen vela_resource.bin: {error}"))?;
                let mut reader = ForwardReader::new(&mut entry);
                for file in files {
                    reader.advance_to(file.offset as u64)?;
                    let mut data = vec![0u8; file.size];
                    reader.read_exact(&mut data)?;
                    if let Ok((_, png)) = lvgl::decode_thumbnail_png(&data, max_dimension) {
                        thumbnails.insert(file.path.clone(), png);
                    }
                }
            }
        }

        Ok(thumbnails)
    }

    fn read_range(&self, offset: usize, size: usize) -> Result<Vec<u8>, String> {
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(size).map_err(|error| {
            format!("could not allocate {size} bytes for the selected resource: {error}")
        })?;
        bytes.resize(size, 0);

        match self {
            Self::RawRomfs(resource) => {
                let end = offset
                    .checked_add(size)
                    .ok_or_else(|| "selected resource range overflow".to_string())?;
                let source = resource
                    .get(offset..end)
                    .ok_or_else(|| "selected resource range is outside ROMFS".to_string())?;
                bytes.copy_from_slice(source);
            }
            Self::Archive {
                bytes: archive_bytes,
                resource_index,
            } => {
                let mut archive = ZipArchive::new(Cursor::new(archive_bytes.as_slice()))
                    .map_err(|error| format!("could not reopen firmware archive: {error}"))?;
                let mut entry = archive
                    .by_index(*resource_index)
                    .map_err(|error| format!("could not reopen vela_resource.bin: {error}"))?;
                let mut reader = ForwardReader::new(&mut entry);
                reader.advance_to(offset as u64)?;
                reader.read_exact(&mut bytes)?;
            }
        }

        Ok(bytes)
    }
}

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd)]
enum RomfsEvent {
    Entry { parent: String },
    ImageHeader { file_index: usize },
}

struct ForwardReader<R> {
    inner: BufReader<R>,
    position: u64,
}

impl<R: Read> ForwardReader<R> {
    fn new(reader: R) -> Self {
        Self {
            inner: BufReader::with_capacity(64 * 1024, reader),
            position: 0,
        }
    }

    fn position(&self) -> u64 {
        self.position
    }

    fn advance_to(&mut self, target: u64) -> Result<(), String> {
        if target < self.position {
            return Err("ROMFS contains a backward or overlapping data reference".into());
        }
        let mut remaining = target - self.position;
        let mut scratch = [0u8; 16 * 1024];
        while remaining > 0 {
            let count = usize::try_from(remaining.min(scratch.len() as u64))
                .expect("bounded by scratch buffer");
            self.read_exact(&mut scratch[..count])?;
            remaining -= count as u64;
        }
        Ok(())
    }

    fn read_exact(&mut self, buffer: &mut [u8]) -> Result<(), String> {
        self.inner
            .read_exact(buffer)
            .map_err(|error| format!("could not read ROMFS data: {error}"))?;
        self.position = self
            .position
            .checked_add(buffer.len() as u64)
            .ok_or_else(|| "ROMFS offset overflow".to_string())?;
        Ok(())
    }

    fn read_name(&mut self) -> Result<String, String> {
        let mut bytes = Vec::new();
        loop {
            let mut byte = [0u8; 1];
            self.read_exact(&mut byte)?;
            if byte[0] == 0 {
                break;
            }
            if bytes.len() >= MAX_RESOURCE_PATH_BYTES {
                return Err("ROMFS contains a name longer than the supported limit".into());
            }
            bytes.push(byte[0]);
        }
        String::from_utf8(bytes).map_err(|_| "ROMFS contains a non-UTF-8 name".into())
    }

    fn finish(mut self, expected_size: u64) -> Result<(), String> {
        self.advance_to(expected_size)?;
        let mut scratch = [0u8; 16 * 1024];
        loop {
            let count = self
                .inner
                .read(&mut scratch)
                .map_err(|error| format!("could not finish reading ROMFS data: {error}"))?;
            if count == 0 {
                break;
            }
            self.position += count as u64;
        }
        if self.position != expected_size {
            return Err("vela_resource.bin length does not match its ZIP directory".into());
        }
        Ok(())
    }
}

impl<R: Read> Read for ForwardReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let count = self.inner.read(buffer)?;
        self.position = self.position.saturating_add(count as u64);
        Ok(count)
    }
}

fn index_romfs<R: Read>(
    reader: R,
    resource_size: u64,
) -> Result<(Vec<ResourceFile>, HashSet<String>), String> {
    let mut reader = ForwardReader::new(reader);
    if resource_size < 32 {
        return Err("vela_resource.bin is not a ROMFS image".into());
    }

    let mut header = [0u8; 16];
    reader.read_exact(&mut header)?;
    if &header[..8] != ROMFS_MAGIC {
        return Err("vela_resource.bin is not a ROMFS image".into());
    }
    let declared = u64::from(u32::from_be_bytes(
        header[8..12].try_into().expect("ROMFS size field"),
    ));
    if declared < 32 || declared > resource_size {
        return Err("ROMFS header contains an invalid image size".into());
    }

    let root_name = reader.read_name()?;
    if root_name.is_empty() || root_name.contains('/') || root_name.contains('\\') {
        return Err("ROMFS root name is invalid".into());
    }
    let root_offset = align_to_16(reader.position())?;
    if root_offset < 32 || root_offset >= declared {
        return Err("ROMFS root directory offset is invalid".into());
    }
    reader.advance_to(root_offset)?;

    let mut files: Vec<ResourceFile> = Vec::new();
    let mut directories = HashSet::new();
    let mut file_paths = HashSet::new();
    let mut seen_offsets = HashSet::new();
    // Process nodes and image headers in physical order; deflated ZIP members cannot seek.
    let mut pending = BinaryHeap::new();
    pending.push(Reverse((
        root_offset,
        RomfsEvent::Entry {
            parent: String::new(),
        },
    )));

    while let Some(Reverse((offset, event))) = pending.pop() {
        if offset < reader.position() {
            return Err("ROMFS contains a backward or overlapping data reference".into());
        }
        reader.advance_to(offset)?;
        match event {
            RomfsEvent::ImageHeader { file_index } => {
                let file = files
                    .get_mut(file_index)
                    .ok_or_else(|| "ROMFS image index is invalid".to_string())?;
                let header_len = file.size.min(24);
                let mut image_header = [0u8; 24];
                reader.read_exact(&mut image_header[..header_len])?;
                file.image = lvgl::inspect_image_header(&image_header[..header_len], file.size);
            }
            RomfsEvent::Entry { parent } => {
                let offset = usize::try_from(offset)
                    .map_err(|_| "ROMFS directory offset is too large".to_string())?;
                if !seen_offsets.insert(offset) {
                    return Err("ROMFS directory entries contain a cycle or shared node".into());
                }
                if seen_offsets.len() > MAX_ROMFS_ENTRIES {
                    return Err("ROMFS has too many directory entries".into());
                }
                if !offset.is_multiple_of(16)
                    || offset < 32
                    || (offset as u64)
                        .checked_add(16)
                        .is_none_or(|end| end > declared)
                {
                    return Err("ROMFS contains an invalid directory entry offset".into());
                }

                let mut entry_header = [0u8; 16];
                reader.read_exact(&mut entry_header)?;
                let next = u32::from_be_bytes(entry_header[..4].try_into().expect("next field"));
                let spec = u32::from_be_bytes(entry_header[4..8].try_into().expect("spec field"));
                let size = u32::from_be_bytes(entry_header[8..12].try_into().expect("size field"));
                let name = reader.read_name()?;
                let content_offset = align_to_16(reader.position())?;
                if content_offset > declared {
                    return Err("ROMFS name alignment runs beyond the image".into());
                }
                reader.advance_to(content_offset)?;

                let next_offset = (next & !0x0f) as u64;
                let kind = next & 0x07;
                if next_offset != 0 {
                    pending.push(Reverse((
                        next_offset,
                        RomfsEvent::Entry {
                            parent: parent.clone(),
                        },
                    )));
                }

                if name == "." || name == ".." {
                    continue;
                }
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
                            let child_offset = u64::from(spec);
                            if !child_offset.is_multiple_of(16)
                                || child_offset < 32
                                || child_offset
                                    .checked_add(16)
                                    .is_none_or(|end| end > declared)
                            {
                                return Err(format!(
                                    "ROMFS directory {path} has an invalid child pointer"
                                ));
                            }
                            pending
                                .push(Reverse((child_offset, RomfsEvent::Entry { parent: path })));
                        }
                    }
                    2 => {
                        let size = size as usize;
                        let end = content_offset
                            .checked_add(size as u64)
                            .ok_or_else(|| format!("ROMFS file {path} size overflow"))?;
                        if end > declared {
                            return Err(format!("ROMFS file {path} is truncated"));
                        }
                        if directories.contains(&path) || !file_paths.insert(path.clone()) {
                            return Err(format!("ROMFS contains a duplicate path: {path}"));
                        }
                        let file_index = files.len();
                        files.push(ResourceFile {
                            path,
                            size,
                            image: None,
                            offset: usize::try_from(content_offset)
                                .map_err(|_| "ROMFS file offset is too large".to_string())?,
                        });
                        if size >= 4 {
                            pending.push(Reverse((
                                content_offset,
                                RomfsEvent::ImageHeader { file_index },
                            )));
                        }
                    }
                    _ => {
                        // Symlinks and special filesystem entries are not followed or exposed.
                    }
                }
            }
        }
    }

    files.sort_by(|left, right| left.path.cmp(&right.path));
    reader.finish(resource_size)?;
    Ok((files, directories))
}

fn align_to_16(offset: u64) -> Result<u64, String> {
    offset
        .checked_add(15)
        .map(|value| value & !15)
        .ok_or_else(|| "ROMFS offset overflow".into())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn synthetic_romfs() -> Vec<u8> {
        synthetic_romfs_with_file(b"test")
    }

    fn synthetic_romfs_with_file(contents: &[u8]) -> Vec<u8> {
        let mut data = vec![0u8; 128 + contents.len()];
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
        data[104..108].copy_from_slice(&(contents.len() as u32).to_be_bytes());
        data[112..120].copy_from_slice(b"test.bin");
        data[128..].copy_from_slice(contents);
        data
    }

    fn synthetic_i8_image() -> Vec<u8> {
        let mut data = vec![0u8; 12 + 256 * 4 + 4];
        data[0] = 0x19;
        data[1] = 0x0a;
        data[4..6].copy_from_slice(&2u16.to_le_bytes());
        data[6..8].copy_from_slice(&2u16.to_le_bytes());
        data[8..10].copy_from_slice(&2u16.to_le_bytes());
        data[12..16].copy_from_slice(&[0, 0, 255, 255]);
        data[12 + 256 * 4..].fill(0);
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
        assert_eq!(
            index.file_bytes(&file.path).unwrap().unwrap().as_slice(),
            b"test"
        );
        assert_eq!(index.entries_in_dir("app")[0].name, "icons");
        assert!(
            index
                .entries_in_dir("app/icons")
                .iter()
                .any(|entry| entry.name == "test.bin")
        );
    }

    #[test]
    fn creates_small_thumbnails_from_raw_and_archived_romfs_without_full_file_buffers() {
        let image = synthetic_i8_image();
        let raw = FirmwareIndex::from_romfs(synthetic_romfs_with_file(&image)).unwrap();
        let paths = vec!["app/icons/test.bin".to_string()];
        let raw_thumbnails = raw.file_thumbnail_pngs(&paths, 28).unwrap();
        let raw_thumbnail = raw_thumbnails.get(&paths[0]).unwrap();
        assert!(raw_thumbnail.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(raw_thumbnail.len() < 1024);

        let archive = zip_with_file("vela_resource.bin", &synthetic_romfs_with_file(&image));
        let index = FirmwareIndex::from_firmware(archive).unwrap();
        let archive_thumbnails = index.file_thumbnail_pngs(&paths, 28).unwrap();
        let archive_thumbnail = archive_thumbnails.get(&paths[0]).unwrap();
        assert!(archive_thumbnail.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(archive_thumbnail.len() < 1024);
    }

    #[test]
    fn extracts_the_root_resource_from_a_jar() {
        let romfs = synthetic_romfs();
        let bytes = zip_with_file("vela_resource.bin", &romfs);
        let index = FirmwareIndex::from_firmware(bytes).unwrap();
        assert_eq!(index.file_count(), 1);
        assert_eq!(
            index
                .file_bytes("app/icons/test.bin")
                .unwrap()
                .unwrap()
                .as_slice(),
            b"test"
        );
    }

    #[test]
    fn rejects_an_archive_without_the_resource_image() {
        let bytes = zip_with_file("other.bin", b"not a ROMFS");
        assert!(FirmwareIndex::from_firmware(bytes).is_err());
    }

    #[test]
    fn parses_real_firmware_when_requested() {
        let Some(path) = std::env::var_os("CONORA_TEST_FIRMWARE") else {
            return;
        };
        let firmware = std::fs::read(path).expect("read requested firmware test fixture");
        let index = FirmwareIndex::from_firmware(firmware).expect("parse requested firmware");
        assert!(index.file_count() > 1_000);
        let images = index
            .files()
            .iter()
            .filter(|file| file.image.is_some())
            .count();
        eprintln!(
            "Indexed {} firmware files and {} images",
            index.file_count(),
            images
        );
        assert_eq!(images, 4115);

        // Test bidirectional conversion on real samples from each format family!
        let samples = [
            ("app/common/icon/confirm.bin", "LVGL9 I8"),
            ("app/watchface/rect_edit_box.bin", "LVGL9 I8 RLE"),
            ("app/sports/setting/reminder_hrzone.bin", "LVGL9 A8"),
            ("app/wxpay/widge_dark21_indexed_8.bin", "LVGL9 ARGB8888"),
            ("app/wxpay/wxlogo.bin", "LVGL9 I4"),
            ("app/sports/icon/anim/9/19.bin", "LVGL9 A4 RLE"),
            ("app/find_phone/phone.bin", "LVGL9 A4"),
            ("system/startup/miui.bin", "LVGL8 RGB565"),
            ("app/sleep/sleep_big_remind.bin", "LVGL8 I8"),
            ("app/stress/measure/Measuring9.jpg", "JPEG"),
            ("app/easter_egg/spaceship.png", "PNG"),
        ];

        for (path, label) in samples {
            let file_bytes = index
                .file_bytes(path)
                .expect("read sample file")
                .unwrap_or_else(|| panic!("fixture contains {}", path));
            let (info, png) = lvgl::decode_image_png(file_bytes.as_slice())
                .unwrap_or_else(|e| panic!("decode {} failed: {}", label, e));
            assert!(info.width > 0 && info.height > 0);
            assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));

            // Re-encode to target format
            let encoded = lvgl::encode_png_to_template(&png, file_bytes.as_slice(), true)
                .unwrap_or_else(|e| panic!("re-encode {} failed: {}", label, e));
            assert!(!encoded.is_empty());

            // Decode re-encoded to verify consistency
            let (info2, png2) = lvgl::decode_image_png(&encoded)
                .unwrap_or_else(|e| panic!("re-decode {} failed: {}", label, e));
            assert_eq!(info.width, info2.width);
            assert_eq!(info.height, info2.height);
            assert!(png2.starts_with(b"\x89PNG\r\n\x1a\n"));
        }
    }

    #[test]
    fn parses_large_firmware_when_requested() {
        let Some(path) = std::env::var_os("CONORA_TEST_LARGE_FIRMWARE") else {
            return;
        };
        let firmware = std::fs::read(path).expect("read requested large firmware fixture");
        let index = FirmwareIndex::from_firmware(firmware).expect("parse requested large firmware");
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
        assert!(index.file_count() > 5_000);
        let image = index
            .files()
            .iter()
            .find(|file| file.image.is_some())
            .expect("large fixture contains an LVGL I8 image");
        let image_bytes = index
            .file_bytes(&image.path)
            .expect("lazily read selected image")
            .expect("image path exists");
        assert_eq!(image_bytes.len(), image.size);
        assert_eq!(lvgl::inspect_i8(image_bytes.as_slice()), image.image);
    }
}
