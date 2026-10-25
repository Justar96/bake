//! Image type sniffing, ported from Pi `src/utils/mime.ts` (v1.1.0).

use std::io::Read;
use std::path::Path;

/// How many leading bytes [`detect_supported_image_mime_type_from_file`]
/// reads.
pub const IMAGE_TYPE_SNIFF_BYTES: usize = 4100;

const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];

fn byte(buffer: &[u8], offset: usize) -> u32 {
    buffer.get(offset).copied().map_or(0, u32::from)
}

fn read_u16_le(buffer: &[u8], offset: usize) -> u32 {
    byte(buffer, offset) | (byte(buffer, offset + 1) << 8)
}

fn read_u32_be(buffer: &[u8], offset: usize) -> u32 {
    (byte(buffer, offset) << 24)
        | (byte(buffer, offset + 1) << 16)
        | (byte(buffer, offset + 2) << 8)
        | byte(buffer, offset + 3)
}

fn read_u32_le(buffer: &[u8], offset: usize) -> u32 {
    byte(buffer, offset)
        | (byte(buffer, offset + 1) << 8)
        | (byte(buffer, offset + 2) << 16)
        | (byte(buffer, offset + 3) << 24)
}

fn starts_with_at(buffer: &[u8], offset: usize, text: &[u8]) -> bool {
    buffer.get(offset..offset.saturating_add(text.len())) == Some(text)
}

fn is_png(buffer: &[u8]) -> bool {
    buffer.len() >= 16
        && read_u32_be(buffer, PNG_SIGNATURE.len()) == 13
        && starts_with_at(buffer, 12, b"IHDR")
}

fn is_animated_png(buffer: &[u8]) -> bool {
    let mut offset = PNG_SIGNATURE.len();
    while offset + 8 <= buffer.len() {
        let chunk_length = read_u32_be(buffer, offset) as usize;
        if starts_with_at(buffer, offset + 4, b"acTL") {
            return true;
        }
        if starts_with_at(buffer, offset + 4, b"IDAT") {
            return false;
        }
        let next = offset
            .saturating_add(8)
            .saturating_add(chunk_length)
            .saturating_add(4);
        if next <= offset || next > buffer.len() {
            return false;
        }
        offset = next;
    }
    false
}

fn is_bmp(buffer: &[u8]) -> bool {
    if buffer.len() < 26 {
        return false;
    }
    let declared_file_size = read_u32_le(buffer, 2);
    let pixel_data_offset = read_u32_le(buffer, 10);
    let dib_header_size = read_u32_le(buffer, 14);
    if declared_file_size != 0 && declared_file_size < 26 {
        return false;
    }
    if u64::from(pixel_data_offset) < 14 + u64::from(dib_header_size) {
        return false;
    }
    if declared_file_size != 0 && pixel_data_offset >= declared_file_size {
        return false;
    }
    let (planes, bits) = if dib_header_size == 12 {
        (read_u16_le(buffer, 22), read_u16_le(buffer, 24))
    } else if (40..=124).contains(&dib_header_size) {
        if buffer.len() < 30 {
            return false;
        }
        (read_u16_le(buffer, 26), read_u16_le(buffer, 28))
    } else {
        return false;
    };
    planes == 1 && [1, 4, 8, 16, 24, 32].contains(&bits)
}

/// Pi's `detectSupportedImageMimeType`: JPEG (not JPEG XL), static PNG,
/// GIF, WebP, or BMP from the leading bytes.
pub fn detect_supported_image_mime_type(buffer: &[u8]) -> Option<&'static str> {
    if buffer.starts_with(&[0xff, 0xd8, 0xff]) {
        return (buffer.get(3) != Some(&0xf7)).then_some("image/jpeg");
    }
    if buffer.starts_with(&PNG_SIGNATURE) {
        return (is_png(buffer) && !is_animated_png(buffer)).then_some("image/png");
    }
    if buffer.starts_with(b"GIF87a") || buffer.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if buffer.starts_with(b"RIFF") && starts_with_at(buffer, 8, b"WEBP") {
        return Some("image/webp");
    }
    if buffer.starts_with(b"BM") && is_bmp(buffer) {
        return Some("image/bmp");
    }
    None
}

/// Pi's `detectSupportedImageMimeTypeFromFile`: sniffs the first
/// [`IMAGE_TYPE_SNIFF_BYTES`] bytes.
pub fn detect_supported_image_mime_type_from_file(
    path: &Path,
) -> std::io::Result<Option<&'static str>> {
    let mut file = std::fs::File::open(path)?;
    let mut buffer = Vec::with_capacity(IMAGE_TYPE_SNIFF_BYTES);
    (&mut file)
        .take(IMAGE_TYPE_SNIFF_BYTES as u64)
        .read_to_end(&mut buffer)?;
    Ok(detect_supported_image_mime_type(&buffer))
}
