//! Path input for the file tools, ported from Pi
//! `src/core/tools/path-utils.ts` and the parts of `src/utils/paths.ts` it
//! uses (v1.1.0; see the crate's `NOTICE`).
//!
//! A tool path has its Unicode spaces turned into spaces and a leading `@`
//! removed, then `~` expanded and, on Windows, Git Bash and WSL drive paths
//! converted ([`normalize_path`]); `file://` URLs become paths. The result is
//! made absolute against the tool's working directory and normalized
//! lexically, as Node's `path.resolve` does. The read tool also tries the
//! names macOS gives screenshots ([`resolve_read_path`]).

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use crate::session::paths::{lexical_normalize, normalize_path};

const NARROW_NO_BREAK_SPACE: char = '\u{202F}';

fn is_unicode_space(c: char) -> bool {
    matches!(
        c,
        '\u{00A0}' | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}'
    )
}

/// Pi's `expandPath`: Unicode spaces normalized, a leading `@` removed, `~`
/// and Windows shell paths expanded, and a `file://` URL converted.
pub fn expand_path(file_path: &str) -> Result<PathBuf, String> {
    let spaced: String = file_path
        .chars()
        .map(|c| if is_unicode_space(c) { ' ' } else { c })
        .collect();
    let stripped = spaced.strip_prefix('@').unwrap_or(&spaced);
    let normalized = normalize_path(stripped);
    match normalized.to_str() {
        Some(text) if text.starts_with("file://") => file_url_to_path(text),
        _ => Ok(normalized),
    }
}

/// Pi's `resolveToCwd`: [`expand_path`], then absolute against `cwd`.
pub fn resolve_to_cwd(file_path: &str, cwd: &Path) -> Result<PathBuf, String> {
    let expanded = expand_path(file_path)?;
    let joined = if expanded.is_absolute() {
        expanded
    } else {
        normalize_base(cwd).join(expanded)
    };
    Ok(lexical_normalize(&joined))
}

fn normalize_base(cwd: &Path) -> PathBuf {
    let base = match cwd.to_str() {
        Some(text) => normalize_path(text),
        None => cwd.to_path_buf(),
    };
    if base.is_absolute() {
        base
    } else {
        std::path::absolute(&base).unwrap_or(base)
    }
}

