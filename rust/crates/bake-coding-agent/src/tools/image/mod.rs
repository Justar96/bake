//! Image handling for tool results, ported from Pi `src/utils/mime.ts`,
//! `exif-orientation.ts`, `image-convert.ts`, `image-resize-core.ts`,
//! `image-resize.ts`, `image-process.ts`, and `tool-result-images.ts`
//! (v1.1.0; see the crate's `NOTICE`).
//!
//! Pi decodes, resizes, and encodes with Photon, a Rust library built on the
//! `image` crate that Pi runs as WebAssembly in a worker thread. Bake calls
//! `image` directly, with the same steps: decode by content, apply the EXIF
//! orientation, resize with Lanczos3, and encode RGBA PNG or RGB JPEG.
//! Encoded bytes therefore differ from Pi's; dimensions, MIME types, and the
//! notes the model sees do not. Decoding allocates at most
//! [`MAX_DECODE_ALLOC`] bytes; a larger image fails as one Photon cannot
//! read. All functions here are blocking.

pub mod exif;
pub mod mime;

use std::io::Cursor;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::imageops::FilterType;
use image::{ImageEncoder, RgbaImage};
use serde_json::Value;

use bake_ai::{ImageContent, TextContent, UserContentBlock};

/// The most memory one image decode may allocate.
pub const MAX_DECODE_ALLOC: u64 = 256 * 1024 * 1024;

/// Pi's `ImageResizeOptions`; `None` takes Pi's default.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ImageResizeOptions {
    /// Default 2000.
    pub max_width: Option<u32>,
    /// Default 2000.
    pub max_height: Option<u32>,
    /// Encoded base64 size limit; default 4.5 MiB.
    pub max_bytes: Option<f64>,
    /// Default 80.
    pub jpeg_quality: Option<u8>,
}

impl ImageResizeOptions {
    /// A model's `inputLimits.images.resize`, Pi's `ModelImageResizeOptions`.
    pub fn from_input_limits(limits: Option<&Value>) -> Option<Self> {
        let resize = limits?.get("images")?.get("resize")?.as_object()?;
        let dimension = |key: &str| {
            resize
                .get(key)
                .and_then(Value::as_f64)
                .filter(|value| *value >= 1.0)
                .map(|value| value.min(f64::from(u32::MAX)) as u32)
        };
        Some(Self {
            max_width: dimension("maxWidth"),
            max_height: dimension("maxHeight"),
            max_bytes: resize.get("maxBytes").and_then(Value::as_f64),
            jpeg_quality: resize
                .get("jpegQuality")
                .and_then(Value::as_f64)
                .map(|quality| quality.clamp(1.0, 100.0) as u8),
        })
    }
}

/// Pi's `ResizedImage`.
#[derive(Debug, Clone, PartialEq)]
pub struct ResizedImage {
    /// Base64 data.
    pub data: String,
    /// The MIME type.
    pub mime_type: String,
    /// Width after orientation, before resizing.
    pub original_width: u32,
    /// Height after orientation, before resizing.
    pub original_height: u32,
    /// Output width.
    pub width: u32,
    /// Output height.
    pub height: u32,
    /// Whether the image was re-encoded.
    pub was_resized: bool,
}

/// Decodes by content, as Photon's `new_from_byteslice`, then orients.
fn decode(bytes: &[u8]) -> Option<RgbaImage> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    let decoded = reader.decode().ok()?;
    Some(exif::apply_exif_orientation(decoded, bytes).into_rgba8())
}

fn encode_png(image: &RgbaImage) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgba8,
        )
        .ok()?;
    Some(out)
}

fn encode_jpeg(image: &RgbaImage, quality: u8) -> Option<Vec<u8>> {
    // JPEG has no alpha channel; it is dropped.
    let rgb = image::DynamicImage::ImageRgba8(image.clone()).to_rgb8();
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, quality)
        .write_image(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            image::ExtendedColorType::Rgb8,
        )
        .ok()?;
    Some(out)
}

/// Pi's `convertImageBytesToPng`: the oriented image as PNG.
pub fn convert_image_bytes_to_png(bytes: &[u8]) -> Option<Vec<u8>> {
    encode_png(&decode(bytes)?)
}

