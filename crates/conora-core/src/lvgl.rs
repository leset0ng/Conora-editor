use std::collections::HashMap;
use std::io::{Cursor, Read};

use image::{DynamicImage, ImageFormat, ImageReader, Limits, RgbaImage};
use serde::{Deserialize, Serialize};

const HEADER_BYTES_V9: usize = 12;
const COMP_HEADER_BYTES_V9: usize = 12;
const PALETTE_BYTES_I8: usize = 256 * 4;
const PALETTE_BYTES_I4: usize = 16 * 4;
const MAX_IMAGE_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_THUMBNAIL_DIMENSION: u32 = 64;
const MAX_PNG_BYTES: usize = 64 * 1024 * 1024;
const MAX_UNIQUE_COLORS: usize = 1_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResizeFilter {
    #[default]
    Lanczos3,
    Nearest,
    Triangle,
    CatmullRom,
    Gaussian,
}

impl ResizeFilter {
    pub fn is_default(&self) -> bool {
        matches!(self, Self::Lanczos3)
    }

    pub fn to_image_filter(self) -> image::imageops::FilterType {
        match self {
            Self::Lanczos3 => image::imageops::FilterType::Lanczos3,
            Self::Nearest => image::imageops::FilterType::Nearest,
            Self::Triangle => image::imageops::FilterType::Triangle,
            Self::CatmullRom => image::imageops::FilterType::CatmullRom,
            Self::Gaussian => image::imageops::FilterType::Gaussian,
        }
    }

    pub fn parse_str(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "lanczos3" | "lanczos" | "smooth" => Some(Self::Lanczos3),
            "nearest" | "pixel" => Some(Self::Nearest),
            "triangle" | "bilinear" => Some(Self::Triangle),
            "catmullrom" | "catmull-rom" | "bicubic" => Some(Self::CatmullRom),
            "gaussian" => Some(Self::Gaussian),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lanczos3 => "lanczos3",
            Self::Nearest => "nearest",
            Self::Triangle => "triangle",
            Self::CatmullRom => "catmull-rom",
            Self::Gaussian => "gaussian",
        }
    }
}

impl std::fmt::Display for ResizeFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl std::str::FromStr for ResizeFilter {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse_str(s).ok_or_else(|| {
            format!("unknown resize filter '{s}'; expected 'lanczos3', 'nearest', 'triangle', 'catmull-rom', or 'gaussian'")
        })
    }
}


type Rgba = [u8; 4];

