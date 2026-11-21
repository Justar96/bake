//! Pasted images: reading a dropped file path out of a paste, and what a
//! staged image shows in the attachments panel.
//!
//! Reading the image is the terminal owner's: this module only reads its
//! header bytes for the type and size.

/// Image types the preview stages.
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

/// A paste read as one local image file path, the way terminals deliver a
/// dropped file: quoted, with its spaces escaped by backslashes, or as a
/// local `file://` URL. Syntax alone cannot prove a path is not a symlink or
/// a mounted remote share; this rejects line breaks, control characters,
/// unmatched quotes, bare filenames, sentences, non-file URLs, remote
/// `file://` hosts, and UNC-like double separators.
pub fn image_path(text: &str) -> Option<String> {
    if text.contains(['\n', '\r']) {
        return None;
    }
    let trimmed = text.trim_matches([' ', '\t']);
    if trimmed.is_empty() || trimmed.chars().any(char::is_control) {
        return None;
    }
    let quoted = ["'", "\""].iter().find_map(|&quote| {
        trimmed
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
            .filter(|inner| !inner.contains(quote))
    });
    let mut path = match quoted {
        Some(inner) => inner.to_owned(),
        // Reject unmatched quotes rather than treating them as a path.
        None if trimmed.starts_with(['\'', '"']) || trimmed.ends_with(['\'', '"']) => return None,
        // Reject UNC-like prefixes before unescaping so `\\server\...` is never
        // collapsed into a single-backslash string.
        None if is_unc_like(trimmed) => return None,
        // A Windows drive path keeps its separators and may hold spaces;
        // elsewhere unescaped whitespace means a sentence or multiple tokens,
        // and a backslash escapes the character after it.
        None if is_drive_path(trimmed) => trimmed.to_owned(),
        None if has_unescaped_whitespace(trimmed) => return None,
        None => unescape(trimmed),
    };
    if let Some(url) = path.strip_prefix("file://") {
        // Only local `file://` URLs (empty host or `localhost`): a named host
        // is a remote/UNC path and must not trigger network access.
        let at = url.find('/')?;
        let host = &url[..at];
        if !host.is_empty() && !host.eq_ignore_ascii_case("localhost") {
            return None;
        }
        path = percent_decode(&url[at..])?;
    } else if has_uri_scheme(&path) {
        return None;
    }
    if path.chars().any(char::is_control)
        || is_unc_like(&path)
        || has_uri_scheme(&path)
        || !path.contains(['/', '\\'])
        || !has_filename_stem(&path)
    {
        return None;
    }
    media_type(&path).map(|_| path)
}

fn is_drive_path(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() > 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
}

/// Rejects `//`, `\\`, `/\`, and `\/` prefixes (UNC / network-ambiguous on
/// Windows), including after `file://` percent-decoding.
fn is_unc_like(text: &str) -> bool {
    let mut chars = text.chars();
    matches!(chars.next(), Some('/' | '\\')) && matches!(chars.next(), Some('/' | '\\'))
}

fn has_unescaped_whitespace(text: &str) -> bool {
    let mut escaped = false;
    for c in text.chars() {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c.is_whitespace() {
            return true;
        }
    }
    false
}

fn has_uri_scheme(text: &str) -> bool {
    let Some((scheme, _)) = text.split_once("://") else {
        return false;
    };
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

fn has_filename_stem(path: &str) -> bool {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    name.rsplit_once('.')
        .is_some_and(|(stem, _)| !stem.is_empty())
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

/// A staged image, as the attachments panel lists it, with the bytes a
/// live session sends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub name: String,
    pub media_type: &'static str,
    pub bytes: u64,
    /// Width and height in pixels, where the header says.
    pub size: Option<(u32, u32)>,
    /// The image file's bytes.
    pub data: ImageData,
}

/// An image's bytes, shared and cheap to clone. Debug output shows only
/// their length.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ImageData(pub std::sync::Arc<[u8]>);

impl ImageData {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes.into())
    }

    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
}

impl std::fmt::Debug for ImageData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ImageData({} bytes)", self.0.len())
    }
}

