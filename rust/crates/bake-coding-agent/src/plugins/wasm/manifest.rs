//! A Wasm plugin's manifest (`plugin.json`) and package: the checks that
//! run in the parent before any helper starts.

use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use serde::Deserialize;

use super::{MAX_COMPONENT_BYTES, MAX_MANIFEST_BYTES, SUPPORTED_API_VERSIONS};
use crate::plugins::{PluginError, PluginManifest};

/// A validated `plugin.json`:
///
/// ```json
/// {"api": "0.1.0", "id": "guard", "version": "0.1.0", "vetoTools": ["edit", "write"]}
/// ```
///
/// `api` is the exact `bake:plugin` version the component implements; the
/// other fields follow [`PluginManifest`]. Unknown fields are refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasmManifest {
    api: String,
    plugin: PluginManifest,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RawManifest {
    api: String,
    id: String,
    version: String,
    veto_tools: Vec<String>,
}

/// Why a package was refused before a helper started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageError {
    /// The manifest is over [`MAX_MANIFEST_BYTES`].
    ManifestTooLarge(usize),
    /// The manifest is not the expected JSON object.
    ManifestSyntax(String),
    /// The manifest names an API version this Bake does not support.
    UnsupportedApi(String),
    /// The id, version, or tool list is invalid.
    Manifest(PluginError),
    /// The component is over [`MAX_COMPONENT_BYTES`].
    ComponentTooLarge(usize),
    /// The bytes are not a WebAssembly component binary.
    NotAComponent,
    /// A package file could not be read, or is not a regular file.
    Read {
        /// `plugin.json` or `plugin.wasm`.
        file: &'static str,
        /// Why.
        message: String,
    },
}

impl std::fmt::Display for PackageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ManifestTooLarge(len) => {
                write!(
                    f,
                    "plugin.json is {len} bytes, over the {MAX_MANIFEST_BYTES}-byte limit"
                )
            }
            Self::ManifestSyntax(error) => write!(f, "plugin.json is invalid: {error}"),
            Self::UnsupportedApi(api) => write!(
                f,
                "plugin API version {api} is not supported; this Bake supports {}",
                SUPPORTED_API_VERSIONS.join(", ")
            ),
            Self::Manifest(error) => write!(f, "plugin.json is invalid: {error}"),
            Self::ComponentTooLarge(len) => {
                write!(
                    f,
                    "the component is {len} bytes, over the {MAX_COMPONENT_BYTES}-byte limit"
                )
            }
            Self::NotAComponent => f.write_str("the file is not a WebAssembly component binary"),
            Self::Read { file, message } => write!(f, "cannot read {file}: {message}"),
        }
    }
}

impl std::error::Error for PackageError {}

impl WasmManifest {
    /// Parses and validates `bytes`.
    pub fn parse(bytes: &[u8]) -> Result<Self, PackageError> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(PackageError::ManifestTooLarge(bytes.len()));
        }
        // Every field is a string or a list of strings, so nesting deeper
        // than two levels fails at its first bracket; serde_json also stops
        // at 128 levels.
        let raw: RawManifest = serde_json::from_slice(bytes).map_err(|error| {
            PackageError::ManifestSyntax(
                crate::plugins::truncate(&error.to_string(), 256).to_owned(),
            )
        })?;
        if !SUPPORTED_API_VERSIONS.contains(&raw.api.as_str()) {
            return Err(PackageError::UnsupportedApi(
                crate::plugins::truncate(&raw.api, 64).to_owned(),
            ));
        }
        if raw.veto_tools.len() > crate::plugins::MAX_VETO_TOOLS {
            return Err(PackageError::Manifest(PluginError::TooManyTools));
        }
        let plugin = PluginManifest::new(&raw.id, &raw.version, raw.veto_tools)
            .map_err(PackageError::Manifest)?;
        Ok(Self {
            api: raw.api,
            plugin,
        })
    }

    /// The `bake:plugin` version.
    pub fn api(&self) -> &str {
        &self.api
    }

    /// The id, version, and vetoed tools.
    pub fn plugin(&self) -> &PluginManifest {
        &self.plugin
    }
}