fn pixel_color(pixel: &[u8; 4]) -> Rgba {
    *pixel
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageFormatKind {
    Lvgl9I8,
    Lvgl9I8Rle,
    Lvgl9A8,
    Lvgl9A8Rle,
    Lvgl9Argb8888,
    Lvgl9Argb8888Rle,
    Lvgl9I4,
    Lvgl9I4Rle,
    Lvgl9A4,
    Lvgl9A4Rle,
    Lvgl8Rgb565,
    Lvgl8I8,
    Png,
    Jpeg,
}

impl ImageFormatKind {
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Lvgl9I8 => "LVGL v9 I8",
            Self::Lvgl9I8Rle => "LVGL v9 I8 (RLE)",
            Self::Lvgl9A8 => "LVGL v9 A8",
            Self::Lvgl9A8Rle => "LVGL v9 A8 (RLE)",
            Self::Lvgl9Argb8888 => "LVGL v9 ARGB8888",
            Self::Lvgl9Argb8888Rle => "LVGL v9 ARGB8888 (RLE)",
            Self::Lvgl9I4 => "LVGL v9 I4",
            Self::Lvgl9I4Rle => "LVGL v9 I4 (RLE)",
            Self::Lvgl9A4 => "LVGL v9 A4",
            Self::Lvgl9A4Rle => "LVGL v9 A4 (RLE)",
            Self::Lvgl8Rgb565 => "LVGL v8 RGB565",
            Self::Lvgl8I8 => "LVGL v8 I8",
            Self::Png => "PNG",
            Self::Jpeg => "JPEG",
        }
    }

    pub fn is_rle(&self) -> bool {
        matches!(
            self,
            Self::Lvgl9I8Rle
                | Self::Lvgl9A8Rle
                | Self::Lvgl9Argb8888Rle
                | Self::Lvgl9I4Rle
                | Self::Lvgl9A4Rle
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageInfo {
    pub format: ImageFormatKind,
    pub width: u16,
    pub height: u16,
    pub stride: u16,
}

pub type I8Info = ImageInfo;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodedImage {
    pub bytes: Vec<u8>,
    pub lossy_quantization: bool,
}

pub type EncodedI8 = EncodedImage;

pub fn rle_decompress(input_data: &[u8], output_len: usize) -> Vec<u8> {
    let mut output = Vec::with_capacity(output_len);
    let mut i = 0;
    while i < input_data.len() && output.len() < output_len {
        let ctrl = input_data[i];
        i += 1;
        if ctrl & 0x80 != 0 {
            let count = (ctrl & 0x7F) as usize;
            let end = (i + count).min(input_data.len());
            let to_copy = (end - i).min(output_len - output.len());
            output.extend_from_slice(&input_data[i..i + to_copy]);
            i += count;
        } else {
            let count = ctrl as usize;
            if i < input_data.len() {
                let block = input_data[i];
                i += 1;
                let to_repeat = count.min(output_len - output.len());
                output.resize(output.len() + to_repeat, block);
            }
        }
    }
    output
}

pub fn rle_compress(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let n = data.len();
    let mut i = 0;
    while i < n {
        let mut run_len = 1;
        while i + run_len < n && run_len < 127 && data[i + run_len] == data[i] {
            run_len += 1;
        }

        if run_len >= 2 {
            out.push(run_len as u8);
            out.push(data[i]);
            i += run_len;
        } else {
            let start_lit = i;
            let mut lit_count = 0;
            while i < n && lit_count < 127 {
                if i + 1 < n && data[i] == data[i + 1] {
                    break;
                }
                lit_count += 1;
                i += 1;
            }
            out.push(0x80 | (lit_count as u8));
            out.extend_from_slice(&data[start_lit..start_lit + lit_count]);
        }
    }
    out
}

pub fn inspect_image_header(header: &[u8], image_size: usize) -> Option<ImageInfo> {
    if header.len() < 4 {
        return None;
    }

    if header.starts_with(b"\x89PNG\r\n\x1a\n") {
        let (width, height) = if header.len() >= 24 && &header[12..16] == b"IHDR" {
            (
                u16::try_from(u32::from_be_bytes([
                    header[16], header[17], header[18], header[19],
                ]))
                .ok()?,
                u16::try_from(u32::from_be_bytes([
                    header[20], header[21], header[22], header[23],
                ]))
                .ok()?,
            )
        } else {
            (0, 0)
        };
        return Some(ImageInfo {
            format: ImageFormatKind::Png,
            width,
            height,
            stride: width.saturating_mul(4),
        });
    }

    if header.starts_with(b"\xff\xd8\xff") {
        let (width, height) = parse_jpeg_dimensions(header).unwrap_or((0, 0));
        return Some(ImageInfo {
            format: ImageFormatKind::Jpeg,
            width,
            height,
            stride: 0,
        });
    }

    if header.len() >= HEADER_BYTES_V9 && header[0] == 0x19 {
        let cf = header[1];
        let flags = u16::from_le_bytes([header[2], header[3]]);
        let width = u16::from_le_bytes([header[4], header[5]]);
        let height = u16::from_le_bytes([header[6], header[7]]);
        let stride = u16::from_le_bytes([header[8], header[9]]);
        let reserved = u16::from_le_bytes([header[10], header[11]]);

        let min_stride = match cf {
            0x09 | 0x0D => width.div_ceil(2),
            0x10 => width.saturating_mul(4),
            0x0A | 0x0E => width,
            _ => return None,
        };

        if reserved != 0 || width == 0 || height == 0 || stride < min_stride {
            return None;
        }

        let pixel_count = u64::from(width) * u64::from(height);
        if pixel_count > MAX_IMAGE_PIXELS {
            return None;
        }

        let (format, min_raw_len) = match (cf, flags) {
            (0x0A, 0) => (
                ImageFormatKind::Lvgl9I8,
                PALETTE_BYTES_I8 + usize::from(stride) * usize::from(height),
            ),
            (0x0A, 8) => (
                ImageFormatKind::Lvgl9I8Rle,
                PALETTE_BYTES_I8 + usize::from(stride) * usize::from(height),
            ),
            (0x0E, 0) => (
                ImageFormatKind::Lvgl9A8,
                usize::from(stride) * usize::from(height),
            ),
            (0x0E, 8) => (
                ImageFormatKind::Lvgl9A8Rle,
                usize::from(stride) * usize::from(height),
            ),
            (0x10, 0) => (
                ImageFormatKind::Lvgl9Argb8888,
                usize::from(stride) * usize::from(height),
            ),
            (0x10, 8) => (
                ImageFormatKind::Lvgl9Argb8888Rle,
                usize::from(stride) * usize::from(height),
            ),
            (0x09, 0) => (
                ImageFormatKind::Lvgl9I4,
                PALETTE_BYTES_I4 + usize::from(stride) * usize::from(height),
            ),
            (0x09, 8) => (
                ImageFormatKind::Lvgl9I4Rle,
                PALETTE_BYTES_I4 + usize::from(stride) * usize::from(height),
            ),
            (0x0D, 0) => (
                ImageFormatKind::Lvgl9A4,
                usize::from(stride) * usize::from(height),
            ),
            (0x0D, 8) => (
                ImageFormatKind::Lvgl9A4Rle,
                usize::from(stride) * usize::from(height),
            ),
            _ => return None,
        };

        if flags == 8 {
            if header.len() >= HEADER_BYTES_V9 + COMP_HEADER_BYTES_V9 {
                let comp_len =
                    u32::from_le_bytes([header[16], header[17], header[18], header[19]]) as usize;
                if 24 + comp_len > image_size {
                    return None;
                }
            } else if image_size < 24 {
                return None;
            }
        } else if HEADER_BYTES_V9 + min_raw_len > image_size {
            return None;
        }

        return Some(ImageInfo {
            format,
            width,
            height,
            stride,
        });
    }

    if header.len() >= 4 {
        let val = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
        let cf = val & 0x1F;
        let always_zero = (val >> 5) & 0x07;
        let width = ((val >> 10) & 0x7FF) as u16;
        let height = ((val >> 21) & 0x7FF) as u16;

        if always_zero == 0 && width > 0 && height > 0 {
            if cf == 4 {
                let expected = 4 + usize::from(width) * usize::from(height) * 2;
                if image_size == expected {
                    return Some(ImageInfo {
                        format: ImageFormatKind::Lvgl8Rgb565,
                        width,
                        height,
                        stride: width.saturating_mul(2),
                    });
                }
            } else if cf == 10 {
                let expected = 4 + PALETTE_BYTES_I8 + usize::from(width) * usize::from(height);
                if image_size == expected {
                    return Some(ImageInfo {
                        format: ImageFormatKind::Lvgl8I8,
                        width,
                        height,
                        stride: width,
                    });
                }
            }
        }
    }

    None
}

pub fn inspect_image(data: &[u8]) -> Option<ImageInfo> {
    if let Some(mut info) = inspect_image_header(data, data.len()) {
        if info.format == ImageFormatKind::Jpeg
            && (info.width == 0 || info.height == 0)
            && let Some((w, h)) = parse_jpeg_dimensions(data)
        {
            info.width = w;
            info.height = h;
        }
        Some(info)
    } else {
        None
    }
}

pub fn inspect_i8(data: &[u8]) -> Option<ImageInfo> {
    inspect_image(data)
}

pub fn inspect_i8_header(header: &[u8], image_size: usize) -> Option<ImageInfo> {
    inspect_image_header(header, image_size)
}

fn parse_jpeg_dimensions(data: &[u8]) -> Option<(u16, u16)> {
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return None;
    }
    let mut i = 2;
    while i + 8 < data.len() {
        if data[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = data[i + 1];
        if matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF) {
            let height = u16::from_be_bytes([data[i + 5], data[i + 6]]);
            let width = u16::from_be_bytes([data[i + 7], data[i + 8]]);
            return Some((width, height));
        } else if marker == 0xD8 || marker == 0xD9 {
            i += 2;
        } else {
            if i + 4 > data.len() {
                break;
            }
            let length = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
            i += 2 + length;
        }
    }
    None
}

fn validate_rle_payload(data: &[u8], info: ImageInfo) -> Result<(), String> {
    if !info.format.is_rle() {
        return Ok(());
    }
    let expanded = data.get(20..24).ok_or("truncated RLE header")?;
    let expanded = u32::from_le_bytes(expanded.try_into().unwrap()) as usize;
    let palette = match info.format {
        ImageFormatKind::Lvgl9I8Rle => PALETTE_BYTES_I8,
        ImageFormatKind::Lvgl9I4Rle => PALETTE_BYTES_I4,
        _ => 0,
    };
    let expected = usize::from(info.stride) * usize::from(info.height) + palette;
    // Vendor resources can include trailing decompressed padding after the pixels.
    if expanded < expected || expanded > 64 * 1024 * 1024 {
        return Err("RLE expanded payload does not match bounded image dimensions".into());
    }
    Ok(())
}

pub fn decode_to_rgba(data: &[u8]) -> Result<(ImageInfo, RgbaImage), String> {
    let info = inspect_image(data).ok_or_else(|| "Not a recognized image format".to_string())?;
    validate_rle_payload(data, info)?;

    match info.format {
        ImageFormatKind::Png | ImageFormatKind::Jpeg => {
            let reader = || {
                image::ImageReader::new(Cursor::new(data))
                    .with_guessed_format()
                    .map_err(|e| format!("decode failed: {e}"))
            };
            let (width, height) = reader()?
                .into_dimensions()
                .map_err(|e| format!("decode failed: {e}"))?;
            if width == 0
                || height == 0
                || width > u32::from(u16::MAX)
                || height > u32::from(u16::MAX)
                || u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS
            {
                return Err("image dimensions exceed the 16-megapixel decoding limit".into());
            }
            let mut limits = image::Limits::default();
            limits.max_image_width = Some(width);
            limits.max_image_height = Some(height);
            limits.max_alloc = Some(MAX_IMAGE_PIXELS * 16 + 1024 * 1024);
            let mut bounded_reader = reader()?;
            bounded_reader.limits(limits);
            let img = bounded_reader
                .decode()
                .map_err(|e| format!("decode failed: {e}"))?
                .to_rgba8();
            let mut resolved_info = info;
            resolved_info.width = img.width() as u16;
            resolved_info.height = img.height() as u16;
            Ok((resolved_info, img))
        }

        ImageFormatKind::Lvgl8Rgb565 => {
            let w = usize::from(info.width);
            let h = usize::from(info.height);
            let mut img = RgbaImage::new(info.width as u32, info.height as u32);
            let raw_px = &data[4..];
            for y in 0..h {
                for x in 0..w {
                    let idx = (y * w + x) * 2;
                    let p16 = u16::from_le_bytes([raw_px[idx], raw_px[idx + 1]]);
                    let r = (((p16 >> 11) & 0x1F) as u32 * 255 / 31) as u8;
                    let g = (((p16 >> 5) & 0x3F) as u32 * 255 / 63) as u8;
                    let b = ((p16 & 0x1F) as u32 * 255 / 31) as u8;
                    img.put_pixel(x as u32, y as u32, image::Rgba([r, g, b, 255]));
                }
            }
            Ok((info, img))
        }

        ImageFormatKind::Lvgl8I8 => {
            let w = usize::from(info.width);
            let h = usize::from(info.height);
            let mut palette = [[0u8; 4]; 256];
            for (idx, entry) in data[4..1028].as_chunks::<4>().0.iter().enumerate() {
                palette[idx] = [entry[2], entry[1], entry[0], entry[3]];
            }
            let px_data = &data[1028..];
            let mut img = RgbaImage::new(info.width as u32, info.height as u32);
            for y in 0..h {
                for x in 0..w {
                    let color = palette[px_data[y * w + x] as usize];
                    img.put_pixel(x as u32, y as u32, image::Rgba(color));
                }
            }
            Ok((info, img))
        }

        ImageFormatKind::Lvgl9I8
        | ImageFormatKind::Lvgl9I8Rle
        | ImageFormatKind::Lvgl9A8
        | ImageFormatKind::Lvgl9A8Rle
        | ImageFormatKind::Lvgl9Argb8888
        | ImageFormatKind::Lvgl9Argb8888Rle
        | ImageFormatKind::Lvgl9I4
        | ImageFormatKind::Lvgl9I4Rle
        | ImageFormatKind::Lvgl9A4
        | ImageFormatKind::Lvgl9A4Rle => {
            let raw_payload = if info.format.is_rle() {
                if data.len() < 24 {
                    return Err("truncated LVGL9 RLE header".into());
                }
                let comp_len =
                    u32::from_le_bytes([data[16], data[17], data[18], data[19]]) as usize;
                let raw_len = u32::from_le_bytes([data[20], data[21], data[22], data[23]]) as usize;
                if data.len() < 24 + comp_len {
                    return Err("truncated LVGL9 RLE payload".into());
                }
                let payload = rle_decompress(&data[24..24 + comp_len], raw_len);
                if payload.len() != raw_len {
                    return Err("truncated LVGL9 RLE expanded payload".into());
                }
                payload
            } else {
                data[12..].to_vec()
            };

            let w = usize::from(info.width);
            let h = usize::from(info.height);
            let stride = usize::from(info.stride);
            let mut img = RgbaImage::new(info.width as u32, info.height as u32);

            match info.format {
                ImageFormatKind::Lvgl9I8 | ImageFormatKind::Lvgl9I8Rle => {
                    if raw_payload.len() < PALETTE_BYTES_I8 {
                        return Err("truncated LVGL9 I8 palette".into());
                    }
                    let mut palette = [[0u8; 4]; 256];
                    for (idx, entry) in raw_payload[..PALETTE_BYTES_I8]
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .enumerate()
                    {
                        palette[idx] = [entry[2], entry[1], entry[0], entry[3]];
                    }
                    let px_data = &raw_payload[PALETTE_BYTES_I8..];
                    for y in 0..h {
                        let row_start = y * stride;
                        for x in 0..w {
                            if row_start + x < px_data.len() {
                                let color = palette[px_data[row_start + x] as usize];
                                img.put_pixel(x as u32, y as u32, image::Rgba(color));
                            }
                        }
                    }
                }

                ImageFormatKind::Lvgl9A8 | ImageFormatKind::Lvgl9A8Rle => {
                    for y in 0..h {
                        let row_start = y * stride;
                        for x in 0..w {
                            let off = row_start + x;
                            let alpha = if off < raw_payload.len() {
                                raw_payload[off]
                            } else {
                                0
                            };
                            img.put_pixel(x as u32, y as u32, image::Rgba([255, 255, 255, alpha]));
                        }
                    }
                }

                ImageFormatKind::Lvgl9Argb8888 | ImageFormatKind::Lvgl9Argb8888Rle => {
                    for y in 0..h {
                        let row_start = y * stride;
                        for x in 0..w {
                            let off = row_start + x * 4;
                            if off + 4 <= raw_payload.len() {
                                let b = raw_payload[off];
                                let g = raw_payload[off + 1];
                                let r = raw_payload[off + 2];
                                let a = raw_payload[off + 3];
                                img.put_pixel(x as u32, y as u32, image::Rgba([r, g, b, a]));
                            }
                        }
                    }
                }

                ImageFormatKind::Lvgl9I4 | ImageFormatKind::Lvgl9I4Rle => {
                    if raw_payload.len() < PALETTE_BYTES_I4 {
                        return Err("truncated LVGL9 I4 palette".into());
                    }
                    let mut palette = [[0u8; 4]; 16];
                    for (idx, entry) in raw_payload[..PALETTE_BYTES_I4]
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .enumerate()
                    {
                        palette[idx] = [entry[2], entry[1], entry[0], entry[3]];
                    }
                    let px_data = &raw_payload[PALETTE_BYTES_I4..];
                    for y in 0..h {
                        let row_start = y * stride;
                        for x in 0..w {
                            let byte_off = row_start + x / 2;
                            if byte_off < px_data.len() {
                                let byte_val = px_data[byte_off];
                                let idx = if x % 2 == 0 {
                                    (byte_val >> 4) & 0x0F
                                } else {
                                    byte_val & 0x0F
                                };
                                let color = palette[idx as usize];
                                img.put_pixel(x as u32, y as u32, image::Rgba(color));
                            }
                        }
                    }
                }

                ImageFormatKind::Lvgl9A4 | ImageFormatKind::Lvgl9A4Rle => {
                    for y in 0..h {
                        let row_start = y * stride;
                        for x in 0..w {
                            let byte_off = row_start + x / 2;
                            if byte_off < raw_payload.len() {
                                let byte_val = raw_payload[byte_off];
                                let val = if x % 2 == 0 {
                                    (byte_val >> 4) & 0x0F
                                } else {
                                    byte_val & 0x0F
                                };
                                let alpha = val * 17;
                                img.put_pixel(
                                    x as u32,
                                    y as u32,
                                    image::Rgba([255, 255, 255, alpha]),
                                );
                            }
                        }
                    }
                }

                _ => unreachable!(),
            }

            Ok((info, img))
        }
    }
}

pub fn decode_image_png(data: &[u8]) -> Result<(ImageInfo, Vec<u8>), String> {
    let (info, img) = decode_to_rgba(data)?;
    let mut cursor = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(img)
        .write_to(&mut cursor, ImageFormat::Png)
        .map_err(|e| format!("PNG encoding failed: {e}"))?;
    Ok((info, cursor.into_inner()))
}

pub fn decode_thumbnail_png(
    data: &[u8],
    max_dimension: u32,
) -> Result<(ImageInfo, Vec<u8>), String> {
    let (info, img) = decode_to_rgba(data)?;
    let source_width = u32::from(info.width.max(1));
    let source_height = u32::from(info.height.max(1));
    let longest_side = source_width.max(source_height);
    let scale_numerator = max_dimension.min(MAX_THUMBNAIL_DIMENSION).min(longest_side);
    let target_width = (source_width * scale_numerator / longest_side).max(1);
    let target_height = (source_height * scale_numerator / longest_side).max(1);

    let thumbnail = if target_width == source_width && target_height == source_height {
        img
    } else {
        image::imageops::resize(
            &img,
            target_width,
            target_height,
            image::imageops::FilterType::Lanczos3,
        )
    };

    let mut cursor = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(thumbnail)
        .write_to(&mut cursor, ImageFormat::Png)
        .map_err(|e| format!("PNG encoding failed: {e}"))?;
    Ok((info, cursor.into_inner()))
}

pub fn decode_i8_png(data: &[u8]) -> Result<(ImageInfo, Vec<u8>), String> {
    decode_image_png(data)
}

pub fn decode_i8_thumbnail_png(
    data: &[u8],
    max_dimension: u32,
) -> Result<(ImageInfo, Vec<u8>), String> {
    decode_thumbnail_png(data, max_dimension)
}

pub fn decode_i8_thumbnail_reader<R: Read>(
    mut reader: R,
    image_size: usize,
    max_dimension: u32,
) -> Result<(ImageInfo, Vec<u8>), String> {
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(image_size)
        .map_err(|e| format!("alloc failed: {e}"))?;
    buffer.resize(image_size, 0);
    reader
        .read_exact(&mut buffer)
        .map_err(|e| format!("read failed: {e}"))?;
    decode_thumbnail_png(&buffer, max_dimension)
}

pub fn encode_png_to_template_detailed(
    png_bytes: &[u8],
    template: &[u8],
    allow_quantize: bool,
) -> Result<EncodedImage, String> {
    encode_png_to_template_with_filter(
        png_bytes,
        template,
        allow_quantize,
        ResizeFilter::default(),
    )
}

pub fn encode_png_to_template_with_filter(
    png_bytes: &[u8],
    template: &[u8],
    allow_quantize: bool,
    filter: ResizeFilter,
) -> Result<EncodedImage, String> {
    if png_bytes.len() > MAX_PNG_BYTES {
        return Err("PNG input exceeds the 64 MiB conversion limit".into());
    }

    let template_info =
        inspect_image(template).ok_or_else(|| "unsupported image template format".to_string())?;
    validate_rle_payload(template, template_info)?;

    let width = u32::from(template_info.width);
    let height = u32::from(template_info.height);
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
        return Err("image template dimensions exceed conversion limits".into());
    }

    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_PIXELS as u32);
    limits.max_image_height = Some(MAX_IMAGE_PIXELS as u32);
    limits.max_alloc = Some(MAX_IMAGE_PIXELS * 16 + 1024 * 1024);
    let mut reader = ImageReader::with_format(Cursor::new(png_bytes), ImageFormat::Png);
    reader.limits(limits.clone());
    let (source_width, source_height) = reader
        .into_dimensions()
        .map_err(|error| format!("PNG decode failed: {error}"))?;
    let source_pixels = u64::from(source_width) * u64::from(source_height);
    if source_width == 0 || source_height == 0 || source_pixels > MAX_IMAGE_PIXELS {
        return Err("PNG dimensions exceed the 16-megapixel conversion limit".into());
    }
    if u64::from(source_width) * u64::from(height) != u64::from(source_height) * u64::from(width) {
        return Err(format!(
            "PNG aspect ratio does not match the original image ({source_width}x{source_height} -> {width}x{height}); only proportional resizing is supported"
        ));
    }

    limits.max_image_width = Some(source_width);
    limits.max_image_height = Some(source_height);
    limits.max_alloc = Some(source_pixels * 16 + 1024 * 1024);
    let mut reader = ImageReader::with_format(Cursor::new(png_bytes), ImageFormat::Png);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|error| format!("PNG decode failed: {error}"))?
        .to_rgba8();
    let resized = (source_width, source_height) != (width, height);
    let image = if resized {
        image::imageops::resize(&image, width, height, filter.to_image_filter())
    } else {
        image
    };

    match template_info.format {
        ImageFormatKind::Png => {
            let bytes = if resized {
                let mut cursor = Cursor::new(Vec::new());
                DynamicImage::ImageRgba8(image)
                    .write_to(&mut cursor, ImageFormat::Png)
                    .map_err(|error| format!("PNG encoding failed: {error}"))?;
                cursor.into_inner()
            } else {
                png_bytes.to_vec()
            };
            Ok(EncodedImage {
                bytes,
                lossy_quantization: false,
            })
        }

        ImageFormatKind::Jpeg => {
            let mut cursor = Cursor::new(Vec::new());
            DynamicImage::ImageRgba8(image)
                .to_rgb8()
                .write_to(&mut cursor, ImageFormat::Jpeg)
                .map_err(|e| format!("JPEG encode failed: {e}"))?;
            Ok(EncodedImage {
                bytes: cursor.into_inner(),
                lossy_quantization: true,
            })
        }

        ImageFormatKind::Lvgl8Rgb565 => {
            let mut out = Vec::with_capacity(4 + (width as usize) * (height as usize) * 2);
            out.extend_from_slice(&template[..4]);
            for y in 0..height {
                for x in 0..width {
                    let px = image.get_pixel(x, y);
                    let r16 = (u32::from(px[0]) * 31 + 127) / 255;
                    let g16 = (u32::from(px[1]) * 63 + 127) / 255;
                    let b16 = (u32::from(px[2]) * 31 + 127) / 255;
                    let val16 = ((r16 as u16) << 11) | ((g16 as u16) << 5) | (b16 as u16);
                    out.extend_from_slice(&val16.to_le_bytes());
                }
            }
            Ok(EncodedImage {
                bytes: out,
                lossy_quantization: true,
            })
        }

        ImageFormatKind::Lvgl8I8 => {
            let rgba = image.into_raw();
            let (colors, counts) = unique_colors(&rgba)?;
            let (palette, lookup, lossy) = if colors.len() <= 256 {
                let mut lookup = HashMap::with_capacity(colors.len());
                for (index, color) in colors.iter().enumerate() {
                    lookup.insert(*color, index as u8);
                }
                (colors, lookup, false)
            } else {
                if !allow_quantize {
                    return Err(format!(
                        "PNG has {} distinct RGBA colors; enable lossy quantization to fit the 256-color palette",
                        colors.len()
                    ));
                }
                let (palette, lookup) = median_cut_palette(colors, counts, 256)?;
                (palette, lookup, true)
            };

            let mut out = vec![0u8; 4 + PALETTE_BYTES_I8 + (width as usize) * (height as usize)];
            out[..4].copy_from_slice(&template[..4]);
            for (i, color) in palette.into_iter().take(256).enumerate() {
                let at = 4 + i * 4;
                out[at..at + 4].copy_from_slice(&[color[2], color[1], color[0], color[3]]);
            }
            let px_start = 4 + PALETTE_BYTES_I8;
            for (i, pixel) in rgba.as_chunks::<4>().0.iter().enumerate() {
                out[px_start + i] = lookup[&pixel_color(pixel)];
            }
            Ok(EncodedImage {
                bytes: out,
                lossy_quantization: lossy,
            })
        }

        ImageFormatKind::Lvgl9I8 | ImageFormatKind::Lvgl9I8Rle => {
            let (_orig_info, orig_img) = decode_to_rgba(template)?;
            if image == orig_img {
                return Ok(EncodedImage {
                    bytes: template.to_vec(),
                    lossy_quantization: false,
                });
            }

            let rgba = image.into_raw();
            let orig_payload = if template_info.format.is_rle() {
                let comp_len =
                    u32::from_le_bytes([template[16], template[17], template[18], template[19]])
                        as usize;
                let raw_len =
                    u32::from_le_bytes([template[20], template[21], template[22], template[23]])
                        as usize;
                let payload = rle_decompress(&template[24..24 + comp_len], raw_len);
                if payload.len() != raw_len {
                    return Err("truncated LVGL9 RLE expanded payload".into());
                }
                payload
            } else {
                template[12..].to_vec()
            };

            let mut orig_lookup = HashMap::<Rgba, u8>::with_capacity(256);
            if orig_payload.len() >= PALETTE_BYTES_I8 {
                for (idx, entry) in orig_payload[..PALETTE_BYTES_I8]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .enumerate()
                    .rev()
                {
                    orig_lookup.insert([entry[2], entry[1], entry[0], entry[3]], idx as u8);
                }
            }

            let all_in_orig_palette = rgba
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| orig_lookup.contains_key(&pixel_color(pixel)));

            let (indices, palette, lossy) = if all_in_orig_palette {
                let indices = rgba
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|pixel| orig_lookup[&pixel_color(pixel)])
                    .collect::<Vec<_>>();
                (indices, None, false)
            } else {
                let (colors, counts) = unique_colors(&rgba)?;
                let (palette, lookup, lossy) = if colors.len() <= 256 {
                    let mut lookup = HashMap::with_capacity(colors.len());
                    for (index, color) in colors.iter().enumerate() {
                        lookup.insert(*color, index as u8);
                    }
                    (colors, lookup, false)
                } else {
                    if !allow_quantize {
                        return Err(format!(
                            "PNG has {} distinct RGBA colors; enable lossy quantization to fit the 256-color palette",
                            colors.len()
                        ));
                    }
                    let (palette, lookup) = median_cut_palette(colors, counts, 256)?;
                    (palette, lookup, true)
                };
                let indices = rgba
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|pixel| lookup[&pixel_color(pixel)])
                    .collect::<Vec<_>>();
                (indices, Some(palette), lossy)
            };

            let mut raw_payload = orig_payload;
            if raw_payload.len()
                < PALETTE_BYTES_I8
                    + usize::from(template_info.stride) * usize::from(template_info.height)
            {
                raw_payload.resize(
                    PALETTE_BYTES_I8
                        + usize::from(template_info.stride) * usize::from(template_info.height),
                    0,
                );
            }

            if let Some(palette) = palette {
                raw_payload[..PALETTE_BYTES_I8].fill(0);
                for (i, color) in palette.into_iter().take(256).enumerate() {
                    let at = i * 4;
                    raw_payload[at..at + 4]
                        .copy_from_slice(&[color[2], color[1], color[0], color[3]]);
                }
            }

            let width = usize::from(template_info.width);
            let stride = usize::from(template_info.stride);
            for row in 0..usize::from(template_info.height) {
                let src = row * width;
                let dst = PALETTE_BYTES_I8 + row * stride;
                raw_payload[dst..dst + width].copy_from_slice(&indices[src..src + width]);
            }

            let bytes = assemble_lvgl9_output(template, template_info, &raw_payload);
            Ok(EncodedImage {
                bytes,
                lossy_quantization: lossy,
            })
        }

        ImageFormatKind::Lvgl9I4 | ImageFormatKind::Lvgl9I4Rle => {
            let rgba = image.into_raw();
            let stride = usize::from(template_info.stride);
            let (colors, counts) = unique_colors(&rgba)?;
            let (palette, lookup, lossy) = if colors.len() <= 16 {
                let mut lookup = HashMap::with_capacity(colors.len());
                for (index, color) in colors.iter().enumerate() {
                    lookup.insert(*color, index as u8);
                }
                (colors, lookup, false)
            } else {
                if !allow_quantize {
                    return Err(format!(
                        "PNG has {} distinct RGBA colors; enable lossy quantization to fit the 16-color palette",
                        colors.len()
                    ));
                }
                let (palette, lookup) = median_cut_palette(colors, counts, 16)?;
                (palette, lookup, true)
            };

            let mut raw_payload =
                vec![0u8; PALETTE_BYTES_I4 + stride * usize::from(template_info.height)];
            for (i, color) in palette.into_iter().take(16).enumerate() {
                let at = i * 4;
                raw_payload[at..at + 4].copy_from_slice(&[color[2], color[1], color[0], color[3]]);
            }
            for y in 0..usize::from(template_info.height) {
                let row_px_start = y * usize::from(template_info.width);
                let row_out_start = PALETTE_BYTES_I4 + y * stride;
                for x in 0..usize::from(template_info.width) {
                    let pixel = rgba.as_chunks::<4>().0[row_px_start + x];
                    let idx = lookup[&pixel_color(&pixel)] & 0x0F;
                    let byte_idx = row_out_start + x / 2;
                    if x % 2 == 0 {
                        raw_payload[byte_idx] |= idx << 4;
                    } else {
                        raw_payload[byte_idx] |= idx;
                    }
                }
            }

            let bytes = assemble_lvgl9_output(template, template_info, &raw_payload);
            Ok(EncodedImage {
                bytes,
                lossy_quantization: lossy,
            })
        }

        ImageFormatKind::Lvgl9A8 | ImageFormatKind::Lvgl9A8Rle => {
            let stride = usize::from(template_info.stride);
            let h = usize::from(template_info.height);
            let w = usize::from(template_info.width);
            let mut raw_payload = vec![0u8; stride * h];

            let has_transparency = image.pixels().any(|px| px[3] < 255);
            for y in 0..h {
                let row_start = y * stride;
                for x in 0..w {
                    let px = image.get_pixel(x as u32, y as u32);
                    let alpha = if has_transparency {
                        px[3]
                    } else {
                        ((u32::from(px[0]) + u32::from(px[1]) + u32::from(px[2])) / 3) as u8
                    };
                    raw_payload[row_start + x] = alpha;
                }
            }

            let bytes = assemble_lvgl9_output(template, template_info, &raw_payload);
            Ok(EncodedImage {
                bytes,
                lossy_quantization: false,
            })
        }

        ImageFormatKind::Lvgl9A4 | ImageFormatKind::Lvgl9A4Rle => {
            let stride = usize::from(template_info.stride);
            let h = usize::from(template_info.height);
            let w = usize::from(template_info.width);
            let mut raw_payload = vec![0u8; stride * h];

            let has_transparency = image.pixels().any(|px| px[3] < 255);
            for y in 0..h {
                let row_start = y * stride;
                for x in 0..w {
                    let px = image.get_pixel(x as u32, y as u32);
                    let alpha = if has_transparency {
                        px[3]
                    } else {
                        ((u32::from(px[0]) + u32::from(px[1]) + u32::from(px[2])) / 3) as u8
                    };
                    let val = ((u16::from(alpha) * 15 + 127) / 255) as u8;
                    let byte_idx = row_start + x / 2;
                    if x % 2 == 0 {
                        raw_payload[byte_idx] |= val << 4;
                    } else {
                        raw_payload[byte_idx] |= val;
                    }
                }
            }

            let bytes = assemble_lvgl9_output(template, template_info, &raw_payload);
            Ok(EncodedImage {
                bytes,
                lossy_quantization: true,
            })
        }

        ImageFormatKind::Lvgl9Argb8888 | ImageFormatKind::Lvgl9Argb8888Rle => {
            let stride = usize::from(template_info.stride);
            let h = usize::from(template_info.height);
            let w = usize::from(template_info.width);
            let mut raw_payload = vec![0u8; stride * h];

            for y in 0..h {
                let row_start = y * stride;
                for x in 0..w {
                    let px = image.get_pixel(x as u32, y as u32);
                    let off = row_start + x * 4;
                    raw_payload[off] = px[2]; // B
                    raw_payload[off + 1] = px[1]; // G
                    raw_payload[off + 2] = px[0]; // R
                    raw_payload[off + 3] = px[3]; // A
                }
            }

            let bytes = assemble_lvgl9_output(template, template_info, &raw_payload);
            Ok(EncodedImage {
                bytes,
                lossy_quantization: false,
            })
        }
    }
}