impl Image {
    /// The panel's line for the image: `clipboard.png · image/png · 2048 B ·
    /// 640×480`.
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
            read("file://localhost/tmp/shot.png").as_deref(),
            Some("/tmp/shot.png")
        );
        assert_eq!(
            read("C:\\Users\\me\\shot.gif").as_deref(),
            Some("C:\\Users\\me\\shot.gif")
        );
        assert_eq!(
            read("C:\\Users\\my name\\shot.gif").as_deref(),
            Some("C:\\Users\\my name\\shot.gif")
        );
        assert_eq!(
            read("\"C:\\Users\\my name\\shot.gif\"").as_deref(),
            Some("C:\\Users\\my name\\shot.gif")
        );
        assert_eq!(read("assets/shot.png").as_deref(), Some("assets/shot.png"));
        assert_eq!(read("./shot.png").as_deref(), Some("./shot.png"));
        assert_eq!(read("/tmp/notes.txt"), None);
        assert_eq!(read("/tmp/a.png\n/tmp/b.png"), None);
        assert_eq!(read(""), None);
        assert_eq!(read("file:///tmp/bad%zz.png"), None);
    }

    #[test]
    fn ordinary_text_urls_unc_and_line_breaks_are_not_image_paths() {
        for text in [
            "shot.png",
            "'shot.png'",
            "see /tmp/shot.png",
            "check assets/shot.png",
            "https://example.com/avatar.png",
            "'https://example.com/avatar.png'",
            "http://localhost:8080/a.jpg",
            "file://fileserver/share/shot.png",
            "file:////server/share/shot.png",
            "file://localhost//server/share/shot.png",
            "file:///%5Cserver/share/shot.png",
            "file:///%2Fserver/share/shot.png",
            "file:///tmp/a%00b.png",
            "file:///tmp/a%0Ab.png",
            "file:///tmp/a%0Db.png",
            "\\\\server\\share\\shot.png",
            "//server/share/shot.png",
            "/\\server\\share\\shot.png",
            "\\/server/share/shot.png",
            "'/tmp/shot.png",
            "/tmp/shot.png'",
            "\"/tmp/shot.png",
            "/tmp/shot.png\"",
            "/tmp/shot\x1b.png",
            "/tmp/shot.png\n",
            "/tmp/shot.png\r\n",
            "\n/tmp/shot.png",
            "'/tmp/a.png' '/tmp/b.png'",
            "/tmp/.png",
        ] {
            assert_eq!(image_path(text), None, "must stay text: {text:?}");
        }
    }

    mod properties {
        use proptest::prelude::*;

        use super::*;

        fn assert_valid_decoded_path(input: &str, path: &str) {
            assert!(!input.contains(['\n', '\r']), "input had CR/LF: {input:?}");
            assert!(
                !path.chars().any(char::is_control),
                "decoded path had control char: {path:?} from {input:?}"
            );
            assert!(
                !is_unc_like(path),
                "decoded path was UNC-like: {path:?} from {input:?}"
            );
            assert!(
                !has_uri_scheme(path),
                "decoded path had URI scheme: {path:?} from {input:?}"
            );
            assert!(
                path.contains(['/', '\\']),
                "decoded path lacked separator: {path:?} from {input:?}"
            );
            assert!(
                has_filename_stem(path),
                "decoded path lacked stem: {path:?} from {input:?}"
            );
            assert!(
                media_type(path).is_some(),
                "decoded path lacked image extension: {path:?} from {input:?}"
            );
        }

        proptest! {
            #[test]
            fn arbitrary_text_image_path_invariants(input in any::<String>()) {
                if let Some(path) = image_path(&input) {
                    assert_valid_decoded_path(&input, &path);
                }
            }

            #[test]
            fn file_uri_inputs_never_produce_unc_or_control_paths(
                host in prop_oneof![Just(String::new()), Just("localhost".to_owned()), "[a-z0-9.-]{1,12}"],
                prefix in prop_oneof![
                    Just(String::new()),
                    Just("/".to_owned()),
                    Just("%2F".to_owned()),
                    Just("%5C".to_owned()),
                    Just("\\".to_owned()),
                ],
                body in prop::collection::vec(
                    prop_oneof![
                        Just("a".to_owned()),
                        Just("/".to_owned()),
                        Just("%20".to_owned()),
                        Just("%00".to_owned()),
                        Just("%0A".to_owned()),
                        Just("%0D".to_owned()),
                        Just("%1B".to_owned()),
                        Just("%5C".to_owned()),
                        Just("%2F".to_owned()),
                        Just(".".to_owned()),
                    ],
                    0..16,
                ),
                ext in prop_oneof![Just(".png"), Just(".jpg"), Just(".webp"), Just(".txt")],
            ) {
                let uri = format!("file://{host}/{prefix}{}{ext}", body.concat());
                if let Some(path) = image_path(&uri) {
                    assert!(
                        host.is_empty() || host.eq_ignore_ascii_case("localhost"),
                        "accepted remote host {host:?} from {uri:?}"
                    );
                    assert_valid_decoded_path(&uri, &path);
                }
            }
        }
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
    fn an_image_reads_as_an_attachment_line() {
        let image = Image {
            name: "clipboard.png".into(),
            media_type: "image/png",
            bytes: 2048,
            size: Some((640, 480)),
            data: ImageData::default(),
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