/// A manifest and its component bytes, checked for size and shape. The
/// component's imports, exports, and validity are checked by the helper,
/// before it compiles.
#[derive(Debug, Clone)]
pub struct WasmPackage {
    manifest: WasmManifest,
    manifest_bytes: Arc<[u8]>,
    component: Arc<[u8]>,
}

impl WasmPackage {
    /// Checks `manifest` (the bytes of `plugin.json`) and `component` (the
    /// bytes of the component binary).
    pub fn new(manifest: &[u8], component: Vec<u8>) -> Result<Self, PackageError> {
        Self::new_parsed(WasmManifest::parse(manifest)?, manifest.to_vec(), component)
    }

    /// Reads the package directory `dir`: its `plugin.json` and its
    /// component, `plugin.wasm`, each opened once without following a
    /// symbolic link (or, on Windows, a reparse point) and read once. Each
    /// read stops one byte past its limit, and anything but a regular file
    /// is refused without reading it. This blocks its thread on file IO.
    pub fn read_dir(dir: &Path) -> Result<Self, PackageError> {
        let manifest_bytes = read_bounded(dir, "plugin.json", MAX_MANIFEST_BYTES)?;
        let manifest = WasmManifest::parse(&manifest_bytes)?;
        let component = read_bounded(dir, "plugin.wasm", MAX_COMPONENT_BYTES)?;
        Self::new_parsed(manifest, manifest_bytes, component)
    }

    fn new_parsed(
        manifest: WasmManifest,
        manifest_bytes: Vec<u8>,
        component: Vec<u8>,
    ) -> Result<Self, PackageError> {
        if component.len() > MAX_COMPONENT_BYTES {
            return Err(PackageError::ComponentTooLarge(component.len()));
        }
        if !wasmparser::Parser::is_component(&component) {
            return Err(PackageError::NotAComponent);
        }
        Ok(Self {
            manifest,
            manifest_bytes: manifest_bytes.into(),
            component: component.into(),
        })
    }

    /// The bytes of `plugin.json` exactly as read.
    pub fn manifest_bytes(&self) -> &[u8] {
        &self.manifest_bytes
    }

    /// The manifest.
    pub fn manifest(&self) -> &WasmManifest {
        &self.manifest
    }

    /// The component bytes.
    pub fn component(&self) -> &[u8] {
        &self.component
    }
}

/// Reads `dir/file` with [`read_regular`], as a package file.
fn read_bounded(dir: &Path, file: &'static str, limit: usize) -> Result<Vec<u8>, PackageError> {
    read_regular(&dir.join(file), limit).map_err(|error| PackageError::Read {
        file,
        message: error.to_string(),
    })
}

/// Why [`read_regular`] read nothing.
#[derive(Debug)]
pub(crate) enum ReadError {
    /// Nothing is at the path.
    NotFound,
    /// The path is a symbolic link, a directory, a device, or a FIFO.
    NotRegular,
    /// Another error.
    Io(std::io::Error),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => f.write_str("it does not exist"),
            Self::NotRegular => f.write_str("not a regular file"),
            Self::Io(error) => write!(f, "{error}"),
        }
    }
}

