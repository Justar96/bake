//! The plugin store and `plugins.json`: install, enable, disable, failure
//! records, and the strict reads a session depends on. Helper processes
//! are exercised by `bake-cli`'s session tests, which have the binary.

use std::path::{Path, PathBuf};

use serde_json::json;

use super::*;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::plugins::veto::Failure;
use crate::test_support::TempDir;

fn guard() -> WasmPackage {
    WasmPackage::read_dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/guard"))
        .expect("the committed guard package")
}

/// A setup whose helper is never started by these tests.
fn setup() -> WasmSetup {
    WasmSetup {
        command: Some(crate::plugins::wasm::HelperCommand::new(
            "/nonexistent",
            ["x"],
        )),
        ..WasmSetup::default()
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn kinds(loaded: &LoadedPlugins) -> Vec<(String, &'static str)> {
    loaded
        .registry
        .plugins
        .iter()
        .map(|registered| {
            (
                registered.manifest.id().as_str().to_owned(),
                match &registered.plugin {
                    Contribution::Trusted(_) => "trusted",
                    Contribution::Wasm(_) => "wasm",
                    Contribution::Blocked(failure) => failure.kind.as_str(),
                },
            )
        })
        .collect()
}

fn store_file(home: &PluginHome, hash: &str, name: &str) -> PathBuf {
    home.store_dir().join(hash).join(name)
}

#[test]
fn installing_copies_the_bytes_read_under_their_hash_read_only() {
    let dir = TempDir::new("plugin-install");
    let home = PluginHome::new(dir.path());
    let package = guard();
    let installed = home.install(&package).expect("install");
    assert_eq!(installed.id, "guard");
    assert!(!installed.already);
    assert_eq!(installed.sha256, package_hash(&package));
    let wasm = store_file(&home, &installed.sha256, "plugin.wasm");
    assert_eq!(std::fs::read(&wasm).unwrap(), package.component());
    assert!(std::fs::metadata(&wasm).unwrap().permissions().readonly());
    // No temporary directory is left beside the copy.
    let names: Vec<_> = std::fs::read_dir(home.store_dir())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, [std::ffi::OsString::from(&installed.sha256)]);
    // Again: the copy is checked and kept.
    assert!(home.install(&package).expect("reinstall").already);
    // Nothing is enabled by installing.
    assert!(home.enabled().unwrap().is_empty());
}

#[test]
fn the_hash_covers_both_files_and_their_split() {
    let a = WasmPackage::new(
        br#"{"api":"0.1.0","id":"g","version":"1","vetoTools":["write"]}"#,
        b"\0asm\x0d\0\x01\0".to_vec(),
    )
    .unwrap();
    let b = WasmPackage::new(
        br#"{"api":"0.1.0","id":"g","version":"1","vetoTools":["write"]} "#,
        b"\0asm\x0d\0\x01\0".to_vec(),
    )
    .unwrap();
    assert_ne!(package_hash(&a), package_hash(&b));
    assert_eq!(package_hash(&a).len(), 64);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_changed_store_copy_is_unloadable_and_blocks_its_recorded_tools() {
    let dir = TempDir::new("plugin-tamper");
    let home = PluginHome::new(dir.path());
    let installed = home.install(&guard()).unwrap();
    home.enable(&installed.sha256).unwrap();
    assert_eq!(
        kinds(&home.load(&setup(), None).unwrap()),
        [("guard".into(), "wasm")]
    );

    // The same package under another id's record is not run as that id.
    let mut config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(home.config_path()).unwrap()).unwrap();
    let record = config["plugins"]["guard"].clone();
    config["plugins"] = json!({ "other": record });
    std::fs::write(home.config_path(), config.to_string()).unwrap();
    let loaded = home.load(&setup(), None).unwrap();
    assert_eq!(kinds(&loaded), [("other".into(), "unloadable")]);
    assert!(
        loaded.warnings[0].contains("does not match its record"),
        "{:?}",
        loaded.warnings
    );
    home.disable("other").unwrap();
    home.enable(&installed.sha256).unwrap();

    let wasm = store_file(&home, &installed.sha256, "plugin.wasm");
    // The owner can still change a read-only copy; the rehash catches it.
    #[cfg(unix)]
    std::fs::set_permissions(&wasm, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    #[cfg(not(unix))]
    {
        let mut permissions = std::fs::metadata(&wasm).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        std::fs::set_permissions(&wasm, permissions).unwrap();
    }
    let mut bytes = std::fs::read(&wasm).unwrap();
    bytes.push(0);
    std::fs::write(&wasm, bytes).unwrap();
    let loaded = home.load(&setup(), None).unwrap();
    assert_eq!(kinds(&loaded), [("guard".into(), "unloadable")]);
    assert!(
        loaded.warnings[0].contains("blocks edit, write"),
        "{:?}",
        loaded.warnings
    );
    assert!(
        loaded.warnings[0].contains("has changed"),
        "{:?}",
        loaded.warnings
    );
    // The record's tools are what is protected.
    let registered = &loaded.registry.plugins[0];
    assert!(registered.manifest.vetoes("write") && registered.manifest.vetoes("edit"));

    // A removed copy, and a missing helper, are unloadable too.
    std::fs::remove_dir_all(home.store_dir().join(&installed.sha256)).unwrap();
    assert_eq!(
        kinds(&home.load(&setup(), None).unwrap()),
        [("guard".into(), "unloadable")]
    );
    home.install(&guard()).unwrap();
    let no_helper = WasmSetup::default();
    let loaded = home.load(&no_helper, None).unwrap();
    assert_eq!(kinds(&loaded), [("guard".into(), "unloadable")]);
    assert!(loaded.warnings[0].contains("no plugin helper"));
}

#[cfg(unix)]
#[test]
fn a_symlinked_store_copy_is_not_followed() {
    let dir = TempDir::new("plugin-store-link");
    let home = PluginHome::new(dir.path());
    let package = guard();
    let elsewhere = dir.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("plugin.json"), package.manifest_bytes()).unwrap();
    std::fs::write(elsewhere.join("plugin.wasm"), package.component()).unwrap();
    std::fs::create_dir_all(home.store_dir()).unwrap();
    let hash = package_hash(&package);
    std::os::unix::fs::symlink(&elsewhere, home.store_dir().join(&hash)).unwrap();
    let error = home.read_installed(&hash).unwrap_err().to_string();
    assert!(error.contains("without following links"), "{error}");
    // Install refuses to trust it as an existing copy.
    assert!(home.install(&package).is_err());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn enabling_records_the_manifest_and_disabling_removes_the_record() {
    let dir = TempDir::new("plugin-enable");
    let home = PluginHome::new(dir.path());
    assert!(
        home.enable(&"0".repeat(64))
            .unwrap_err()
            .to_string()
            .contains("not installed")
    );
    assert!(
        home.enable("guard")
            .unwrap_err()
            .to_string()
            .contains("not a package hash")
    );
    let installed = home.install(&guard()).unwrap();
    let enabled = home
        .enable(&format!("sha256:{}", installed.sha256))
        .unwrap();
    assert!(enabled.changed);
    assert_eq!(enabled.record.veto_tools, ["edit", "write"]);
    assert!(!home.enable(&installed.sha256).unwrap().changed);

    // Another package with the same id is refused until the first is
    // disabled.
    let other = WasmPackage::new(
        br#"{"api":"0.1.0","id":"guard","version":"9","vetoTools":["write"]}"#,
        guard().component().to_vec(),
    )
    .unwrap();
    let other = home.install(&other).unwrap();
    assert!(
        home.enable(&other.sha256)
            .unwrap_err()
            .to_string()
            .contains("disable it first")
    );

    let text = std::fs::read_to_string(home.config_path()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        value,
        json!({"plugins": {"guard": {
            "sha256": installed.sha256,
            "api": "0.1.0",
            "version": "0.1.0",
            "vetoTools": ["edit", "write"],
        }}})
    );
    assert!(home.disable("guard").unwrap().is_some());
    assert!(home.disable("guard").unwrap().is_none());
    assert!(home.enabled().unwrap().is_empty());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_recorded_failure_blocks_every_later_load_until_disabled() {
    let dir = TempDir::new("plugin-failure");
    let home = PluginHome::new(dir.path());
    let installed = home.install(&guard()).unwrap();
    home.enable(&installed.sha256).unwrap();
    let loaded = home.load(&setup(), None).unwrap();
    let record = loaded.registry.plugins[0].record.clone().expect("a record");
    record
        .record(&Failure::new(
            FailureKind::Trap,
            "the plugin trapped: unreachable",
        ))
        .unwrap();
    // A second failure does not replace the first.
    record
        .record(&Failure::new(FailureKind::Error, "later"))
        .unwrap();
    let enabled = home.enabled().unwrap();
    let failure = enabled["guard"].failure.as_ref().expect("recorded");
    assert_eq!(
        (failure.kind.as_str(), failure.message.as_str()),
        ("trap", "the plugin trapped: unreachable")
    );

    let loaded = home.load(&setup(), None).unwrap();
    assert_eq!(kinds(&loaded), [("guard".into(), "trap")]);
    assert!(loaded.warnings[0].contains("bake-rs plugin disable guard"));
    // Enabling again keeps the failure; disabling then enabling resets it.
    assert!(
        home.enable(&installed.sha256)
            .unwrap()
            .record
            .failure
            .is_some()
    );
    home.disable("guard").unwrap();
    home.enable(&installed.sha256).unwrap();
    assert_eq!(
        kinds(&home.load(&setup(), None).unwrap()),
        [("guard".into(), "wasm")]
    );

    // A record for a plugin since disabled, or re-pinned, writes nothing.
    home.disable("guard").unwrap();
    record
        .record(&Failure::new(FailureKind::Trap, "late"))
        .unwrap();
    assert!(home.enabled().unwrap().is_empty());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn an_invalid_configuration_is_an_error_not_an_empty_set() {
    let dir = TempDir::new("plugin-invalid");
    let home = PluginHome::new(dir.path());
    let installed = home.install(&guard()).unwrap();
    home.enable(&installed.sha256).unwrap();
    let good = std::fs::read_to_string(home.config_path()).unwrap();
    let record = |patch: serde_json::Value| {
        let mut value: serde_json::Value = serde_json::from_str(&good).unwrap();
        for (key, member) in patch.as_object().unwrap() {
            value["plugins"]["guard"][key] = member.clone();
        }
        value.to_string()
    };
    for (text, expected) in [
        ("{".to_owned(), "is invalid"),
        ("[]".to_owned(), "expected a JSON object"),
        (r#"{"plugins":{},"extra":1}"#.to_owned(), "unknown field"),
        (record(json!({"extra": 1})), "unknown field"),
        (record(json!({"vetoTools": []})), "lists no tool"),
        (record(json!({"vetoTools": ["write", "edit"]})), "sorted"),
        (record(json!({"sha256": "abc"})), "not a package hash"),
        (record(json!({"api": "0.2.0"})), "not supported"),
        (
            record(json!({"failure": {"kind": "unloadable", "message": "x", "atMs": 1}})),
            "not one that is recorded",
        ),
        (
            record(json!({"failure": {"kind": "made-up", "message": "x", "atMs": 1}})),
            "not one that is recorded",
        ),
    ] {
        std::fs::write(home.config_path(), &text).unwrap();
        let error = home.load(&setup(), None).unwrap_err().to_string();
        assert!(error.contains(expected), "{text}: {error}");
        assert!(error.contains("Repair it, or remove it"), "{error}");
        // Writes refuse it too, and leave it as it was.
        assert!(home.disable("guard").is_err(), "{text}");
        assert_eq!(std::fs::read_to_string(home.config_path()).unwrap(), text);
    }
    let mut big = String::from(r#"{"plugins":{}}"#);
    big.push_str(&" ".repeat(MAX_CONFIG_BYTES as usize));
    std::fs::write(home.config_path(), &big).unwrap();
    assert!(
        home.enabled()
            .unwrap_err()
            .to_string()
            .contains("larger than")
    );
}

#[cfg(unix)]
#[test]
fn a_symlinked_configuration_is_refused() {
    let dir = TempDir::new("plugin-config-link");
    let home = PluginHome::new(dir.path());
    std::fs::create_dir_all(dir.join("plugins")).unwrap();
    std::fs::write(dir.join("real.json"), r#"{"plugins":{}}"#).unwrap();
    std::os::unix::fs::symlink(dir.join("real.json"), home.config_path()).unwrap();
    let error = home.enabled().unwrap_err().to_string();
    assert!(error.contains("is a symbolic link"), "{error}");
}

/// Configuration and store copies others can change are not trusted:
/// group- or world-writable files and directories, and a plugins directory
/// in the session's workspace or in a git working tree.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn files_and_places_others_can_change_are_not_trusted() {
    use std::os::unix::fs::PermissionsExt;
    let mode = |path: &Path, bits: u32| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(bits)).unwrap();
    };
    let dir = TempDir::new("plugin-trust");
    let home = PluginHome::new(&dir.join("home"));
    let installed = home.install(&guard()).unwrap();
    home.enable(&installed.sha256).unwrap();
    assert_eq!(
        kinds(&home.load(&setup(), None).unwrap()),
        [("guard".into(), "wasm")]
    );

    // plugins.json, then a directory above it.
    mode(&home.config_path(), 0o620);
    let error = home.load(&setup(), None).unwrap_err().to_string();
    assert!(
        error.contains("is not trusted") && error.contains("chmod go-w"),
        "{error}"
    );
    mode(&home.config_path(), 0o600);
    mode(&dir.join("home"), 0o757);
    let error = home.load(&setup(), None).unwrap_err().to_string();
    assert!(
        error.contains("writable by its group or by others"),
        "{error}"
    );
    // Sticky, as `/tmp` is: accepted.
    mode(&dir.join("home"), 0o1777);
    assert!(home.load(&setup(), None).is_ok());
    mode(&dir.join("home"), 0o755);

    // A store copy others can write is unloadable, its tools still blocked.
    let copy = home.store_dir().join(&installed.sha256);
    mode(&copy, 0o777);
    let loaded = home.load(&setup(), None).unwrap();
    assert_eq!(kinds(&loaded), [("guard".into(), "unloadable")]);
    assert!(
        loaded.warnings[0].contains("not trusted"),
        "{:?}",
        loaded.warnings
    );
    mode(&copy, 0o700);

    // The session's workspace contains the home, or lies inside it.
    for workspace in [dir.path().to_path_buf(), dir.join("home/plugins/store")] {
        let error = home
            .load(&setup(), Some(&workspace))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("overlaps the session's workspace"),
            "{error}"
        );
    }
    let elsewhere = dir.join("ws");
    std::fs::create_dir_all(&elsewhere).unwrap();
    assert!(home.load(&setup(), Some(&elsewhere)).is_ok());

    // A home inside a git working tree.
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    let error = home
        .load(&setup(), Some(&elsewhere))
        .unwrap_err()
        .to_string();
    assert!(error.contains("inside the git working tree"), "{error}");
    // With nothing enabled, no placement rule applies: an empty home works.
    home.disable("guard").unwrap();
    assert!(
        home.load(&setup(), Some(dir.path()))
            .unwrap()
            .registry
            .is_empty()
    );
}

/// A symbolic link on the way to the home must be the user's own.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_linked_home_is_checked_through_its_link() {
    let dir = TempDir::new("plugin-trust-link");
    let real = PluginHome::new(&dir.join("real"));
    let installed = real.install(&guard()).unwrap();
    real.enable(&installed.sha256).unwrap();
    std::os::unix::fs::symlink(dir.join("real"), dir.join("linked")).unwrap();
    let linked = PluginHome::new(&dir.join("linked"));
    assert_eq!(
        kinds(&linked.load(&setup(), None).unwrap()),
        [("guard".into(), "wasm")]
    );
}

/// Where owners and permissions cannot be checked, nothing is enabled: an
/// enabled configuration stops setup, and enabling is refused, but an empty
/// home, an empty configuration, install, and disable work.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[test]
fn an_enabled_configuration_is_refused_where_trust_cannot_be_checked() {
    let dir = TempDir::new("plugin-unsupported");
    let home = PluginHome::new(dir.path());
    assert!(home.load(&setup(), None).unwrap().registry.is_empty());
    let installed = home.install(&guard()).unwrap();
    let error = home.enable(&installed.sha256).unwrap_err().to_string();
    assert!(error.contains("cannot yet check who can change"), "{error}");
    std::fs::write(home.config_path(), r#"{"plugins":{}}"#).unwrap();
    assert!(home.load(&setup(), None).unwrap().registry.is_empty());
    let config = json!({ "plugins": { "guard": {
        "sha256": installed.sha256,
        "api": "0.1.0",
        "version": "0.1.0",
        "vetoTools": ["edit", "write"],
    } } });
    std::fs::write(home.config_path(), config.to_string()).unwrap();
    let error = home.load(&setup(), None).unwrap_err().to_string();
    assert!(error.contains("cannot yet check who can change"), "{error}");
    assert!(home.disable("guard").unwrap().is_some());
    assert!(home.load(&setup(), None).unwrap().registry.is_empty());
}

/// A file or directory is checked as opened: a group-writable directory
/// above the home is refused even when the path to it is the user's own
/// symbolic link.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn checks_follow_the_opened_objects_not_the_given_path() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new("plugin-trust-bound");
    let shared = dir.join("shared");
    std::fs::create_dir_all(&shared).unwrap();
    let real = PluginHome::new(&shared.join("home"));
    let installed = real.install(&guard()).unwrap();
    real.enable(&installed.sha256).unwrap();
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o775)).unwrap();
    std::os::unix::fs::symlink(shared.join("home"), dir.join("linked")).unwrap();
    let error = PluginHome::new(&dir.join("linked"))
        .load(&setup(), None)
        .unwrap_err()
        .to_string();
    assert!(error.contains("writable by its group"), "{error}");
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        PluginHome::new(&dir.join("linked"))
            .load(&setup(), None)
            .is_ok()
    );
}

/// A macOS ACL entry that allows others to write is refused, though the
/// mode bits are private.
#[cfg(target_os = "macos")]
#[test]
fn a_macos_acl_that_allows_writes_is_refused() {
    let dir = TempDir::new("plugin-trust-acl");
    let home = PluginHome::new(dir.path());
    let installed = home.install(&guard()).unwrap();
    home.enable(&installed.sha256).unwrap();
    let chmod = |args: &[&str]| {
        let status = std::process::Command::new("/bin/chmod")
            .args(args)
            .arg(home.config_path())
            .status()
            .unwrap();
        assert!(status.success(), "chmod {args:?}");
    };
    chmod(&["+a", "everyone deny write"]);
    assert!(home.load(&setup(), None).is_ok());
    chmod(&["-N"]);
    chmod(&["+a", "everyone allow write"]);
    let error = home.load(&setup(), None).unwrap_err().to_string();
    assert!(error.contains("which allows write_data"), "{error}");
    chmod(&["-N"]);
    assert!(home.load(&setup(), None).is_ok());
}