fn js_round(value: f64) -> u32 {
    (value + 0.5).floor().clamp(0.0, f64::from(u32::MAX)) as u32
}

/// Pi's `resizeImageInProcess`: the original bytes when the oriented image
/// fits the dimensions and its base64 the size limit; otherwise the first
/// PNG or JPEG (qualities `jpeg_quality`, 85, 70, 55, 40) under the limit,
/// shrinking by a quarter until 1x1; `None` when nothing fits or decoding
/// fails.
pub fn resize_image(
    input: &[u8],
    mime_type: &str,
    options: ImageResizeOptions,
) -> Option<ResizedImage> {
    let max_width = options.max_width.unwrap_or(2000);
    let max_height = options.max_height.unwrap_or(2000);
    let max_bytes = options.max_bytes.unwrap_or(4.5 * 1024.0 * 1024.0);
    let jpeg_quality = options.jpeg_quality.unwrap_or(80);
    let input_base64_size = input.len().div_ceil(3) as f64 * 4.0;

    let image = decode(input)?;
    let (original_width, original_height) = image.dimensions();
    let format = mime_type.split('/').nth(1).unwrap_or("png");
    if original_width <= max_width && original_height <= max_height && input_base64_size < max_bytes
    {
        return Some(ResizedImage {
            data: STANDARD.encode(input),
            mime_type: if mime_type.is_empty() {
                format!("image/{format}")
            } else {
                mime_type.to_owned()
            },
            original_width,
            original_height,
            width: original_width,
            height: original_height,
            was_resized: false,
        });
    }

    let mut width = original_width;
    let mut height = original_height;
    if width > max_width {
        height = js_round(f64::from(height) * f64::from(max_width) / f64::from(width));
        width = max_width;
    }
    if height > max_height {
        width = js_round(f64::from(width) * f64::from(max_height) / f64::from(height));
        height = max_height;
    }
    let mut qualities: Vec<u8> = Vec::new();
    for quality in [jpeg_quality, 85, 70, 55, 40] {
        if !qualities.contains(&quality) {
            qualities.push(quality);
        }
    }
    let image = image::DynamicImage::ImageRgba8(image);
    loop {
        // Photon cannot resize to zero; neither can `image`.
        // `DynamicImage::resize_exact` is compiled, optimized, in `image`.
        let resized = image
            .resize_exact(width.max(1), height.max(1), FilterType::Lanczos3)
            .into_rgba8();
        // Pi encodes every candidate, then takes the first under the limit;
        // encoding them in order and stopping there gives the same result.
        let candidates = std::iter::once((None, "image/png")).chain(
            qualities
                .iter()
                .map(|quality| (Some(*quality), "image/jpeg")),
        );
        for (quality, mime) in candidates {
            let encoded = match quality {
                None => encode_png(&resized),
                Some(quality) => encode_jpeg(&resized, quality),
            };
            let Some(bytes) = encoded else { continue };
            let data = STANDARD.encode(&bytes);
            if (data.len() as f64) < max_bytes {
                return Some(ResizedImage {
                    data,
                    mime_type: mime.to_owned(),
                    original_width,
                    original_height,
                    width,
                    height,
                    was_resized: true,
                });
            }
        }
        if width == 1 && height == 1 {
            return None;
        }
        let next_width = if width == 1 {
            1
        } else {
            ((f64::from(width) * 0.75).floor() as u32).max(1)
        };
        let next_height = if height == 1 {
            1
        } else {
            ((f64::from(height) * 0.75).floor() as u32).max(1)
        };
        if next_width == width && next_height == height {
            return None;
        }
        width = next_width;
        height = next_height;
    }
}

/// Pi's `formatDimensionNote`.
pub fn format_dimension_note(result: &ResizedImage) -> Option<String> {
    if !result.was_resized {
        return None;
    }
    let scale = f64::from(result.original_width) / f64::from(result.width);
    Some(format!(
        "[Image: original {}x{}, displayed at {}x{}. Multiply coordinates by {} to map to original image.]",
        result.original_width,
        result.original_height,
        result.width,
        result.height,
        crate::tools::js::to_fixed(scale, 2)
    ))
}

