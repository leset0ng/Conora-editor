use std::collections::HashMap;
use std::io::Cursor;

use image::{DynamicImage, ImageFormat, ImageReader, Limits, RgbaImage};

const HEADER_BYTES: usize = 12;
const PALETTE_BYTES: usize = 256 * 4;
const PIXEL_OFFSET: usize = HEADER_BYTES + PALETTE_BYTES;
const MAX_IMAGE_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_PNG_BYTES: usize = 64 * 1024 * 1024;
const MAX_UNIQUE_COLORS: usize = 1_000_000;

type Rgba = [u8; 4];

fn pixel_color(pixel: &[u8; 4]) -> Rgba {
    *pixel
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct I8Info {
    pub width: u16,
    pub height: u16,
    pub stride: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodedI8 {
    pub bytes: Vec<u8>,
    pub lossy_quantization: bool,
}

#[derive(Clone, Debug)]
struct ParsedImage {
    info: I8Info,
    palette: [Rgba; 256],
    pixels: Vec<u8>,
}

pub fn inspect_i8(data: &[u8]) -> Option<I8Info> {
    inspect_i8_header(data, data.len())
}

pub fn inspect_i8_header(header: &[u8], image_size: usize) -> Option<I8Info> {
    parse_i8_header(header, image_size).ok()
}

fn parse_i8_header(header: &[u8], image_size: usize) -> Result<I8Info, String> {
    if header.len() < HEADER_BYTES {
        return Err("truncated LVGL I8 header".into());
    }
    if header[0] != 0x19 || header[1] != 0x0a {
        return Err("unsupported LVGL magic or color format".into());
    }

    let flags = u16::from_le_bytes([header[2], header[3]]);
    let width = u16::from_le_bytes([header[4], header[5]]);
    let height = u16::from_le_bytes([header[6], header[7]]);
    let stride = u16::from_le_bytes([header[8], header[9]]);
    let reserved = u16::from_le_bytes([header[10], header[11]]);
    if flags != 0 || reserved != 0 || width == 0 || height == 0 || stride < width {
        return Err("invalid LVGL I8 header".into());
    }

    let pixel_count = u64::from(width) * u64::from(height);
    if pixel_count > MAX_IMAGE_PIXELS {
        return Err("LVGL image exceeds the preview pixel limit".into());
    }
    let expected = PIXEL_OFFSET
        .checked_add(usize::from(stride) * usize::from(height))
        .ok_or_else(|| "LVGL image size overflow".to_string())?;
    if expected != image_size {
        return Err("LVGL image size does not match its header".into());
    }
    Ok(I8Info {
        width,
        height,
        stride,
    })
}

fn parse_i8(data: &[u8]) -> Result<ParsedImage, String> {
    if data.len() < PIXEL_OFFSET {
        return Err("truncated LVGL I8 image".into());
    }
    let info = parse_i8_header(data, data.len())?;
    let pixel_count = usize::from(info.width) * usize::from(info.height);

    let mut palette = [[0u8; 4]; 256];
    for (index, entry) in data[HEADER_BYTES..PIXEL_OFFSET]
        .as_chunks::<4>()
        .0
        .iter()
        .enumerate()
    {
        palette[index] = [entry[2], entry[1], entry[0], entry[3]];
    }

    let mut pixels = Vec::with_capacity(pixel_count);
    for row in 0..usize::from(info.height) {
        let start = PIXEL_OFFSET + row * usize::from(info.stride);
        pixels.extend_from_slice(&data[start..start + usize::from(info.width)]);
    }

    Ok(ParsedImage {
        info,
        palette,
        pixels,
    })
}

fn rgba_from_i8(parsed: &ParsedImage) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(parsed.pixels.len() * 4);
    for index in &parsed.pixels {
        rgba.extend_from_slice(&parsed.palette[usize::from(*index)]);
    }
    rgba
}

pub fn decode_i8_png(data: &[u8]) -> Result<(I8Info, Vec<u8>), String> {
    let parsed = parse_i8(data)?;
    let rgba = rgba_from_i8(&parsed);
    let image = RgbaImage::from_raw(
        u32::from(parsed.info.width),
        u32::from(parsed.info.height),
        rgba,
    )
    .ok_or_else(|| "could not construct the decoded image".to_string())?;

    let mut cursor = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(image)
        .write_to(&mut cursor, ImageFormat::Png)
        .map_err(|error| format!("PNG encoding failed: {error}"))?;
    Ok((parsed.info, cursor.into_inner()))
}

pub fn encode_png_i8(
    png_bytes: &[u8],
    template: &[u8],
    allow_quantize: bool,
) -> Result<Vec<u8>, String> {
    encode_png_i8_detailed(png_bytes, template, allow_quantize).map(|encoded| encoded.bytes)
}

pub fn encode_png_i8_detailed(
    png_bytes: &[u8],
    template: &[u8],
    allow_quantize: bool,
) -> Result<EncodedI8, String> {
    if png_bytes.len() > MAX_PNG_BYTES {
        return Err("PNG input exceeds the 64 MiB conversion limit".into());
    }
    let parsed = parse_i8(template)?;
    let width = u32::from(parsed.info.width);
    let height = u32::from(parsed.info.height);
    let max_alloc = u64::from(width) * u64::from(height) * 16 + 1024 * 1024;
    let mut reader = ImageReader::with_format(Cursor::new(png_bytes), ImageFormat::Png);
    let mut limits = Limits::default();
    limits.max_image_width = Some(width);
    limits.max_image_height = Some(height);
    limits.max_alloc = Some(max_alloc);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|error| format!("PNG decode failed: {error}"))?
        .to_rgba8();
    if image.dimensions() != (width, height) {
        return Err(format!(
            "PNG must be exactly {}x{}; resizing is disabled",
            parsed.info.width, parsed.info.height
        ));
    }
    let rgba = image.into_raw();
    let original_rgba = rgba_from_i8(&parsed);
    if rgba == original_rgba {
        return Ok(EncodedI8 {
            bytes: template.to_vec(),
            lossy_quantization: false,
        });
    }

    let mut original_lookup = HashMap::<Rgba, u8>::with_capacity(256);
    for index in (0..256).rev() {
        original_lookup.insert(parsed.palette[index], index as u8);
    }

    if rgba
        .as_chunks::<4>()
        .0
        .iter()
        .all(|pixel| original_lookup.contains_key(&pixel_color(pixel)))
    {
        let indices = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| original_lookup[&pixel_color(pixel)])
            .collect::<Vec<_>>();
        return write_indices(template, parsed.info, &indices, None).map(|bytes| EncodedI8 {
            bytes,
            lossy_quantization: false,
        });
    }

    let (palette, lookup, lossy_quantization) = {
        let (colors, counts) = unique_colors(&rgba)?;
        if colors.len() <= 256 {
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
            let (palette, lookup) = median_cut_palette(colors, counts)?;
            (palette, lookup, true)
        }
    };

    let indices = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .map(|pixel| lookup[&pixel_color(pixel)])
        .collect::<Vec<_>>();
    let bytes = write_indices(template, parsed.info, &indices, Some(palette))?;
    Ok(EncodedI8 {
        bytes,
        lossy_quantization,
    })
}

