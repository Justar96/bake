//! The checks the helper runs on a component before compiling it: a
//! validating parse, then the top-level imports and exports against the
//! allowlist. Nothing here compiles code, and a refused component never
//! reaches the compiler.

use wasmparser::{Encoding, Parser, Payload, Validator, WasmFeatures};

use super::SUPPORTED_API_VERSIONS;

/// The WASI 0.2 packages a component may import. All of them are linked to
/// an empty context: no preopened directory, environment, arguments,
/// stdin, socket address, or name lookup.
const WASI_PACKAGES: [&str; 6] = ["cli", "io", "clocks", "random", "filesystem", "sockets"];

/// The most top-level imports a component may declare; the Rust standard
/// library's adapter imports about 30.
const MAX_IMPORTS: usize = 64;

/// Checks `bytes` as a plugin component for API version `api`: a valid
/// component binary whose top-level imports are WASI 0.2 interfaces from
/// [`WASI_PACKAGES`] or `bake:plugin/host@<api>`, and whose only export is
/// `bake:plugin/plugin@<api>`. The error names the first refused item.
pub(crate) fn check_component(bytes: &[u8], api: &str) -> Result<(), String> {
    if !SUPPORTED_API_VERSIONS.contains(&api) {
        return Err(format!(
            "plugin API version {api} is not supported; this Bake supports {}",
            SUPPORTED_API_VERSIONS.join(", ")
        ));
    }
    if !Parser::is_component(bytes) {
        return Err("not a WebAssembly component binary".to_owned());
    }
    Validator::new_with_features(WasmFeatures::default())
        .validate_all(bytes)
        .map_err(|error| format!("invalid component: {error}"))?;

    let host = format!("bake:plugin/host@{api}");
    let plugin = format!("bake:plugin/plugin@{api}");
    let mut imports = 0usize;
    let mut exported_plugin = false;
    // Nested modules and components open with `Version` and close with
    // `End`; only depth 1, the outer component, faces the host.
    let mut depth = 0usize;
    for payload in Parser::new(0).parse_all(bytes) {
        let payload = payload.map_err(|error| format!("invalid component: {error}"))?;
        match payload {
            Payload::Version { encoding, .. } => {
                if depth == 0 && encoding != Encoding::Component {
                    return Err("not a WebAssembly component binary".to_owned());
                }
                depth += 1;
            }
            Payload::End(_) => depth = depth.saturating_sub(1),
            Payload::ComponentImportSection(reader) if depth == 1 => {
                for import in reader {
                    let import = import.map_err(|error| format!("invalid component: {error}"))?;
                    imports += 1;
                    if imports > MAX_IMPORTS {
                        return Err(format!(
                            "the component declares more than {MAX_IMPORTS} imports"
                        ));
                    }
                    check_import(import.name.name, &host)?;
                }
            }
            Payload::ComponentExportSection(reader) if depth == 1 => {
                for export in reader {
                    let export = export.map_err(|error| format!("invalid component: {error}"))?;
                    let name = export.name.name;
                    if name == plugin && !exported_plugin {
                        exported_plugin = true;
                    } else {
                        return Err(refused("export", name, &plugin));
                    }
                }
            }
            _ => {}
        }
    }
    if !exported_plugin {
        return Err(format!("the component does not export {plugin}"));
    }
    Ok(())
}

fn check_import(name: &str, host: &str) -> Result<(), String> {
    if name == host {
        return Ok(());
    }
    if let Some((package, version)) = name
        .strip_prefix("wasi:")
        .and_then(|rest| rest.split_once('@'))
        && let Some((package, interface)) = package.split_once('/')
        && WASI_PACKAGES.contains(&package)
        && !interface.is_empty()
        && version
            .strip_prefix("0.2.")
            .is_some_and(|patch| !patch.is_empty() && patch.bytes().all(|b| b.is_ascii_digit()))
    {
        return Ok(());
    }
    Err(refused("import", name, host))
}

/// The refusal text; a `bake:plugin` item of another version names both.
fn refused(what: &str, name: &str, expected: &str) -> String {
    let name = super::super::truncate(name, 256);
    if name.starts_with("bake:plugin/") {
        format!("{what} {name} is not supported: this Bake links {expected}")
    } else {
        format!("{what} {name} is not allowed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_follow_the_allowlist() {
        let host = "bake:plugin/host@0.1.0";
        for allowed in [
            host,
            "wasi:cli/environment@0.2.0",
            "wasi:io/streams@0.2.12",
            "wasi:sockets/tcp@0.2.6",
            "wasi:filesystem/preopens@0.2.3",
        ] {
            assert!(check_import(allowed, host).is_ok(), "{allowed}");
        }
        for refused_name in [
            "bake:plugin/host@0.2.0",
            "bake:plugin/state@0.1.0",
            "wasi:http/outgoing-handler@0.2.0",
            "wasi:cli/environment@0.3.0",
            "wasi:cli/environment@0.2",
            "wasi:cli/environment@0.2.x",
            "wasi:cli@0.2.0",
            "wasi:cli/@0.2.0",
            "env",
            "",
        ] {
            assert!(check_import(refused_name, host).is_err(), "{refused_name}");
        }
        assert_eq!(
            check_import("bake:plugin/host@0.2.0", host).unwrap_err(),
            "import bake:plugin/host@0.2.0 is not supported: this Bake links bake:plugin/host@0.1.0"
        );
    }

    #[test]
    fn non_components_and_unknown_versions_are_refused() {
        assert!(
            check_component(b"\0asm\x01\0\0\0", "0.1.0")
                .unwrap_err()
                .contains("not a WebAssembly component")
        );
        assert!(check_component(b"(component)", "0.1.0").is_err());
        assert!(
            check_component(b"\0asm\x0d\0\x01\0garbage", "0.1.0")
                .unwrap_err()
                .starts_with("invalid component")
        );
        assert!(
            check_component(b"\0asm\x0d\0\x01\0", "0.2.0")
                .unwrap_err()
                .contains("0.2.0 is not supported")
        );
        // An empty component exports nothing.
        assert_eq!(
            check_component(b"\0asm\x0d\0\x01\0", "0.1.0").unwrap_err(),
            "the component does not export bake:plugin/plugin@0.1.0"
        );
    }
}
