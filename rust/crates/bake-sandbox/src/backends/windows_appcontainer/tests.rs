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
    grants.validate_shape(&at(&["work", "repo"]), &host())
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
    // The workspace: equal, inside, and containing.
    for (root, what, other) in [
        (at(&["work", "repo"]), "workspace", at(&["work", "repo"])),
        (
            at(&["work", "repo", "src"]),
            "workspace",
            at(&["work", "repo"]),
        ),
        (at(&["work"]), "workspace", at(&["work", "repo"])),
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
        workspace_read_capability_name(path),
        write_capability_name(path),
        read_capability_name(path),
    ];
    // Pinned so a change to the derivation, which would orphan every
    // standing grant, is deliberate. The values are Python's
    // `hashlib.sha256(domain + '\0' + path)`, first 16 bytes in hex.
    assert_eq!(
        names,
        [
            "bake.ws-read.26854e915a2f8cf888cb57e8fbad1cb6",
            "bake.ws-write.4ac5ca9887097f69e30e4c46bbfc566d",
            "bake.read.71b2eba1a341d29fb0b46dd5ebac334b",
        ]
    );
    for name in &names {
        assert!(name.len() <= 64, "{name}");
    }
    assert_ne!(
        workspace_read_capability_name(path),
        workspace_read_capability_name(r"C:\Users\agent\repo2")
    );
    const _: () = assert!(RUN_MONIKER_PREFIX.len() + 32 <= 64);
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
    let tag = ProfileTag::parse("0123456789abcdef").expect("a tag");
    let mut argv = runner_profile_args(
        RunnerMode::WorkspaceWrite,
        &at(&["work", "repo"]),
        &grants,
        Some(&tag),
    );
    argv.extend(args(&["--", "cmd", "/c", "exit 0"]));
    assert_eq!(
        parse(argv),
        Ok(Cli {
            workspace: at(&["work", "repo"]),
            mode: RunnerMode::WorkspaceWrite,
            grants: grants.clone(),
            tag: Some(tag),
            command: args(&["cmd", "/c", "exit 0"]),
        })
    );
    // Untagged, as Bake runs it.
    let mut argv = runner_profile_args(RunnerMode::ReadOnly, &at(&["work"]), &grants, None);
    argv.extend(args(&["--", "x"]));
    assert_eq!(parse(argv).map(|cli| cli.tag), Ok(None));
}

#[test]
fn profile_tags_only_name_a_namespace() {
    for bad in [
        "",
        "0123456789abcde",
        "0123456789abcdef0",
        "0123456789ABCDEF",
        "0123456789abcdeg",
        "0123456789abcd.f",
        "../../../../xxxx",
    ] {
        assert_eq!(ProfileTag::parse(bad), None, "{bad:?}");
        let error = parse(args(&[
            "--workspace",
            "w",
            "--mode",
            "read-only",
            "--profile-tag",
            bad,
            "--",
            "c",
        ]))
        .expect_err("a bad tag");
        assert_eq!(error, format!("invalid profile tag: {bad}"));
    }
    let random = "0123456789abcdef0123456789abcdef";
    assert_eq!(run_moniker(None, random), format!("bake.run.{random}"));
    let tag = ProfileTag::parse("fedcba9876543210").expect("a tag");
    let tagged = run_moniker(Some(&tag), random);
    assert_eq!(tagged, format!("bake.run.fedcba9876543210.{random}"));
    assert!(tagged.starts_with(&tag.moniker_prefix()));
    // Within CreateAppContainerProfile's limit and its name pattern.
    assert!(tagged.len() <= 64, "{tagged}");
    assert!(
        tagged
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.'),
        "{tagged}"
    );
    // Another tag's prefix never matches this tag's names.
    let other = ProfileTag::parse("fedcba987654321f").expect("a tag");
    assert!(!tagged.starts_with(&other.moniker_prefix()));
    let error = parse(args(&[
        "--workspace",
        "w",
        "--mode",
        "read-only",
        "--profile-tag",
        "0123456789abcdef",
        "--profile-tag",
        "0123456789abcdef",
        "--",
        "c",
    ]))
    .expect_err("a repeated tag");
    assert_eq!(error, "duplicate argument: --profile-tag");
}