fn write_indices(
    template: &[u8],
    info: I8Info,
    indices: &[u8],
    palette: Option<Vec<Rgba>>,
) -> Result<Vec<u8>, String> {
    let expected_pixels = usize::from(info.width) * usize::from(info.height);
    if indices.len() != expected_pixels {
        return Err("converted image pixel count is inconsistent".into());
    }
    let mut output = template.to_vec();
    if let Some(palette) = palette {
        output[HEADER_BYTES..PIXEL_OFFSET].fill(0);
        for (index, color) in palette.into_iter().take(256).enumerate() {
            let at = HEADER_BYTES + index * 4;
            output[at..at + 4].copy_from_slice(&[color[2], color[1], color[0], color[3]]);
        }
    }
    let width = usize::from(info.width);
    let stride = usize::from(info.stride);
    for row in 0..usize::from(info.height) {
        let src = row * width;
        let dst = PIXEL_OFFSET + row * stride;
        output[dst..dst + width].copy_from_slice(&indices[src..src + width]);
    }
    Ok(output)
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
) -> Result<(Vec<Rgba>, HashMap<Rgba, u8>), String> {
    let initial = colors
        .into_iter()
        .zip(counts)
        .map(|(color, count)| ColorCount { color, count })
        .collect::<Vec<_>>();
    let mut boxes = vec![initial];

    while boxes.len() < 256 {
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
    if palette.is_empty() || palette.len() > 256 {
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

    fn sample_template() -> Vec<u8> {
        let width = 2u16;
        let height = 2u16;
        let stride = 4u16;
        let mut data = vec![0u8; PIXEL_OFFSET + usize::from(stride * height)];
        data[0] = 0x19;
        data[1] = 0x0a;
        data[4..6].copy_from_slice(&width.to_le_bytes());
        data[6..8].copy_from_slice(&height.to_le_bytes());
        data[8..10].copy_from_slice(&stride.to_le_bytes());
        data[12..16].copy_from_slice(&[0, 0, 0, 255]);
        data[16..20].copy_from_slice(&[0, 0, 255, 255]);
        data[20..24].copy_from_slice(&[0, 255, 0, 255]);
        data[24..28].copy_from_slice(&[255, 0, 0, 255]);
        data[PIXEL_OFFSET..PIXEL_OFFSET + 2].copy_from_slice(&[0, 1]);
        data[PIXEL_OFFSET + 4..PIXEL_OFFSET + 6].copy_from_slice(&[2, 3]);
        data[PIXEL_OFFSET + 2..PIXEL_OFFSET + 4].copy_from_slice(&[77, 88]);
        data[PIXEL_OFFSET + 6..PIXEL_OFFSET + 8].copy_from_slice(&[99, 111]);
        data
    }

    #[test]
    fn header_inspection_validates_lazy_image_metadata() {
        let template = sample_template();
        let info = inspect_i8_header(&template[..HEADER_BYTES], template.len()).unwrap();
        assert_eq!(
            info,
            I8Info {
                width: 2,
                height: 2,
                stride: 4,
            }
        );
        assert!(inspect_i8_header(&template[..HEADER_BYTES], template.len() - 1).is_none());
    }

    #[test]
    fn i8_round_trip_preserves_template_bytes() {
        let template = sample_template();
        let (_, png) = decode_i8_png(&template).unwrap();
        let restored = encode_png_i8(&png, &template, false).unwrap();
        assert_eq!(restored, template);
    }

    #[test]
    fn png_encoding_preserves_stride_padding_and_changes_pixels() {
        let template = sample_template();
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
        let output = encode_png_i8(&cursor.into_inner(), &template, false).unwrap();
        assert_eq!(&output[PIXEL_OFFSET + 2..PIXEL_OFFSET + 4], &[77, 88]);
        assert_eq!(&output[PIXEL_OFFSET + 6..PIXEL_OFFSET + 8], &[99, 111]);
        assert_eq!(
            inspect_i8(&output).unwrap(),
            I8Info {
                width: 2,
                height: 2,
                stride: 4
            }
        );
    }

    #[test]
    fn lossy_conversion_requires_explicit_opt_in() {
        let mut template = vec![0u8; PIXEL_OFFSET + 257];
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
        assert!(encode_png_i8(cursor.get_ref(), &template, false).is_err());
        let quantized = encode_png_i8_detailed(&cursor.into_inner(), &template, true).unwrap();
        assert!(quantized.lossy_quantization);
        assert_eq!(inspect_i8(&quantized.bytes).unwrap().width, 257);
    }
}