/// Reads `path`, at most `limit + 1` bytes, so an oversized file is
/// reported by size without being read whole. The open does not follow a
/// symbolic link at the final component (Unix `O_NOFOLLOW`, Windows
/// `FILE_FLAG_OPEN_REPARSE_POINT`), and the opened handle must be a
/// regular file, so what is checked is what is read, once. Blocks.
pub(crate) fn read_regular(path: &Path, limit: usize) -> Result<Vec<u8>, ReadError> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    // A FIFO would block the open itself; the type check below then
    // refuses it.
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(
        &mut options,
        libc::O_NONBLOCK | libc::O_NOFOLLOW,
    );
    #[cfg(windows)]
    std::os::windows::fs::OpenOptionsExt::custom_flags(
        &mut options,
        windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT,
    );
    let handle = match options.open(path) {
        Ok(handle) => handle,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(ReadError::NotFound);
        }
        // `O_NOFOLLOW` refuses a symbolic link with `ELOOP`.
        #[cfg(unix)]
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
            return Err(ReadError::NotRegular);
        }
        Err(error) => return Err(ReadError::Io(error)),
    };
    let metadata = handle.metadata().map_err(ReadError::Io)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ReadError::NotRegular);
    }
    let mut bytes = Vec::new();
    handle
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(ReadError::Io)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMPONENT_HEADER: &[u8] = b"\0asm\x0d\0\x01\0";

    #[test]
    fn a_valid_manifest_parses() {
        let manifest = WasmManifest::parse(
            br#"{"api":"0.1.0","id":"guard","version":"0.1.0","vetoTools":["write","edit"]}"#,
        )
        .unwrap();
        assert_eq!(manifest.api(), "0.1.0");
        assert_eq!(manifest.plugin().id().as_str(), "guard");
        assert_eq!(
            manifest.plugin().veto_tools().collect::<Vec<_>>(),
            ["edit", "write"]
        );
    }

    #[test]
    fn manifests_are_bounded_and_strict() {
        let big = vec![b' '; MAX_MANIFEST_BYTES + 1];
        assert_eq!(
            WasmManifest::parse(&big),
            Err(PackageError::ManifestTooLarge(big.len()))
        );
        let cases: [(&[u8], &str); 7] = [
            (
                br#"{"api":"0.2.0","id":"g","version":"1","vetoTools":["write"]}"#,
                "0.2.0 is not supported",
            ),
            (
                br#"{"api":"0.1.0","id":"G","version":"1","vetoTools":["write"]}"#,
                "invalid plugin id",
            ),
            (
                br#"{"api":"0.1.0","id":"g","version":"1","vetoTools":[]}"#,
                "lists no tool",
            ),
            (
                br#"{"api":"0.1.0","id":"g","version":"1","vetoTools":["write"],"x":1}"#,
                "unknown field",
            ),
            (
                br#"{"api":"0.1.0","id":"g","version":"1"}"#,
                "missing field",
            ),
            (
                br#"{"api":"0.1.0","id":"g","version":"1","vetoTools":[[[["write"]]]]}"#,
                "invalid type",
            ),
            (
                br#"{"api":"0.1.0","id":"g","version":"1","vetoTools":["write"]} x"#,
                "trailing",
            ),
        ];
        for (bytes, expected) in cases {
            let error = WasmManifest::parse(bytes).unwrap_err().to_string();
            assert!(error.contains(expected), "{error}");
        }
        let tools: Vec<String> = (0..33).map(|n| format!("\"t{n}\"")).collect();
        let many = format!(
            r#"{{"api":"0.1.0","id":"g","version":"1","vetoTools":[{}]}}"#,
            tools.join(",")
        );
        assert_eq!(
            WasmManifest::parse(many.as_bytes()),
            Err(PackageError::Manifest(PluginError::TooManyTools))
        );
        // Deep nesting fails at its first bracket, not at serde's limit.
        let deep = format!(
            r#"{{"api":"0.1.0","id":"g","version":"1","vetoTools":{}}}"#,
            "[".repeat(100_000)
        );
        assert!(
            WasmManifest::parse(&deep.as_bytes()[..MAX_MANIFEST_BYTES.min(deep.len())]).is_err()
        );
    }

    #[test]
    fn packages_check_the_component_shape_and_size() {
        let manifest = br#"{"api":"0.1.0","id":"g","version":"1","vetoTools":["write"]}"#;
        assert!(WasmPackage::new(manifest, COMPONENT_HEADER.to_vec()).is_ok());
        assert_eq!(
            WasmPackage::new(manifest, b"\0asm\x01\0\0\0".to_vec()).unwrap_err(),
            PackageError::NotAComponent
        );
        assert_eq!(
            WasmPackage::new(manifest, b"(component)".to_vec()).unwrap_err(),
            PackageError::NotAComponent
        );
        let mut big = COMPONENT_HEADER.to_vec();
        big.resize(MAX_COMPONENT_BYTES + 1, 0);
        assert_eq!(
            WasmPackage::new(manifest, big).unwrap_err(),
            PackageError::ComponentTooLarge(MAX_COMPONENT_BYTES + 1)
        );
    }

    #[test]
    fn package_directories_are_read_within_bounds() {
        let dir = crate::test_support::TempDir::new("wasm-package");
        let manifest = br#"{"api":"0.1.0","id":"g","version":"1","vetoTools":["write"]}"#;
        let missing = WasmPackage::read_dir(dir.path()).unwrap_err();
        assert!(
            matches!(
                missing,
                PackageError::Read {
                    file: "plugin.json",
                    ..
                }
            ),
            "{missing}"
        );

        std::fs::write(dir.join("plugin.json"), manifest).unwrap();
        std::fs::create_dir(dir.join("plugin.wasm")).unwrap();
        // Windows refuses opening a directory as a file; Unix reaches the
        // metadata check. Both must reject it before reading any bytes.
        let directory = WasmPackage::read_dir(dir.path()).unwrap_err();
        assert!(
            matches!(
                directory,
                PackageError::Read {
                    file: "plugin.wasm",
                    ..
                }
            ),
            "{directory}"
        );
        std::fs::remove_dir(dir.join("plugin.wasm")).unwrap();

        // An oversized component is read one byte past the limit, then refused.
        let mut big = COMPONENT_HEADER.to_vec();
        big.resize(MAX_COMPONENT_BYTES + 4096, 0);
        std::fs::write(dir.join("plugin.wasm"), &big).unwrap();
        assert_eq!(
            WasmPackage::read_dir(dir.path()).unwrap_err(),
            PackageError::ComponentTooLarge(MAX_COMPONENT_BYTES + 1)
        );

        std::fs::write(dir.join("plugin.wasm"), COMPONENT_HEADER).unwrap();
        let package = WasmPackage::read_dir(dir.path()).unwrap();
        assert_eq!(package.manifest().plugin().id().as_str(), "g");
        assert_eq!(package.component(), COMPONENT_HEADER);

        std::fs::write(dir.join("plugin.json"), vec![b' '; MAX_MANIFEST_BYTES + 10]).unwrap();
        assert_eq!(
            WasmPackage::read_dir(dir.path()).unwrap_err(),
            PackageError::ManifestTooLarge(MAX_MANIFEST_BYTES + 1)
        );
    }

    /// A symbolic link is refused rather than followed, even to a valid
    /// file, so a source cannot be swapped behind its name.
    #[cfg(unix)]
    #[test]
    fn a_symbolic_link_is_refused() {
        let dir = crate::test_support::TempDir::new("wasm-symlink");
        let manifest = br#"{"api":"0.1.0","id":"g","version":"1","vetoTools":["write"]}"#;
        std::fs::write(dir.join("real.json"), manifest).unwrap();
        std::os::unix::fs::symlink(dir.join("real.json"), dir.join("plugin.json")).unwrap();
        std::fs::write(dir.join("plugin.wasm"), COMPONENT_HEADER).unwrap();
        assert_eq!(
            WasmPackage::read_dir(dir.path()).unwrap_err(),
            PackageError::Read {
                file: "plugin.json",
                message: "not a regular file".to_owned()
            }
        );
        std::fs::remove_file(dir.join("plugin.json")).unwrap();
        std::fs::write(dir.join("plugin.json"), manifest).unwrap();
        let package = WasmPackage::read_dir(dir.path()).unwrap();
        assert_eq!(package.manifest_bytes(), manifest);
    }

    /// A FIFO is refused without blocking on its open.
    #[cfg(unix)]
    #[test]
    fn a_fifo_is_refused_without_blocking() {
        let dir = crate::test_support::TempDir::new("wasm-fifo");
        let path = std::ffi::CString::new(
            dir.join("plugin.json")
                .into_os_string()
                .into_encoded_bytes(),
        )
        .unwrap();
        // SAFETY: a valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        assert_eq!(
            WasmPackage::read_dir(dir.path()).unwrap_err(),
            PackageError::Read {
                file: "plugin.json",
                message: "not a regular file".to_owned()
            }
        );
    }
}