/// Pi's `ProcessImageResult`.
#[derive(Debug, Clone, PartialEq)]
pub enum ProcessedImage {
    /// The image to send and its notes.
    Ok {
        /// Base64 data.
        data: String,
        /// The MIME type.
        mime_type: String,
        /// Notes for the model.
        hints: Vec<String>,
    },
    /// Why the image is omitted.
    Failed(String),
}

fn base_mime_type(mime_type: &str) -> String {
    mime_type
        .split(';')
        .next()
        .unwrap_or(mime_type)
        .trim()
        .to_lowercase()
}

fn normalize_supported_image_mime_type(mime_type: &str) -> Option<&'static str> {
    match base_mime_type(mime_type).as_str() {
        "image/png" => Some("image/png"),
        "image/jpeg" | "image/jpg" => Some("image/jpeg"),
        "image/gif" => Some("image/gif"),
        "image/webp" => Some("image/webp"),
        _ => None,
    }
}

fn conversion_hint(from: Option<&str>, to: &str) -> Option<String> {
    from.filter(|from| *from != to)
        .map(|from| format!("[Image converted from {from} to {to}.]"))
}

/// Pi's `processImage`: converts formats a provider does not take to PNG,
/// then resizes to the inline limits unless `auto_resize_images` is off.
pub fn process_image(
    bytes: &[u8],
    mime_type: &str,
    auto_resize_images: bool,
    resize_options: ImageResizeOptions,
) -> ProcessedImage {
    let converted;
    let (bytes, mime, converted_from) = match normalize_supported_image_mime_type(mime_type) {
        Some(mime) => (bytes, mime, None),
        None => match convert_image_bytes_to_png(bytes) {
            Some(png) => {
                converted = png;
                (
                    converted.as_slice(),
                    "image/png",
                    Some(base_mime_type(mime_type)),
                )
            }
            None => {
                return ProcessedImage::Failed(
                    "[Image omitted: could not be converted to a supported inline image format.]"
                        .into(),
                );
            }
        },
    };
    let mut hints = Vec::new();
    if auto_resize_images {
        let Some(resized) = resize_image(bytes, mime, resize_options) else {
            return ProcessedImage::Failed(
                "[Image omitted: could not be resized below the inline image size limit.]".into(),
            );
        };
        hints.extend(conversion_hint(
            converted_from.as_deref(),
            &resized.mime_type,
        ));
        hints.extend(format_dimension_note(&resized));
        return ProcessedImage::Ok {
            data: resized.data,
            mime_type: resized.mime_type,
            hints,
        };
    }
    hints.extend(conversion_hint(converted_from.as_deref(), mime));
    ProcessedImage::Ok {
        data: STANDARD.encode(bytes),
        mime_type: mime.to_owned(),
        hints,
    }
}

/// Pi's `normalizeToolResultImages`: images a tool produced, processed as
/// [`process_image`] does, each followed by its notes. An image that cannot
/// be processed is kept as it was. `None` when nothing changed.
pub fn normalize_tool_result_images(
    content: &[UserContentBlock],
    auto_resize_images: bool,
    resize_options: ImageResizeOptions,
) -> Option<Vec<UserContentBlock>> {
    if !content
        .iter()
        .any(|block| matches!(block, UserContentBlock::Image(_)))
    {
        return None;
    }
    let mut normalized = Vec::with_capacity(content.len());
    let mut changed = false;
    for block in content {
        let UserContentBlock::Image(image) = block else {
            normalized.push(block.clone());
            continue;
        };
        // Node's base64 decoder skips characters outside the alphabet.
        let cleaned: String = image
            .data
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '-' | '_'))
            .collect();
        let bytes = base64::engine::GeneralPurpose::new(
            &base64::alphabet::STANDARD,
            base64::engine::GeneralPurposeConfig::new()
                .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent)
                .with_decode_allow_trailing_bits(true),
        )
        .decode(cleaned.replace('-', "+").replace('_', "/"))
        .unwrap_or_default();
        match process_image(&bytes, &image.mime_type, auto_resize_images, resize_options) {
            ProcessedImage::Failed(_) => normalized.push(block.clone()),
            ProcessedImage::Ok {
                data,
                mime_type,
                hints,
            } => {
                if data == image.data && mime_type == image.mime_type && hints.is_empty() {
                    normalized.push(block.clone());
                    continue;
                }
                normalized.push(UserContentBlock::Image(ImageContent { data, mime_type }));
                if !hints.is_empty() {
                    normalized.push(UserContentBlock::Text(TextContent::new(hints.join("\n"))));
                }
                changed = true;
            }
        }
    }
    changed.then_some(normalized)
}

