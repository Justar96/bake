//! Pasted images: reading a dropped file path out of a paste, and what a
//! staged image shows in the attachments panel.
//!
//! Ports `imagePath` from the TypeScript `paste.ts` and `formatAttachment`
//! from its `rows.ts`. Reading the image is the terminal owner's: this module
//! only reads its header bytes for the type and size.

/// Image types the preview stages, as the oracle's attachment admission
/// accepts them.
const EXTENSIONS: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("webp", "image/webp"),
    ("gif", "image/gif"),
];

/// The media type of an image path, by its extension.
pub fn media_type(path: &str) -> Option<&'static str> {
    let (_, extension) = path.rsplit_once('.')?;
    EXTENSIONS
        .iter()
        .find(|(known, _)| known.eq_ignore_ascii_case(extension))
        .map(|&(_, media)| media)
}

/// A paste read as one image file path, the way terminals deliver a dropped
/// file: quoted, with its spaces escaped by backslashes, or as a `file://`
/// URL. Only a paste that is exactly one such path is read; a path inside a
/// sentence stays text.
pub fn image_path(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.contains(['\n', '\r']) {
        return None;
    }
    let quoted = ["'", "\""].iter().find_map(|quote| {
        trimmed
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
    });
    let mut path = match quoted {
        Some(inner) => inner.to_owned(),
        // A Windows drive path keeps its separators; elsewhere a backslash
        // escapes the character after it.
        None if is_drive_path(trimmed) => trimmed.to_owned(),
        None => unescape(trimmed),
    };
    if let Some(url) = path.strip_prefix("file://") {
        // The host, usually empty, ends at the path's first slash.
        let at = url.find('/')?;
        path = percent_decode(&url[at..])?;
    }
    media_type(&path).map(|_| path)
}

fn is_drive_path(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() > 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\'
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.extend(chars.next()),
            c => out.push(c),
        }
    }
    out
}

/// Decodes `%XX` escapes; `None` for a malformed escape or invalid UTF-8.
fn percent_decode(text: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(text.len());
    let mut rest = text.as_bytes();
    while let Some((&byte, tail)) = rest.split_first() {
        if byte == b'%' {
            let hex = std::str::from_utf8(tail.get(..2)?).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            rest = &tail[2..];
        } else {
            bytes.push(byte);
            rest = tail;
        }
    }
    String::from_utf8(bytes).ok()
}

/// A staged image, as the attachments panel lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub name: String,
    pub media_type: &'static str,
    pub bytes: u64,
    /// Width and height in pixels, where the header says.
    pub size: Option<(u32, u32)>,
}

impl Image {
    /// The panel's line for the image: `clipboard.png · image/png · 2048 B ·
    /// 640×480`, as the oracle's `formatAttachment` reads.
    pub fn summary(&self) -> String {
        let mut parts = vec![
            self.name.clone(),
            self.media_type.to_owned(),
            format!("{} B", self.bytes),
        ];
        if let Some((width, height)) = self.size {
            parts.push(format!("{width}×{height}"));
        }
        parts.join(" · ")
    }
}

/// The media type an image's first bytes declare.
pub fn sniff(header: &[u8]) -> Option<&'static str> {
    if header.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if header.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if header.starts_with(b"GIF87a") || header.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if header.len() >= 12 && &header[..4] == b"RIFF" && &header[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// The pixel size a PNG or GIF header declares. JPEG and WebP keep theirs
/// further in, so they read as unknown.
pub fn pixel_size(header: &[u8]) -> Option<(u32, u32)> {
    match sniff(header)? {
        "image/png" if header.len() >= 24 => {
            let word = |at: usize| {
                u32::from_be_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]])
            };
            Some((word(16), word(20)))
        }
        "image/gif" if header.len() >= 10 => {
            let half = |at: usize| u32::from(u16::from_le_bytes([header[at], header[at + 1]]));
            Some((half(6), half(8)))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_dropped_image_path_is_read_however_the_terminal_quotes_it() {
        // The oracle's `imagePath` cases.
        let read = |text: &str| image_path(text);
        assert_eq!(read("/tmp/shot.png").as_deref(), Some("/tmp/shot.png"));
        assert_eq!(
            read("'/tmp/my shot.PNG' ").as_deref(),
            Some("/tmp/my shot.PNG")
        );
        assert_eq!(
            read("/tmp/my\\ shot.jpeg").as_deref(),
            Some("/tmp/my shot.jpeg")
        );
        assert_eq!(
            read("file:///tmp/my%20shot.webp").as_deref(),
            Some("/tmp/my shot.webp")
        );
        assert_eq!(
            read("C:\\Users\\me\\shot.gif").as_deref(),
            Some("C:\\Users\\me\\shot.gif")
        );
        assert_eq!(read("/tmp/notes.txt"), None);
        assert_eq!(read("/tmp/a.png\n/tmp/b.png"), None);
        assert_eq!(read(""), None);
        assert_eq!(read("file:///tmp/bad%zz.png"), None);
    }

    #[test]
    fn a_header_declares_its_type_and_a_png_or_gif_its_size() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend(640u32.to_be_bytes());
        png.extend(480u32.to_be_bytes());
        assert_eq!(sniff(&png), Some("image/png"));
        assert_eq!(pixel_size(&png), Some((640, 480)));
        let gif = b"GIF89a\x20\x00\x10\x00";
        assert_eq!(pixel_size(gif), Some((32, 16)));
        assert_eq!(sniff(b"\xff\xd8\xff\xe0"), Some("image/jpeg"));
        assert_eq!(pixel_size(b"\xff\xd8\xff\xe0"), None);
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff(b"plain text"), None);
    }

    #[test]
    fn an_image_reads_as_the_oracle_lists_an_attachment() {
        let image = Image {
            name: "clipboard.png".into(),
            media_type: "image/png",
            bytes: 2048,
            size: Some((640, 480)),
        };
        assert_eq!(
            image.summary(),
            "clipboard.png · image/png · 2048 B · 640×480"
        );
        let image = Image {
            size: None,
            ..image
        };
        assert_eq!(image.summary(), "clipboard.png · image/png · 2048 B");
    }
}