#[test]
fn cli_errors_name_the_argument() {
    for (list, error) in [
        (&["--mode", "read-only"][..], "missing --workspace"),
        (
            &["--workspace", "w", "--mode", "x", "--", "c"][..],
            "unknown mode: x",
        ),
        // The restricted-token runner's `--temp` is not this runner's.
        (
            &[
                "--workspace",
                "w",
                "--temp",
                "t",
                "--mode",
                "read-only",
                "--",
                "c",
            ][..],
            "unknown argument: --temp",
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
            &["--workspace", "w", "--mode", "read-only", "--"][..],
            "missing command after --",
        ),
        (&["--bogus", "x"][..], "unknown argument: --bogus"),
        (
            &[
                "--workspace",
                "w",
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
    let read = Dir::new("read");
    let file = read.0.join("file.txt");
    std::fs::write(&file, "x").expect("seed");
    let run = |extra: &[OsString]| {
        let mut list = runner_profile_args(
            RunnerMode::ReadOnly,
            &workspace.0,
            &AppContainerGrants::new(),
            None,
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

#[test]
fn the_profile_root_is_the_packages_entry_of_its_own_moniker() {
    let moniker = "bake.run.0123456789abcdef0123456789abcdef";
    let root = at(&["Users", "agent", "AppData", "Local", "Packages", moniker]);
    // GetAppContainerFolderPath reports the profile's LOCALAPPDATA, its `AC`
    // folder; the root itself is accepted too.
    assert_eq!(
        profile_root_for(&root.join("AC"), moniker),
        Some(root.clone())
    );
    assert_eq!(
        profile_root_for(&root.join("ac"), moniker),
        Some(root.clone())
    );
    assert_eq!(profile_root_for(&root, moniker), Some(root.clone()));
    let upper = at(&[
        "Users",
        "agent",
        "AppData",
        "Local",
        "PACKAGES",
        &moniker.to_ascii_uppercase(),
    ]);
    assert_eq!(profile_root_for(&upper, moniker), Some(upper.clone()));
    // Anything else is refused rather than guessed at.
    for folder in [
        root.join("AC").join("Temp"),
        at(&[
            "Users",
            "agent",
            "AppData",
            "Local",
            "Packages",
            "bake.run.other",
            "AC",
        ]),
        at(&["Users", "agent", "AppData", "Local", moniker, "AC"]),
        at(&[moniker]),
        at(&[]),
    ] {
        assert_eq!(
            profile_root_for(&folder, moniker),
            None,
            "{}",
            folder.display()
        );
    }
}

#[test]
fn a_grant_root_may_not_contain_the_profile() {
    let packages = at(&["Users", "agent", "AppData", "Local", "Packages"]);
    let root = packages.join("bake.run.0123");
    let elsewhere = at(&["work", "repo"]);
    assert_eq!(
        check_profile_outside_grants(&root, &elsewhere, &[at(&["tools"])]),
        Ok(())
    );
    // A sibling profile's tree is not this one's.
    assert_eq!(
        check_profile_outside_grants(&root, &packages.join("bake.run.01"), &[]),
        Ok(())
    );
    for workspace in [at(&["Users", "agent"]), packages.clone(), root.clone()] {
        let error = check_profile_outside_grants(&root, &workspace, &[])
            .expect_err("a workspace holding the profile");
        assert!(error.contains("the workspace"), "{error}");
    }
    let error = check_profile_outside_grants(&root, &elsewhere, &[at(&["tools"]), packages])
        .expect_err("a read root holding the profile");
    assert!(error.contains("the read root"), "{error}");
}

#[test]
fn only_a_successful_creation_owns_the_profile() {
    assert_eq!(classify_profile_creation(0), ProfileCreation::Created);
    assert_eq!(classify_profile_creation(1), ProfileCreation::Created);
    assert_eq!(
        classify_profile_creation(0x8007_00B7_u32 as i32),
        ProfileCreation::AlreadyExists
    );
    for status in [0x8007_0005_u32, 0x8007_0057, 0x8000_4005] {
        assert_eq!(
            classify_profile_creation(status as i32),
            ProfileCreation::Failed,
            "{status:#010x}"
        );
    }
}

#[test]
fn the_profile_write_mask_is_every_mutation_and_no_read() {
    // icacls `WD,AD,WEA,DC,WA,D,WDAC,WO`.
    assert_eq!(PROFILE_WRITE_MASK, 0x000D_0156);
    // Nothing a read or traversal needs: list, read data, read EA, execute
    // or traverse, read attributes, READ_CONTROL, SYNCHRONIZE.
    assert_eq!(PROFILE_WRITE_MASK & READ_EXECUTE, 0);
    // Every write right the workspace grant gives, and the DACL and owner.
    assert_eq!(
        PROFILE_WRITE_MASK,
        (WORKSPACE_WRITE_MASK & !READ_EXECUTE) | 0x0004_0000 | 0x0008_0000
    );
}