/// Node's `fileURLToPath` for the URLs a tool path can hold.
fn file_url_to_path(url: &str) -> Result<PathBuf, String> {
    let rest = &url["file://".len()..];
    let (host, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, ""),
    };
    let path = path.split(['?', '#']).next().unwrap_or_default();
    if path.contains("%2F")
        || path.contains("%2f")
        || (cfg!(windows) && (path.contains("%5C") || path.contains("%5c")))
    {
        return Err(if cfg!(windows) {
            "File URL path must not include encoded \\ or / characters".into()
        } else {
            "File URL path must not include encoded / characters".into()
        });
    }
    let decoded = percent_decode(path);
    if cfg!(windows) {
        let windows = decoded.replace('/', "\\");
        if !host.is_empty() && host != "localhost" {
            return Ok(PathBuf::from(format!("\\\\{host}{windows}")));
        }
        let trimmed = windows.strip_prefix('\\').unwrap_or(&windows);
        let bytes = trimmed.as_bytes();
        if bytes.len() < 2 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
            return Err("File URL path must be absolute".into());
        }
        return Ok(PathBuf::from(trimmed));
    }
    if !host.is_empty() && host != "localhost" {
        return Err(format!(
            "File URL host must be \"localhost\" or empty on {}",
            std::env::consts::OS
        ));
    }
    Ok(PathBuf::from(decoded))
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let hex = |byte: u8| (byte as char).to_digit(16);
        if bytes[index] == b'%'
            && let (Some(high), Some(low)) = (
                bytes.get(index + 1).copied().and_then(hex),
                bytes.get(index + 2).copied().and_then(hex),
            )
        {
            out.push((high * 16 + low) as u8);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Node's `fs.access(path, F_OK)`: the path exists, following links.
pub fn path_exists(path: &Path) -> bool {
    std::fs::metadata(path).is_ok()
}

fn macos_screenshot_variant(path: &str) -> String {
    static AM_PM: LazyLock<Option<regex::Regex>> =
        LazyLock::new(|| regex::Regex::new(r"(?i) (AM|PM)\.").ok());
    match AM_PM.as_ref() {
        Some(pattern) => pattern
            .replace_all(path, format!("{NARROW_NO_BREAK_SPACE}${{1}}."))
            .into_owned(),
        None => path.to_owned(),
    }
}

fn nfd_variant(path: &str) -> String {
    icu_normalizer::DecomposingNormalizerBorrowed::new_nfd()
        .normalize(path)
        .into_owned()
}

fn curly_quote_variant(path: &str) -> String {
    path.replace('\'', "\u{2019}")
}

/// Pi's `resolveReadPath`: [`resolve_to_cwd`], or when that does not exist,
/// the first variant that does: a narrow no-break space before `AM.` or
/// `PM.`, the NFD form, curly apostrophes, then NFD with curly apostrophes.
pub fn resolve_read_path(file_path: &str, cwd: &Path) -> Result<PathBuf, String> {
    let resolved = resolve_to_cwd(file_path, cwd)?;
    if path_exists(&resolved) {
        return Ok(resolved);
    }
    let Some(text) = resolved.to_str() else {
        return Ok(resolved);
    };
    let nfd = nfd_variant(text);
    let candidates = [
        macos_screenshot_variant(text),
        nfd.clone(),
        curly_quote_variant(text),
        curly_quote_variant(&nfd),
    ];
    for candidate in candidates {
        if candidate != text && path_exists(Path::new(&candidate)) {
            return Ok(PathBuf::from(candidate));
        }
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    //! Pi `test/path-utils.test.ts`.

    use super::*;

    struct Dir(PathBuf);

    impl Dir {
        fn new(name: &str) -> Self {
            let mut random = [0u8; 8];
            let _ = getrandom::fill(&mut random);
            let id: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
            let path = std::env::temp_dir().join(format!("bake-path-utils-{name}-{id}"));
            std::fs::create_dir_all(&path).expect("temp dir");
            Self(path)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn expands_home_and_keeps_tilde_names_literal() {
        if let Some(home) = std::env::home_dir() {
            assert_eq!(expand_path("~"), Ok(home.clone()));
            assert_eq!(
                expand_path("~/Documents/file.txt"),
                Ok(home.join("Documents/file.txt"))
            );
        }
        assert_eq!(expand_path("~draft.md"), Ok(PathBuf::from("~draft.md")));
        assert_eq!(expand_path("@~draft.md"), Ok(PathBuf::from("~draft.md")));
        assert_eq!(
            expand_path("file\u{00A0}name.txt"),
            Ok(PathBuf::from("file name.txt"))
        );
    }

    #[test]
    fn resolves_against_cwd() {
        let cwd = std::env::temp_dir().join("some").join("cwd");
        let absolute = std::env::temp_dir()
            .join("absolute")
            .join("path")
            .join("file.txt");
        assert_eq!(
            resolve_to_cwd(&absolute.to_string_lossy(), &cwd),
            Ok(absolute.clone())
        );
        assert_eq!(
            resolve_to_cwd("relative/file.txt", &cwd),
            Ok(cwd.join("relative").join("file.txt"))
        );
        assert_eq!(resolve_to_cwd("~draft.md", &cwd), Ok(cwd.join("~draft.md")));
        assert_eq!(
            resolve_to_cwd("@~draft.md", &cwd),
            Ok(cwd.join("~draft.md"))
        );
        assert_eq!(
            resolve_to_cwd("a/../../b", &cwd),
            Ok(cwd.parent().map(|p| p.join("b")).unwrap_or_default())
        );
    }

    #[cfg(unix)]
    #[test]
    fn converts_file_urls() {
        assert_eq!(
            expand_path("file:///tmp/a%20b"),
            Ok(PathBuf::from("/tmp/a b"))
        );
        assert_eq!(expand_path("file://localhost/x"), Ok(PathBuf::from("/x")));
        assert!(expand_path("file://host/x").is_err());
        assert!(expand_path("file:///a%2Fb").is_err());
    }

    #[test]
    fn read_path_variants() {
        let dir = Dir::new("variants");
        std::fs::write(dir.0.join("test-file.txt"), "content").expect("write");
        assert_eq!(
            resolve_read_path("test-file.txt", &dir.0),
            Ok(dir.0.join("test-file.txt"))
        );

        let curly = "Capture d\u{2019}cran.txt";
        std::fs::write(dir.0.join(curly), "content").expect("write");
        assert_eq!(
            resolve_read_path("Capture d'cran.txt", &dir.0),
            Ok(dir.0.join(curly))
        );

        let nfc_curly = "Capture d\u{2019}\u{00e9}cran.txt";
        std::fs::write(dir.0.join(nfc_curly), "content").expect("write");
        assert_eq!(
            resolve_read_path("Capture d'\u{00e9}cran.txt", &dir.0),
            Ok(dir.0.join(nfc_curly))
        );

        for (disk, typed) in [
            (
                "Screenshot 10.00.00\u{202F}AM.png",
                "Screenshot 10.00.00 AM.png",
            ),
            (
                "Screenshot 10.00.00\u{202F}am.png",
                "Screenshot 10.00.00 am.png",
            ),
        ] {
            std::fs::write(dir.0.join(disk), "content").expect("write");
            let found = resolve_read_path(typed, &dir.0).expect("resolves");
            // Case-insensitive file systems may match either name.
            assert!(path_exists(&found));
            assert_eq!(found.parent(), Some(dir.0.as_path()));
        }

        let nfd = "file\u{0065}\u{0301}.txt";
        std::fs::write(dir.0.join(nfd), "content").expect("write");
        let found = resolve_read_path("file\u{00e9}.txt", &dir.0).expect("resolves");
        assert!(path_exists(&found));
    }
}
