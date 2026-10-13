//! `@file` command-line arguments: text files and image attachments.
//!
//! Ported from Pi `packages/coding-agent/src/cli/file-processor.ts` (v1.2.0,
//! `06a466935536f8c9bcacb2d5efc0fc021be1bdff`, unchanged since v1.1.0). Each
//! path is resolved against the working directory with [`resolve_read_path`],
//! empty files are skipped, supported images become [`ImageContent`]
//! attachments with `<file>` references (or an inline omission note when the
//! image cannot be processed), and other files are read as UTF-8 text inside
//! `<file>` tags with a leading UTF-8 BOM stripped.

use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

use bake_ai::ImageContent;

use crate::auth_storage::{read_text_file, strip_bom};
use crate::cli::MAX_STDIN_BYTES;
use crate::tools::image::{
    ImageResizeOptions, MAX_DECODE_ALLOC, ProcessedImage, mime, process_image,
};
use crate::tools::path_utils::resolve_read_path;

/// Pi's `ProcessedFiles`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProcessedFiles {
    /// `<file name="...">...</file>` blocks in argument order.
    pub text: String,
    /// Image attachments in argument order.
    pub images: Vec<ImageContent>,
}

/// Pi's `ProcessFileOptions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessFileOptions {
    /// Whether to auto-resize images; `true` by default.
    pub auto_resize_images: bool,
}

impl Default for ProcessFileOptions {
    fn default() -> Self {
        Self {
            auto_resize_images: true,
        }
    }
}

fn read_bounded_bytes(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > limit {
        return Err(format!("file is larger than {limit} bytes"));
    }
    Ok(bytes)
}

/// Pi's `processFileArguments`: turns `@file` arguments into `<file>` text
/// and image attachments.
pub fn process_file_arguments(
    file_args: &[String],
    cwd: &Path,
    options: ProcessFileOptions,
) -> Result<ProcessedFiles, String> {
    let mut text = String::new();
    let mut images = Vec::new();

    for file_arg in file_args {
        let absolute_path = resolve_read_path(file_arg, cwd)?;
        let metadata = fs::metadata(&absolute_path)
            .map_err(|_| format!("File not found: {}", absolute_path.display()))?;
        if metadata.len() == 0 {
            continue;
        }
        let display = absolute_path.display();
        let mime_type = mime::detect_supported_image_mime_type_from_file(&absolute_path)
            .ok()
            .flatten();
        if let Some(mime_type) = mime_type {
            let content = read_bounded_bytes(&absolute_path, MAX_DECODE_ALLOC)
                .map_err(|error| format!("Could not read file {display}: {error}"))?;
            match process_image(
                &content,
                mime_type,
                options.auto_resize_images,
                ImageResizeOptions::default(),
            ) {
                ProcessedImage::Failed(message) => {
                    text.push_str(&format!("<file name=\"{display}\">{message}</file>\n"));
                }
                ProcessedImage::Ok {
                    data,
                    mime_type,
                    hints,
                } => {
                    images.push(ImageContent { data, mime_type });
                    if hints.is_empty() {
                        text.push_str(&format!("<file name=\"{display}\"></file>\n"));
                    } else {
                        text.push_str(&format!(
                            "<file name=\"{display}\">{}</file>\n",
                            hints.join("\n")
                        ));
                    }
                }
            }
        } else {
            let raw = read_text_file(&absolute_path, MAX_STDIN_BYTES)
                .map_err(|error| format!("Could not read file {display}: {error}"))?
                .unwrap_or_default();
            let content = strip_bom(&raw);
            text.push_str(&format!("<file name=\"{display}\">\n{content}\n</file>\n"));
        }
    }

    Ok(ProcessedFiles { text, images })
}

#[cfg(test)]
mod tests {
    //! Cases from Pi `test/image-resize-callers.test.ts` and
    //! `src/cli/file-processor.ts`.

    use super::*;
    use crate::test_support::{TempDir, write};

    const TINY_PNG: &[u8] = &[
        0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, b'I', b'H', b'D',
        b'R', 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, b'I', b'D', b'A', b'T', 0x78, 0x9c, 0x62, 0xf8,
        0xcf, 0xc0, 0xf0, 0x1f, 0x00, 0x05, 0x05, 0x02, 0x00, 0x5f, 0xc8, 0xf1, 0xd2, 0x00, 0x00,
        0x00, 0x00, b'I', b'E', b'N', b'D', 0xae, 0x42, 0x60, 0x82,
    ];

    #[test]
    fn reads_text_files_skips_empty_files_and_strips_bom() {
        let dir = TempDir::new("file-processor");
        let a = dir.join("a.txt");
        let empty = dir.join("empty.txt");
        write(&a, "\u{feff}hello\nworld");
        write(&empty, "");

        let out = process_file_arguments(
            &["a.txt".into(), "empty.txt".into()],
            dir.path(),
            ProcessFileOptions::default(),
        )
        .expect("processed");
        assert!(out.images.is_empty());
        assert_eq!(
            out.text,
            format!("<file name=\"{}\">\nhello\nworld\n</file>\n", a.display())
        );
    }

    #[test]
    fn processes_images_and_omits_corrupt_ones() {
        let dir = TempDir::new("file-processor-images");
        let valid = dir.join("one.png");
        let corrupt = dir.join("corrupt.png");
        std::fs::write(&valid, TINY_PNG).expect("write png");
        // PNG header + IHDR followed by garbage so MIME magic matches PNG but decode fails.
        let mut corrupt_bytes = TINY_PNG[..16].to_vec();
        corrupt_bytes.extend_from_slice(b"not-a-png-body");
        std::fs::write(&corrupt, corrupt_bytes).expect("write corrupt png");

        let deferred = process_file_arguments(
            &["one.png".into()],
            dir.path(),
            ProcessFileOptions {
                auto_resize_images: false,
            },
        )
        .expect("deferred");
        assert_eq!(deferred.images.len(), 1);
        assert_eq!(deferred.images[0].mime_type, "image/png");
        assert_eq!(
            deferred.text,
            format!("<file name=\"{}\"></file>\n", valid.display())
        );

        let omitted = process_file_arguments(
            &["corrupt.png".into()],
            dir.path(),
            ProcessFileOptions::default(),
        )
        .expect("omitted");
        assert!(omitted.images.is_empty());
        assert!(omitted.text.contains("Image omitted"), "{}", omitted.text);
    }

    #[test]
    fn missing_file_reports_pi_error() {
        let dir = TempDir::new("file-processor-missing");
        let err = process_file_arguments(
            &["missing.txt".into()],
            dir.path(),
            ProcessFileOptions::default(),
        )
        .expect_err("missing");
        assert_eq!(
            err,
            format!("File not found: {}", dir.join("missing.txt").display())
        );
    }
}