fn assemble_lvgl9_output(template: &[u8], info: ImageInfo, raw_payload: &[u8]) -> Vec<u8> {
    let mut header = [0u8; 12];
    header.copy_from_slice(&template[..12]);
    if info.format.is_rle() {
        header[2..4].copy_from_slice(&8u16.to_le_bytes()); // flags = 8
        let comp_bytes = rle_compress(raw_payload);
        let comp_method = 1u32;
        let comp_len = comp_bytes.len() as u32;
        let raw_len = raw_payload.len() as u32;

        let mut out = Vec::with_capacity(24 + comp_bytes.len());
        out.extend_from_slice(&header);
        out.extend_from_slice(&comp_method.to_le_bytes());
        out.extend_from_slice(&comp_len.to_le_bytes());
        out.extend_from_slice(&raw_len.to_le_bytes());
        out.extend_from_slice(&comp_bytes);
        out
    } else {
        header[2..4].copy_from_slice(&0u16.to_le_bytes()); // flags = 0
        let mut out = Vec::with_capacity(12 + raw_payload.len());
        out.extend_from_slice(&header);
        out.extend_from_slice(raw_payload);
        out
    }
}

pub fn encode_png_to_template(
    png_bytes: &[u8],
    template: &[u8],
    allow_quantize: bool,
) -> Result<Vec<u8>, String> {
    encode_png_to_template_detailed(png_bytes, template, allow_quantize)
        .map(|encoded| encoded.bytes)
}