#[cfg(test)]
mod tests {
    //! Pi `test/image-processing.test.ts` and `image-process.test.ts`,
    //! with images generated here instead of Pi's base64 constants.

    use super::*;

    fn png(width: u32, height: u32, noisy: bool) -> Vec<u8> {
        let mut seed = 0x9e37_79b9_u32;
        let image = RgbaImage::from_fn(width, height, |x, y| {
            if noisy {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                image::Rgba(seed.to_le_bytes())
            } else {
                image::Rgba([x as u8, y as u8, 0, 255])
            }
        });
        encode_png(&image).expect("png")
    }

    fn limits(max: u32, max_bytes: f64) -> ImageResizeOptions {
        ImageResizeOptions {
            max_width: Some(max),
            max_height: Some(max),
            max_bytes: Some(max_bytes),
            jpeg_quality: None,
        }
    }

    #[test]
    fn images_within_limits_are_returned_unchanged() {
        let bytes = png(2, 2, false);
        let result =
            resize_image(&bytes, "image/png", limits(100, 1024.0 * 1024.0)).expect("result");
        assert!(!result.was_resized);
        assert_eq!(result.data, STANDARD.encode(&bytes));
        assert_eq!(
            (result.original_width, result.width, result.height),
            (2, 2, 2)
        );
        assert_eq!(format_dimension_note(&result), None);
    }

    #[test]
    fn images_over_the_dimension_limits_are_resized() {
        let result = resize_image(
            &png(100, 100, false),
            "image/png",
            limits(50, 1024.0 * 1024.0),
        )
        .expect("result");
        assert!(result.was_resized);
        assert_eq!((result.original_width, result.original_height), (100, 100));
        assert!(result.width <= 50 && result.height <= 50);
    }

    #[test]
    fn images_over_the_byte_limit_shrink_or_fail() {
        let bytes = png(200, 200, true);
        let encoded = STANDARD.encode(&bytes).len() as f64;
        let result = resize_image(&bytes, "image/png", limits(2000, (encoded * 0.9).floor()))
            .expect("result");
        assert!((result.data.len() as f64) < encoded);
        assert!(resize_image(&bytes, "image/png", limits(2000, 1.0)).is_none());
    }

    #[test]
    fn dimension_notes_follow_pi() {
        let note = format_dimension_note(&ResizedImage {
            data: String::new(),
            mime_type: "image/png".into(),
            original_width: 2000,
            original_height: 1000,
            width: 1000,
            height: 500,
            was_resized: true,
        });
        assert_eq!(
            note.as_deref(),
            Some(
                "[Image: original 2000x1000, displayed at 1000x500. Multiply coordinates by 2.00 to map to original image.]"
            )
        );
    }

    #[test]
    fn undecodable_and_converted_images() {
        assert!(
            resize_image(b"not an image", "image/png", ImageResizeOptions::default()).is_none()
        );
        let jpeg = encode_jpeg(&RgbaImage::new(2, 2), 80).expect("jpeg");
        let png = convert_image_bytes_to_png(&jpeg).expect("png");
        assert!(png.starts_with(b"\x89PNG"));
        assert_eq!(
            mime::detect_supported_image_mime_type(&jpeg),
            Some("image/jpeg")
        );
        // Pi `image-process.test.ts`: a 1x1 BMP is detected and converted.
        let bmp = STANDARD
            .decode(
                "Qk06AAAAAAAAADYAAAAoAAAAAQAAAAEAAAABABgAAAAAAAQAAAAAAAAAAAAAAAAAAAAAAAAAAAD/AA==",
            )
            .expect("bmp");
        assert_eq!(
            mime::detect_supported_image_mime_type(&bmp),
            Some("image/bmp")
        );
        assert!(convert_image_bytes_to_png(&bmp).is_some_and(|png| png.starts_with(b"\x89PNG")));
    }
}
