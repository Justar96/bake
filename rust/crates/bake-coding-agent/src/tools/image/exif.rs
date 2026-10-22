//! EXIF orientation, ported from Pi `src/utils/exif-orientation.ts`
//! (v1.1.0). Reads past the end count as zero, as JavaScript's `undefined`
//! does in Pi's bit arithmetic.

use image::DynamicImage;

fn at(bytes: &[u8], position: i64) -> u32 {
    usize::try_from(position)
        .ok()
        .and_then(|index| bytes.get(index))
        .copied()
        .map_or(0, u32::from)
}

fn read_orientation_from_tiff(bytes: &[u8], tiff_start: i64) -> u16 {
    let len = bytes.len() as i64;
    if tiff_start + 8 > len {
        return 1;
    }
    let little_endian = ((at(bytes, tiff_start) << 8) | at(bytes, tiff_start + 1)) == 0x4949;
    let read16 = |position: i64| -> u32 {
        if little_endian {
            at(bytes, position) | (at(bytes, position + 1) << 8)
        } else {
            (at(bytes, position) << 8) | at(bytes, position + 1)
        }
    };
    let read32 = |position: i64| -> i64 {
        if little_endian {
            // `<< 24` is a signed 32-bit shift in JavaScript.
            let value = at(bytes, position)
                | (at(bytes, position + 1) << 8)
                | (at(bytes, position + 2) << 16)
                | (at(bytes, position + 3) << 24);
            i64::from(value as i32)
        } else {
            i64::from(
                (at(bytes, position) << 24)
                    | (at(bytes, position + 1) << 16)
                    | (at(bytes, position + 2) << 8)
                    | at(bytes, position + 3),
            )
        }
    };
    let ifd_start = tiff_start + read32(tiff_start + 4);
    if ifd_start + 2 > len {
        return 1;
    }
    let entry_count = read16(ifd_start);
    for index in 0..i64::from(entry_count) {
        let entry = ifd_start + 2 + index * 12;
        if entry + 12 > len {
            return 1;
        }
        if read16(entry) == 0x0112 {
            let value = read16(entry + 8);
            return if (1..=8).contains(&value) {
                value as u16
            } else {
                1
            };
        }
    }
    1
}

fn has_exif_header(bytes: &[u8], offset: usize) -> bool {
    bytes.get(offset..offset + 6) == Some(b"Exif\0\0")
}

fn find_jpeg_tiff_offset(bytes: &[u8]) -> Option<usize> {
    let mut offset = 2usize;
    while offset + 1 < bytes.len() {
        if bytes[offset] != 0xff {
            return None;
        }
        let marker = bytes[offset + 1];
        if marker == 0xff {
            offset += 1;
            continue;
        }
        if marker == 0xe1 {
            if offset + 4 >= bytes.len() {
                return None;
            }
            let segment_start = offset + 4;
            if segment_start + 6 > bytes.len() {
                return None;
            }
            if has_exif_header(bytes, segment_start) {
                return Some(segment_start + 6);
            }
        }
        if offset + 4 > bytes.len() {
            return None;
        }
        let length = (usize::from(bytes[offset + 2]) << 8) | usize::from(bytes[offset + 3]);
        offset += 2 + length;
    }
    None
}

fn find_webp_tiff_offset(bytes: &[u8]) -> Option<usize> {
    let mut offset: i64 = 12;
    let len = bytes.len() as i64;
    while offset + 8 <= len {
        let chunk = |index: i64| at(bytes, offset + index);
        let id = [chunk(0), chunk(1), chunk(2), chunk(3)];
        let size =
            i64::from((chunk(4) | (chunk(5) << 8) | (chunk(6) << 16) | (chunk(7) << 24)) as i32);
        let data_start = offset + 8;
        if id
            == [
                u32::from(b'E'),
                u32::from(b'X'),
                u32::from(b'I'),
                u32::from(b'F'),
            ]
        {
            if data_start + size > len {
                return None;
            }
            let data = usize::try_from(data_start).ok()?;
            return Some(if size >= 6 && has_exif_header(bytes, data) {
                data + 6
            } else {
                data
            });
        }
        // RIFF chunks are padded to an even size. A negative size would loop
        // forever in Pi; it ends the search here.
        let next = data_start + size + size % 2;
        if next <= offset {
            return None;
        }
        offset = next;
    }
    None
}

/// The EXIF orientation of JPEG or WebP bytes, 1 when absent.
pub fn exif_orientation(bytes: &[u8]) -> u16 {
    let tiff = if bytes.starts_with(&[0xff, 0xd8]) {
        find_jpeg_tiff_offset(bytes)
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        find_webp_tiff_offset(bytes)
    } else {
        None
    };
    match tiff {
        Some(offset) => read_orientation_from_tiff(bytes, offset as i64),
        None => 1,
    }
}

/// Pi's `applyExifOrientation`: the pixels as the orientation displays them.
///
/// `DynamicImage`'s methods are compiled, optimized, in the `image` crate;
/// the generic `imageops` functions would be compiled here.
pub fn apply_exif_orientation(image: DynamicImage, original_bytes: &[u8]) -> DynamicImage {
    match exif_orientation(original_bytes) {
        2 => image.fliph(),
        3 => image.rotate180(),
        4 => image.flipv(),
        5 => image.rotate90().fliph(),
        6 => image.rotate90(),
        7 => image.rotate270().fliph(),
        8 => image.rotate270(),
        _ => image,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{RgbaImage, imageops};

    #[test]
    fn hostile_headers_do_not_hang_or_panic() {
        let mut webp = b"RIFF\0\0\0\0WEBP".to_vec();
        // A chunk whose size is -8 points back at itself.
        webp.extend_from_slice(b"VP8 \xf8\xff\xff\xff");
        assert_eq!(exif_orientation(&webp), 1);
        assert_eq!(exif_orientation(&[0xff, 0xd8, 0xff, 0xe1, 0xff]), 1);
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe1, 0x00, 0x10];
        jpeg.extend_from_slice(b"Exif\0\0II*\0\xff\xff\xff\x7f");
        assert_eq!(exif_orientation(&jpeg), 1);
    }

    #[test]
    fn rotations_match_pi() {
        // Orientation 6 rotates a 2x1 image clockwise into 1x2.
        let mut image = RgbaImage::new(2, 1);
        image.put_pixel(0, 0, image::Rgba([1, 0, 0, 255]));
        image.put_pixel(1, 0, image::Rgba([2, 0, 0, 255]));
        let rotated = imageops::rotate90(&image);
        assert_eq!(rotated.dimensions(), (1, 2));
        // Pi: dst index x * h + (h - 1 - y): (0,0) -> row 0, (1,0) -> row 1.
        assert_eq!(rotated.get_pixel(0, 0)[0], 1);
        assert_eq!(rotated.get_pixel(0, 1)[0], 2);
    }
}