pub fn encode_png_i8(
    png_bytes: &[u8],
    template: &[u8],
    allow_quantize: bool,
) -> Result<Vec<u8>, String> {
    encode_png_to_template(png_bytes, template, allow_quantize)
}

pub fn encode_png_i8_detailed(
    png_bytes: &[u8],
    template: &[u8],
    allow_quantize: bool,
) -> Result<EncodedImage, String> {
    encode_png_to_template_detailed(png_bytes, template, allow_quantize)
}

fn unique_colors(rgba: &[u8]) -> Result<(Vec<Rgba>, Vec<u64>), String> {
    let mut counts = HashMap::<Rgba, u64>::new();
    for pixel in rgba.as_chunks::<4>().0 {
        let color = pixel_color(pixel);
        let count = counts.entry(color).or_default();
        *count += 1;
        if counts.len() > MAX_UNIQUE_COLORS {
            return Err("PNG contains too many distinct colors to quantize safely".into());
        }
    }
    let mut colors = Vec::with_capacity(counts.len());
    let mut frequencies = Vec::with_capacity(counts.len());
    for (color, count) in counts {
        colors.push(color);
        frequencies.push(count);
    }
    Ok((colors, frequencies))
}

#[derive(Clone)]
struct ColorCount {
    color: Rgba,
    count: u64,
}

