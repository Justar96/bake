//! Portable runner tests: grant validation, identity names, and the CLI.
//! Paths are the host's own syntax, so the same cases run on Linux and
//! Windows; the Win32 side runs in `tests/enforcement/windows_appcontainer.rs`.

use std::ffi::OsString;
use std::path::PathBuf;

use super::*;

/// A path under a fixed absolute base in the host's syntax.
fn at(parts: &[&str]) -> PathBuf {
    let mut path = if cfg!(windows) {
        PathBuf::from(r"C:\")
    } else {
        PathBuf::from("/")
    };
    for part in parts {
        path.push(part);
    }
    path
}

fn host() -> HostRoots {
    HostRoots {
        system: vec![at(&["Windows"]), at(&["Program Files"])],
        broad: vec![at(&["Users"]), at(&["Users", "agent"])],
    }
}

fn shape(grants: &AppContainerGrants) -> Result<(), GrantError> {
    grants.validate_shape(&at(&["work", "repo"]), &at(&["tmp"]), &host())
}

#[test]
fn narrow_roots_and_capabilities_pass() {
    let grants = AppContainerGrants::new()
        .with_read_root(at(&["Users", "agent", ".cargo"]))
        .with_read_root(at(&["tools", "node"]))
        .with_network(NetworkGrant::InternetClient)
        .with_network(NetworkGrant::PrivateNetworkClientServer);
    assert_eq!(shape(&grants), Ok(()));
    assert_eq!(shape(&AppContainerGrants::new()), Ok(()));
}

#[test]
fn every_unsafe_root_is_refused_before_any_grant() {
    let one = |root: PathBuf| shape(&AppContainerGrants::new().with_read_root(root));
    assert_eq!(
        one(PathBuf::from("relative")),
        Err(GrantError::NotAbsolute("relative".into()))
    );
    assert_eq!(one(at(&[])), Err(GrantError::VolumeRoot(at(&[]))));
    // The workspace and temp root: equal, inside, and containing.
    for (root, what, other) in [
        (at(&["work", "repo"]), "workspace", at(&["work", "repo"])),
        (
            at(&["work", "repo", "src"]),
            "workspace",
            at(&["work", "repo"]),
        ),
        (at(&["work"]), "workspace", at(&["work", "repo"])),
        (at(&["tmp", "x"]), "temp root", at(&["tmp"])),
    ] {
        assert_eq!(
            one(root.clone()),
            Err(GrantError::Overlaps { root, what, other })
        );
    }
    // System locations in both directions.
    assert_eq!(
        one(at(&["Windows", "System32"])),
        Err(GrantError::System {
            root: at(&["Windows", "System32"]),
            system: at(&["Windows"])
        })
    );
    // Broad locations: the profile and the users directory, not below.
    for (root, covers) in [
        (at(&["Users", "agent"]), at(&["Users", "agent"])),
        (at(&["Users"]), at(&["Users"])),
    ] {
        assert_eq!(
            one(root.clone()),
            Err(GrantError::TooBroad { root, covers })
        );
    }
}

#[test]
fn duplicate_nested_and_too_many_roots_are_refused() {
    let a = at(&["tools", "a"]);
    let inner = at(&["tools", "a", "bin"]);
    assert_eq!(
        shape(
            &AppContainerGrants::new()
                .with_read_root(a.clone())
                .with_read_root(a.clone())
        ),
        Err(GrantError::Duplicate(a.clone()))
    );
    for (first, second) in [(&a, &inner), (&inner, &a)] {
        assert_eq!(
            shape(
                &AppContainerGrants::new()
                    .with_read_root(first.clone())
                    .with_read_root(second.clone())
            ),
            Err(GrantError::Nested {
                outer: a.clone(),
                inner: inner.clone()
            })
        );
    }
    let mut many = AppContainerGrants::new();
    for index in 0..=MAX_READ_ROOTS {
        many = many.with_read_root(at(&["tools", &index.to_string()]));
    }
    assert_eq!(
        shape(&many),
        Err(GrantError::TooManyReadRoots(MAX_READ_ROOTS + 1))
    );
}

#[cfg(windows)]
#[test]
fn comparisons_ignore_case_on_windows() {
    let grants = AppContainerGrants::new().with_read_root(PathBuf::from(r"C:\WORK\Repo\src"));
    assert!(matches!(shape(&grants), Err(GrantError::Overlaps { .. })));
    let grants = AppContainerGrants::new()
        .with_read_root(PathBuf::from(r"C:\Tools\A"))
        .with_read_root(PathBuf::from(r"c:\tools\a"));
    assert!(matches!(shape(&grants), Err(GrantError::Duplicate(_))));
}

#[test]
fn loopback_and_duplicate_network_grants_are_refused() {
    assert_eq!(
        shape(&AppContainerGrants::new().with_network(NetworkGrant::Loopback)),
        Err(GrantError::LoopbackUnsupported)
    );
    assert_eq!(
        shape(
            &AppContainerGrants::new()
                .with_network(NetworkGrant::InternetClient)
                .with_network(NetworkGrant::InternetClient)
        ),
        Err(GrantError::DuplicateNetwork(NetworkGrant::InternetClient))
    );
    assert_eq!(
        GrantError::LoopbackUnsupported.to_string(),
        "loopback network access is unsupported: an AppContainer reaches the host's loopback only through a network isolation loopback exemption, a persistent host setting Bake does not change"
    );
}

#[test]
fn network_capabilities_are_the_well_known_sids() {
    let table: Vec<_> = NetworkGrant::ALL
        .iter()
        .map(|grant| (grant.as_str(), grant.capability_sid()))
        .collect();
    assert_eq!(
        table,
        [
            ("internet-client", Some("S-1-15-3-1")),
            ("internet-client-server", Some("S-1-15-3-2")),
            ("private-network-client-server", Some("S-1-15-3-3")),
            ("loopback", None),
        ]
    );
    for grant in NetworkGrant::ALL {
        assert_eq!(NetworkGrant::parse(grant.as_str()), Some(grant));
    }
    assert_eq!(NetworkGrant::parse("Internet-Client"), None);
}

#[test]
fn identities_are_separated_bounded_and_stable() {
    let path = r"C:\Users\agent\repo";
    let names = [
        profile_name(path),
        write_capability_name(path),
        read_capability_name(path),
    ];
    // Pinned so a change to the derivation, which would orphan every
    // standing grant, is deliberate. The values are Python's
    // `hashlib.sha256(domain + '\0' + path)`, first 16 bytes in hex.
    assert_eq!(
        names,
        [
            "bake.ws.1fd401d76a318383c68b5875f8dc73ef",
            "bake.ws-write.4ac5ca9887097f69e30e4c46bbfc566d",
            "bake.read.71b2eba1a341d29fb0b46dd5ebac334b",
        ]
    );
    for name in &names {
        assert!(name.len() <= 64, "{name}");
    }
    assert_ne!(profile_name(path), profile_name(r"C:\Users\agent\repo2"));
    assert_eq!(WORKSPACE_WRITE_MASK, 0x0013_01FF);
    const WRITE_DAC: u32 = 0x0004_0000;
    const WRITE_OWNER: u32 = 0x0008_0000;
    assert_eq!(WORKSPACE_WRITE_MASK & (WRITE_DAC | WRITE_OWNER), 0);
    // The two masks share only SYNCHRONIZE: a read grant carries no write
    // or delete right.
    assert_eq!(READ_EXECUTE & GRANT_MASK, 0x0010_0000);
}

fn args(list: &[&str]) -> Vec<OsString> {
    list.iter().map(OsString::from).collect()
}

#[test]
fn the_cli_round_trips_the_provider_arguments() {
    let grants = AppContainerGrants::new()
        .with_read_root(at(&["tools", "a"]))
        .with_network(NetworkGrant::InternetClient);
    let mut argv = runner_profile_args(
        RunnerMode::WorkspaceWrite,
        &at(&["work", "repo"]),
        &at(&["tmp"]),
        &grants,
    );
    argv.extend(args(&["--", "cmd", "/c", "exit 0"]));
    assert_eq!(
        parse(argv),
        Ok(Cli {
            workspace: at(&["work", "repo"]),
            temp: at(&["tmp"]),
            mode: RunnerMode::WorkspaceWrite,
            grants,
            command: args(&["cmd", "/c", "exit 0"]),
        })
    );
}

#[test]
fn cli_errors_name_the_argument() {
    for (list, error) in [
        (&["--workspace", "w"][..], "missing --temp"),
        (
            &["--workspace", "w", "--temp", "t", "--mode", "x", "--", "c"][..],
            "unknown mode: x",
        ),
        (
            &["--network", "everything", "--", "c"][..],
            "unknown network grant: everything",
        ),
        (&["--read"][..], "missing value after --read"),
        (
            &["--workspace", "w", "--workspace", "w"][..],
            "duplicate argument: --workspace",
        ),
        (
            &[
                "--workspace",
                "w",
                "--temp",
                "t",
                "--mode",
                "read-only",
                "--",
            ][..],
            "missing command after --",
        ),
        (&["--bogus", "x"][..], "unknown argument: --bogus"),
        (
            &[
                "--workspace",
                "w",
                "--temp",
                "t",
                "--mode",
                "read-only",
                "--",
                "cmd",
                "bad\0arg",
            ][..],
            "the command contains a NUL character",
        ),
    ] {
        assert_eq!(parse(args(list)), Err(error.to_owned()), "{list:?}");
    }
}

/// A directory this test owns, removed on drop.
struct Dir(PathBuf);

impl Dir {
    fn new(label: &str) -> Self {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let base = crate::paths::canonical_path(&std::env::temp_dir());
        let dir = base.join(format!(
            "bake-ac-unit-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).expect("create a fresh test directory");
        Self(dir)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn the_runner_refuses_bad_roots_before_running() {
    let workspace = Dir::new("ws");
    let temp = Dir::new("temp");
    let read = Dir::new("read");
    let file = read.0.join("file.txt");
    std::fs::write(&file, "x").expect("seed");
    let run = |extra: &[OsString]| {
        let mut list = runner_profile_args(
            RunnerMode::ReadOnly,
            &workspace.0,
            &temp.0,
            &AppContainerGrants::new(),
        );
        list.extend(extra.iter().cloned());
        list.extend(args(&["--", "cmd"]));
        checked(list, &HostRoots::default())
    };
    let missing = read.0.join("missing");
    for (root, expected) in [
        (&missing, "read root"),
        (&file, "read root"),
        (&workspace.0, "overlaps the workspace"),
    ] {
        let error = run(&["--read".into(), root.clone().into_os_string()])
            .expect_err("a bad read root must be refused");
        assert!(error.contains(expected), "{error}");
    }
    let error = run(&args(&["--network", "loopback"])).expect_err("loopback");
    assert!(
        error.contains("loopback network access is unsupported"),
        "{error}"
    );
    // A temp root inside the workspace is refused in both modes: the
    // standing package ACE would reach every private directory.
    let inner = workspace.0.join("t");
    std::fs::create_dir(&inner).expect("inner temp");
    let mut list = runner_profile_args(
        RunnerMode::ReadOnly,
        &workspace.0,
        &inner,
        &AppContainerGrants::new(),
    );
    list.extend(args(&["--", "cmd"]));
    let error = checked(list, &HostRoots::default()).expect_err("temp inside workspace");
    assert!(
        error.contains("temp root must be outside the workspace"),
        "{error}"
    );
    // The good case passes every check.
    let ok = run(&["--read".into(), read.0.clone().into_os_string()]).expect("valid");
    assert_eq!(ok.grants.read_roots(), std::slice::from_ref(&read.0));
}

#[cfg(unix)]
#[test]
fn a_linked_read_root_is_refused() {
    let read = Dir::new("real");
    let links = Dir::new("links");
    let link = links.0.join("link");
    std::os::unix::fs::symlink(&read.0, &link).expect("link");
    let error = AppContainerGrants::new()
        .with_read_root(&link)
        .check_on_disk()
        .expect_err("a link must be refused");
    assert!(matches!(error, GrantError::NotADirectory { .. }), "{error}");
}