fn median_cut_palette(
    colors: Vec<Rgba>,
    counts: Vec<u64>,
    max_colors: usize,
) -> Result<(Vec<Rgba>, HashMap<Rgba, u8>), String> {
    let initial = colors
        .into_iter()
        .zip(counts)
        .map(|(color, count)| ColorCount { color, count })
        .collect::<Vec<_>>();
    let mut boxes = vec![initial];

    while boxes.len() < max_colors {
        let candidate = boxes
            .iter()
            .enumerate()
            .filter(|(_, colors)| colors.len() > 1)
            .map(|(index, colors)| {
                let (channel, range) = widest_channel(colors);
                (index, channel, u64::from(range) * colors.len() as u64)
            })
            .max_by_key(|(_, _, score)| *score);
        let Some((index, channel, _)) = candidate else {
            break;
        };

        let mut current = boxes.swap_remove(index);
        current.sort_by_key(|item| item.color[channel]);
        let total = current.iter().map(|item| item.count).sum::<u64>();
        let mut cumulative = 0u64;
        let mut split = 1usize;
        for (position, item) in current.iter().enumerate().take(current.len() - 1) {
            cumulative += item.count;
            split = position + 1;
            if cumulative.saturating_mul(2) >= total {
                break;
            }
        }
        let right = current.split_off(split);
        boxes.push(current);
        boxes.push(right);
    }

    let mut palette = Vec::with_capacity(boxes.len());
    let mut lookup = HashMap::new();
    for (index, bucket) in boxes.into_iter().enumerate() {
        let total = bucket.iter().map(|item| item.count).sum::<u64>().max(1);
        let mut sums = [0u64; 4];
        for item in &bucket {
            for (channel, component) in item.color.iter().enumerate() {
                sums[channel] += u64::from(*component) * item.count;
            }
        }
        let color = sums.map(|value| ((value + total / 2) / total) as u8);
        for item in bucket {
            lookup.insert(item.color, index as u8);
        }
        palette.push(color);
    }
    if palette.is_empty() || palette.len() > max_colors {
        return Err("could not construct an LVGL palette".into());
    }
    Ok((palette, lookup))
}

fn widest_channel(colors: &[ColorCount]) -> (usize, u8) {
    let mut min = [u8::MAX; 4];
    let mut max = [u8::MIN; 4];
    for item in colors {
        for channel in 0..4 {
            min[channel] = min[channel].min(item.color[channel]);
            max[channel] = max[channel].max(item.color[channel]);
        }
    }
    let channel = (0..4)
        .max_by_key(|index| max[*index] - min[*index])
        .unwrap_or(0);
    (channel, max[channel] - min[channel])
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::GenericImageView;

    fn sample_template() -> Vec<u8> {
        let width = 2u16;
        let height = 2u16;
        let stride = 4u16;
        let pixel_offset = HEADER_BYTES_V9 + PALETTE_BYTES_I8;
        let mut data = vec![0u8; pixel_offset + usize::from(stride * height)];
        data[0] = 0x19;
        data[1] = 0x0a;
        data[4..6].copy_from_slice(&width.to_le_bytes());
        data[6..8].copy_from_slice(&height.to_le_bytes());
        data[8..10].copy_from_slice(&stride.to_le_bytes());
        data[12..16].copy_from_slice(&[0, 0, 0, 255]);
        data[16..20].copy_from_slice(&[0, 0, 255, 255]);
        data[20..24].copy_from_slice(&[0, 255, 0, 255]);
        data[24..28].copy_from_slice(&[255, 0, 0, 255]);
        data[pixel_offset..pixel_offset + 2].copy_from_slice(&[0, 1]);
        data[pixel_offset + 4..pixel_offset + 6].copy_from_slice(&[2, 3]);
        data[pixel_offset + 2..pixel_offset + 4].copy_from_slice(&[77, 88]);
        data[pixel_offset + 6..pixel_offset + 8].copy_from_slice(&[99, 111]);
        data
    }

    fn png_bytes(image: RgbaImage) -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(image)
            .write_to(&mut cursor, ImageFormat::Png)
            .unwrap();
        cursor.into_inner()
    }

    #[test]
    fn proportional_downscale_preserves_pixels_header_and_stride_padding() {
        let template = sample_template();
        let colors = [
            image::Rgba([255, 0, 0, 255]),
            image::Rgba([0, 255, 0, 255]),
            image::Rgba([0, 0, 255, 255]),
            image::Rgba([0, 0, 0, 255]),
        ];
        let png = png_bytes(RgbaImage::from_fn(4, 4, |x, y| {
            colors[(y / 2 * 2 + x / 2) as usize]
        }));
        let encoded = encode_png_to_template_with_filter(&png, &template, false, ResizeFilter::Nearest).unwrap();
        assert!(!encoded.lossy_quantization);
        assert_eq!(
            &encoded.bytes[..HEADER_BYTES_V9],
            &template[..HEADER_BYTES_V9]
        );
        let pixel_offset = HEADER_BYTES_V9 + PALETTE_BYTES_I8;
        assert_eq!(
            &encoded.bytes[pixel_offset..],
            &[1, 2, 77, 88, 3, 0, 99, 111]
        );
        let (_, decoded) = decode_to_rgba(&encoded.bytes).unwrap();
        assert_eq!(decoded.dimensions(), (2, 2));
        assert_eq!(decoded.pixels().copied().collect::<Vec<_>>(), colors);
    }

    #[test]
    fn proportional_downscale_with_default_lanczos3_filter() {
        let template = sample_template();
        let pixel = image::Rgba([255, 0, 0, 255]);
        let png = png_bytes(RgbaImage::from_pixel(4, 4, pixel));
        let encoded = encode_png_to_template_detailed(&png, &template, false).unwrap();
        assert_eq!(encoded.bytes[..HEADER_BYTES_V9], template[..HEADER_BYTES_V9]);
        assert_eq!(encoded.bytes.len(), template.len());
        assert!(!encoded.lossy_quantization);
        let (_, decoded) = decode_to_rgba(&encoded.bytes).unwrap();
        assert_eq!(decoded.dimensions(), (2, 2));
        assert_eq!(decoded.get_pixel(0, 0), &pixel);
    }

    #[test]
    fn proportional_resize_supports_all_template_formats() {
        let pixel = image::Rgba([255, 0, 0, 128]);
        let same_size = png_bytes(RgbaImage::from_pixel(2, 2, pixel));
        let inputs = [
            png_bytes(RgbaImage::from_pixel(1, 1, pixel)),
            png_bytes(RgbaImage::from_pixel(4, 4, pixel)),
        ];
        let mut templates = Vec::new();
        for (cf, stride, palette_bytes) in [
            (0x0a, 4u16, PALETTE_BYTES_I8),
            (0x09, 2, PALETTE_BYTES_I4),
            (0x0e, 4, 0),
            (0x0d, 2, 0),
            (0x10, 12, 0),
        ] {
            let mut template = vec![0u8; HEADER_BYTES_V9 + palette_bytes + usize::from(stride) * 2];
            template[0] = 0x19;
            template[1] = cf;
            template[4..6].copy_from_slice(&2u16.to_le_bytes());
            template[6..8].copy_from_slice(&2u16.to_le_bytes());
            template[8..10].copy_from_slice(&stride.to_le_bytes());
            let mut info = inspect_image(&template).unwrap();
            info.format = match cf {
                0x0a => ImageFormatKind::Lvgl9I8Rle,
                0x09 => ImageFormatKind::Lvgl9I4Rle,
                0x0e => ImageFormatKind::Lvgl9A8Rle,
                0x0d => ImageFormatKind::Lvgl9A4Rle,
                0x10 => ImageFormatKind::Lvgl9Argb8888Rle,
                _ => unreachable!(),
            };
            templates.push(assemble_lvgl9_output(
                &template,
                info,
                &template[HEADER_BYTES_V9..],
            ));
            templates.push(template);
        }
        for (cf, payload_bytes) in [(4u32, 8), (10, PALETTE_BYTES_I8 + 4)] {
            let header = cf | (2 << 10) | (2 << 21);
            let mut template = header.to_le_bytes().to_vec();
            template.resize(4 + payload_bytes, 0);
            templates.push(template);
        }
        templates.push(same_size.clone());
        let mut jpeg = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(2, 2, pixel))
            .to_rgb8()
            .write_to(&mut jpeg, ImageFormat::Jpeg)
            .unwrap();
        templates.push(jpeg.into_inner());

        for template in templates {
            let info = inspect_image(&template).unwrap();
            let baseline = encode_png_to_template_detailed(&same_size, &template, false).unwrap();
            let (_, expected) = decode_to_rgba(&baseline.bytes).unwrap();
            for input in &inputs {
                let encoded = encode_png_to_template_detailed(input, &template, false).unwrap();
                assert_eq!(inspect_image(&encoded.bytes), Some(info));
                assert_eq!(encoded.lossy_quantization, baseline.lossy_quantization);
                let (_, actual) = decode_to_rgba(&encoded.bytes).unwrap();
                assert_eq!(actual, expected, "resize failed for {:?}", info.format);
            }
        }
    }

    #[test]
    fn proportional_resize_handles_non_square_images_and_preserves_transparency() {
        let pixel = image::Rgba([123, 45, 67, 89]);
        let template = png_bytes(RgbaImage::from_pixel(2, 1, image::Rgba([0, 0, 0, 0])));
        for (width, height) in [(6, 3), (4, 2)] {
            let input = png_bytes(RgbaImage::from_pixel(width, height, pixel));
            let encoded = encode_png_to_template(&input, &template, false).unwrap();
            let (_, actual) = decode_to_rgba(&encoded).unwrap();
            assert_eq!(actual, RgbaImage::from_pixel(2, 1, pixel));
        }
    }

    #[test]
    fn aspect_ratio_mismatch_is_rejected_even_with_quantization_enabled() {
        let template = sample_template();
        for (width, height) in [(3, 2), (1, 2), (4, 3)] {
            let input = png_bytes(RgbaImage::new(width, height));
            for allow_quantize in [false, true] {
                let error = encode_png_to_template(&input, &template, allow_quantize).unwrap_err();
                assert!(error.contains("aspect ratio"));
                assert!(error.contains(&format!("{width}x{height} -> 2x2")));
            }
        }
    }

    #[test]
    fn same_size_png_template_preserves_input_bytes() {
        let input = png_bytes(RgbaImage::from_pixel(2, 2, image::Rgba([1, 2, 3, 4])));
        let template = png_bytes(RgbaImage::new(2, 2));
        assert_eq!(
            encode_png_to_template(&input, &template, false).unwrap(),
            input
        );
    }

    #[test]
    fn oversized_png_is_rejected_before_decoding_pixels() {
        let mut input = png_bytes(RgbaImage::new(1, 1));
        input[16..20].copy_from_slice(&4097u32.to_be_bytes());
        input[20..24].copy_from_slice(&4097u32.to_be_bytes());
        // Update the IHDR CRC so the header is valid without allocating a large bitmap.
        let mut crc = u32::MAX;
        for &byte in &input[12..29] {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(crc & 1));
            }
        }
        input[29..33].copy_from_slice(&(!crc).to_be_bytes());
        let error = encode_png_to_template(&input, &sample_template(), false).unwrap_err();
        assert!(error.contains("16-megapixel"), "unexpected error: {error}");
    }

    #[test]
    fn forged_rle_expansion_is_rejected_before_decoding_or_conversion() {
        let mut template = vec![0; 26];
        template[0] = 0x19;
        template[1] = 0x0a;
        template[2] = 8;
        template[4..6].copy_from_slice(&1u16.to_le_bytes());
        template[6..8].copy_from_slice(&1u16.to_le_bytes());
        template[8..10].copy_from_slice(&1u16.to_le_bytes());
        template[16..20].copy_from_slice(&2u32.to_le_bytes());
        template[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
        template[24] = 1;
        assert!(
            decode_to_rgba(&template)
                .unwrap_err()
                .contains("RLE expanded payload")
        );
        let png = png_bytes(RgbaImage::new(1, 1));
        assert!(
            encode_png_to_template(&png, &template, false)
                .unwrap_err()
                .contains("RLE expanded payload")
        );
    }

    #[test]
    fn rle_vendor_padding_is_valid_but_truncated_expansion_is_not() {
        let original = sample_template();
        let mut payload = original[12..].to_vec();
        payload.push(0);
        let compressed = rle_compress(&payload);
        let mut padded = original[..12].to_vec();
        padded[2] = 8;
        padded.extend_from_slice(&1u32.to_le_bytes());
        padded.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        padded.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        padded.extend_from_slice(&compressed);
        let (_, expected) = decode_to_rgba(&original).unwrap();
        let (_, actual) = decode_to_rgba(&padded).unwrap();
        assert_eq!(actual, expected);
        let png = png_bytes(expected.clone());
        let converted = encode_png_to_template(&png, &padded, false).unwrap();
        assert_eq!(decode_to_rgba(&converted).unwrap().1, expected);
        padded[20..24].copy_from_slice(&((payload.len() + 7) as u32).to_le_bytes());
        assert!(decode_to_rgba(&padded).unwrap_err().contains("truncated"));
        assert!(
            encode_png_to_template(&png, &padded, false)
                .unwrap_err()
                .contains("truncated")
        );
    }

    #[test]
    fn raster_metadata_does_not_truncate_large_png_dimensions() {
        let mut header = vec![0; 24];
        header[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        header[12..16].copy_from_slice(b"IHDR");
        header[16..20].copy_from_slice(&65536u32.to_be_bytes());
        header[20..24].copy_from_slice(&1u32.to_be_bytes());
        assert!(inspect_image(&header).is_none());
        assert!(decode_to_rgba(&header).is_err());
    }

    #[test]
    fn invalid_png_is_rejected() {
        assert!(encode_png_to_template(b"not a PNG", &sample_template(), false).is_err());
    }

    #[test]
    fn header_inspection_validates_lazy_image_metadata() {
        let template = sample_template();
        let header = &template[..12];
        assert_eq!(
            inspect_image_header(header, template.len()),
            Some(ImageInfo {
                format: ImageFormatKind::Lvgl9I8,
                width: 2,
                height: 2,
                stride: 4,
            })
        );
        assert_eq!(inspect_image_header(header, template.len() - 1), None);
        assert_eq!(inspect_image_header(&header[..4], template.len()), None);
    }

    #[test]
    fn thumbnail_is_bounded_and_does_not_enlarge_small_images() {
        let template = sample_template();
        let (_, thumbnail_png) = decode_thumbnail_png(&template, 1).unwrap();
        let thumbnail = ImageReader::with_format(Cursor::new(thumbnail_png), ImageFormat::Png)
            .decode()
            .unwrap();
        assert_eq!(thumbnail.dimensions(), (1, 1));

        let (_, thumbnail_png) = decode_thumbnail_png(&template, 32).unwrap();
        let thumbnail = ImageReader::with_format(Cursor::new(thumbnail_png), ImageFormat::Png)
            .decode()
            .unwrap();
        assert_eq!(thumbnail.dimensions(), (2, 2));
    }

    #[test]
    fn i8_round_trip_preserves_template_bytes() {
        let template = sample_template();
        let (_, png) = decode_image_png(&template).unwrap();
        let restored = encode_png_to_template(&png, &template, false).unwrap();
        assert_eq!(restored, template);
    }

    #[test]
    fn rle_round_trip() {
        let data = b"AAAAABBBCCCCCCCCDEEEEEFFFFFFGGGGGGGGGGGGGGGGGGGG";
        let compressed = rle_compress(data);
        let decompressed = rle_decompress(&compressed, data.len());
        assert_eq!(&decompressed, data);
    }

    #[test]
    fn png_encoding_preserves_stride_padding_and_changes_pixels() {
        let template = sample_template();
        let pixel_offset = HEADER_BYTES_V9 + PALETTE_BYTES_I8;
        let image = RgbaImage::from_raw(
            2,
            2,
            vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 0, 0, 0, 255],
        )
        .unwrap();
        let mut cursor = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(image)
            .write_to(&mut cursor, ImageFormat::Png)
            .unwrap();
        let output = encode_png_to_template(&cursor.into_inner(), &template, false).unwrap();
        assert_eq!(&output[pixel_offset + 2..pixel_offset + 4], &[77, 88]);
        assert_eq!(&output[pixel_offset + 6..pixel_offset + 8], &[99, 111]);
        assert_eq!(
            inspect_image(&output).unwrap(),
            ImageInfo {
                format: ImageFormatKind::Lvgl9I8,
                width: 2,
                height: 2,
                stride: 4
            }
        );
    }

    #[test]
    fn thumbnail_downscales_large_images_with_a_hard_dimension_cap() {
        let width = 128u16;
        let height = 64u16;
        let stride = 128u16;
        let pixel_offset = HEADER_BYTES_V9 + PALETTE_BYTES_I8;
        let mut image = vec![0u8; pixel_offset + usize::from(width) * usize::from(height)];
        image[0] = 0x19;
        image[1] = 0x0a;
        image[4..6].copy_from_slice(&width.to_le_bytes());
        image[6..8].copy_from_slice(&height.to_le_bytes());
        image[8..10].copy_from_slice(&stride.to_le_bytes());
        image[12..16].copy_from_slice(&[0, 0, 255, 255]);

        let (_, thumbnail_png) = decode_thumbnail_png(&image, 28).unwrap();
        let thumbnail = ImageReader::with_format(Cursor::new(thumbnail_png), ImageFormat::Png)
            .decode()
            .unwrap()
            .to_rgba8();
        assert_eq!(thumbnail.dimensions(), (28, 14));
        assert_eq!(thumbnail.get_pixel(0, 0).0, [255, 0, 0, 255]);

        let (_, thumbnail_png) = decode_thumbnail_png(&image, u32::MAX).unwrap();
        let thumbnail = ImageReader::with_format(Cursor::new(thumbnail_png), ImageFormat::Png)
            .decode()
            .unwrap()
            .to_rgba8();
        assert_eq!(thumbnail.dimensions(), (64, 32));
    }

    #[test]
    fn lossy_conversion_requires_explicit_opt_in() {
        let pixel_offset = HEADER_BYTES_V9 + PALETTE_BYTES_I8;
        let mut template = vec![0u8; pixel_offset + 257];
        template[0] = 0x19;
        template[1] = 0x0a;
        template[4..6].copy_from_slice(&257u16.to_le_bytes());
        template[6..8].copy_from_slice(&1u16.to_le_bytes());
        template[8..10].copy_from_slice(&257u16.to_le_bytes());
        let mut pixels = Vec::new();
        for value in 0..257u16 {
            pixels.extend_from_slice(&[value as u8, (value >> 8) as u8, 37, 255]);
        }
        let image = RgbaImage::from_raw(257, 1, pixels).unwrap();
        let mut cursor = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(image)
            .write_to(&mut cursor, ImageFormat::Png)
            .unwrap();
        assert!(encode_png_to_template(cursor.get_ref(), &template, false).is_err());
        let quantized =
            encode_png_to_template_detailed(&cursor.into_inner(), &template, true).unwrap();
        assert!(quantized.lossy_quantization);
        assert_eq!(inspect_image(&quantized.bytes).unwrap().width, 257);
    }

    #[test]
    fn lvgl9_argb8888_and_rle_round_trip() {
        let width = 2u16;
        let height = 2u16;
        let stride = 8u16; // 2 * 4
        let mut original = vec![0u8; 12 + usize::from(stride * height)];
        original[0] = 0x19;
        original[1] = 0x10; // ARGB8888
        original[4..6].copy_from_slice(&width.to_le_bytes());
        original[6..8].copy_from_slice(&height.to_le_bytes());
        original[8..10].copy_from_slice(&stride.to_le_bytes());
        // Put 4 pixels: BGRA
        original[12..16].copy_from_slice(&[10, 20, 30, 255]);
        original[16..20].copy_from_slice(&[40, 50, 60, 200]);
        original[20..24].copy_from_slice(&[70, 80, 90, 150]);
        original[24..28].copy_from_slice(&[100, 110, 120, 100]);

        let (_, png) = decode_image_png(&original).unwrap();
        let restored = encode_png_to_template(&png, &original, false).unwrap();
        assert_eq!(restored, original);

        // Test with RLE compression enabled
        let rle_original = assemble_lvgl9_output(
            &original,
            ImageInfo {
                format: ImageFormatKind::Lvgl9Argb8888Rle,
                width,
                height,
                stride,
            },
            &original[12..],
        );
        let (_, rle_png) = decode_image_png(&rle_original).unwrap();
        assert_eq!(png, rle_png);
        let rle_restored = encode_png_to_template(&rle_png, &rle_original, false).unwrap();
        let (_, rle_restored_png) = decode_image_png(&rle_restored).unwrap();
        assert_eq!(rle_restored_png, png);
    }
}
