//! The platform profile builders, ported from
//! `packages/sandbox/sandbox-local/src/profiles.ts` (`bwrapProfileArgs`,
//! `landlockProfileArgs`, `sbplString`, `seatbeltProfileArgs`) and
//! `grantArgs` in `@deepseek-ai/node-addon-system/landlock-run`
//! (`native/system/packages/entry/src/index.ts`).
//!
//! Every builder takes its writable directories from
//! [`SandboxPolicy::writable_roots`], so the three backends and the
//! in-process file fence grant the same canonical set.

use std::ffi::OsString;
use std::path::PathBuf;

use crate::policy::SandboxPolicy;

/// The bubblewrap profile arguments for `policy`, before the `--` separator
/// and the command (`bwrapProfileArgs`): the whole tree bound read-only, a
/// fresh `/dev`, a private PID namespace with its own `/proc`, death with
/// the parent, a new terminal session (`--new-session`, which TypeScript did
/// not pass: without it a command could inject keystrokes into Bake's
/// terminal through `TIOCSTI`), and under `workspace-write` a read-write
/// bind of every
/// writable root, parents before the roots nested in them.
///
/// The TypeScript profile mounted an ephemeral tmpfs on `/tmp` and bound only
/// the workspace; this one binds the host's writable roots, `/tmp` and the
/// temporary directory included, as Landlock, Seatbelt, and the file tools
/// grant them, so a file one tool writes to `/tmp` is the file the next
/// command reads.
pub fn bwrap_profile_args(policy: &SandboxPolicy) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "--ro-bind",
        "/",
        "/",
        "--dev",
        "/dev",
        "--unshare-pid",
        "--proc",
        "/proc",
        "--die-with-parent",
        "--new-session",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    let mut roots = policy.writable_roots();
    // Stable: equal depths keep `writable_roots` order.
    roots.sort_by_key(|root| root.components().count());
    for root in roots {
        args.push("--bind".into());
        args.push(root.clone().into_os_string());
        args.push(root.into_os_string());
    }
    args
}

/// The Landlock helper grants for `policy` (`landlockProfileArgs` over
/// `grantArgs`): the tree readable and executable, `/dev/null` writable,
/// and under `workspace-write` every writable root. `/dev/null` is the file,
/// not `/dev`: a `/dev` grant would let a read-only command write the host's
/// `/dev/shm`.
pub fn landlock_profile_args(policy: &SandboxPolicy) -> Vec<OsString> {
    let mut read_write = vec![PathBuf::from("/dev/null")];
    read_write.extend(policy.writable_roots());
    let mut args: Vec<OsString> = vec!["--ro".into(), "/".into()];
    for root in read_write {
        args.push("--rw".into());
        args.push(root.into_os_string());
    }
    args
}

/// One path as an SBPL string literal (`sbplString`).
pub fn sbpl_string(path: &str) -> String {
    format!("\"{}\"", path.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The `sandbox-exec` arguments and SBPL profile for `policy`
/// (`seatbeltProfileArgs`): allow by default, deny every file write except
/// the `/dev/null` literal, and under `workspace-write` allow writes beneath
/// each writable root. Seatbelt matches resolved paths, which the canonical
/// roots are (`/tmp` is `/private/tmp` on macOS).
///
/// A root that is not UTF-8 cannot be spelled in SBPL; the error names it
/// and the caller refuses rather than grant a different path.
pub fn seatbelt_profile_args(policy: &SandboxPolicy) -> Result<Vec<OsString>, PathBuf> {
    let mut forms = vec![
        "(version 1)".to_owned(),
        "(allow default)".to_owned(),
        "(deny file-write*)".to_owned(),
        format!("(allow file-write* (literal {}))", sbpl_string("/dev/null")),
    ];
    let roots = policy.writable_roots();
    if !roots.is_empty() {
        let mut subpaths = Vec::with_capacity(roots.len());
        for root in &roots {
            let Some(text) = root.to_str() else {
                return Err(root.clone());
            };
            subpaths.push(format!("(subpath {})", sbpl_string(text)));
        }
        forms.push(format!("(allow file-write* {})", subpaths.join(" ")));
    }
    Ok(vec!["-p".into(), forms.join(" ").into()])
}

#[cfg(test)]
mod tests {
    //! Follows `describe('profile dialects')` in
    //! `packages/sandbox/sandbox-local/tests/local.spec.ts`.
    use super::*;
    use crate::mode::SandboxMode;
    use crate::paths::canonical_path;
    use std::path::Path;

    fn canonical(path: &str) -> OsString {
        canonical_path(Path::new(path)).into_os_string()
    }

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    fn ro() -> SandboxPolicy {
        SandboxPolicy::new(SandboxMode::ReadOnly, "/ws")
    }

    fn ww() -> SandboxPolicy {
        SandboxPolicy::new(SandboxMode::WorkspaceWrite, "/ws")
    }

    /// The canonical, deduplicated temp roots, as `writable_roots` lists them.
    fn temp_roots() -> Vec<OsString> {
        let mut roots: Vec<OsString> = Vec::new();
        for root in [
            canonical("/tmp"),
            canonical_path(&std::env::temp_dir()).into(),
        ] {
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
        roots
    }

    const BWRAP_BASE: [&str; 10] = [
        "--ro-bind",
        "/",
        "/",
        "--dev",
        "/dev",
        "--unshare-pid",
        "--proc",
        "/proc",
        "--die-with-parent",
        "--new-session",
    ];

    #[test]
    fn bwrap_read_only_has_no_writable_mounts() {
        assert_eq!(strings(&bwrap_profile_args(&ro())), BWRAP_BASE);
    }

    #[test]
    fn bwrap_workspace_write_binds_every_writable_root_parents_first() {
        let mut expected: Vec<OsString> = BWRAP_BASE.iter().map(OsString::from).collect();
        let mut roots: Vec<OsString> = vec!["/ws".into()];
        roots.extend(temp_roots().into_iter().filter(|root| root != "/ws"));
        roots.sort_by_key(|root| Path::new(root).components().count());
        for root in roots {
            expected.extend(["--bind".into(), root.clone(), root]);
        }
        assert_eq!(bwrap_profile_args(&ww()), expected);
    }

    #[test]
    fn bwrap_binds_a_nested_extra_root_after_its_parent() {
        let workspace = std::env::temp_dir().join("bake-profiles-nested");
        let policy = SandboxPolicy::new(SandboxMode::WorkspaceWrite, &workspace)
            .with_extra_writable_roots([PathBuf::from("/")]);
        let args = bwrap_profile_args(&policy);
        let binds: Vec<&OsString> = args
            .iter()
            .skip(BWRAP_BASE.len())
            .skip(1)
            .step_by(3)
            .collect();
        assert_eq!(
            binds.first().map(|root| root.as_os_str()),
            Some("/".as_ref())
        );
    }

    #[test]
    fn landlock_read_only_grants_only_dev_null() {
        assert_eq!(
            strings(&landlock_profile_args(&ro())),
            ["--ro", "/", "--rw", "/dev/null"]
        );
    }

    #[test]
    fn landlock_workspace_write_adds_the_writable_roots() {
        let mut expected: Vec<OsString> = ["--ro", "/", "--rw", "/dev/null", "--rw", "/ws"]
            .iter()
            .map(OsString::from)
            .collect();
        for root in temp_roots() {
            expected.extend(["--rw".into(), root]);
        }
        assert_eq!(landlock_profile_args(&ww()), expected);
    }

    const SEATBELT_RO: &str = "(version 1) (allow default) (deny file-write*) (allow file-write* (literal \"/dev/null\"))";

    #[test]
    fn seatbelt_read_only_denies_every_write_but_dev_null() {
        assert_eq!(
            seatbelt_profile_args(&ro()).map(|args| strings(&args)),
            Ok(vec!["-p".to_owned(), SEATBELT_RO.to_owned()])
        );
    }

    #[test]
    fn seatbelt_workspace_write_allows_the_canonical_roots() {
        let mut roots = vec!["/ws".to_owned()];
        for root in temp_roots() {
            let root = root.to_string_lossy().into_owned();
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
        let allow = format!(
            "(allow file-write* {})",
            roots
                .iter()
                .map(|root| format!("(subpath \"{root}\")"))
                .collect::<Vec<_>>()
                .join(" ")
        );
        assert_eq!(
            seatbelt_profile_args(&ww()).map(|args| strings(&args)),
            Ok(vec!["-p".to_owned(), format!("{SEATBELT_RO} {allow}")])
        );
    }

    #[test]
    fn seatbelt_dedups_a_workspace_that_is_the_temp_dir() {
        let policy = SandboxPolicy::new(SandboxMode::WorkspaceWrite, std::env::temp_dir());
        let args = seatbelt_profile_args(&policy).unwrap_or_default();
        let profile = args.get(1).map(|arg| arg.to_string_lossy().into_owned());
        let grant = format!(
            "(subpath \"{}\")",
            canonical_path(&std::env::temp_dir()).display()
        );
        assert_eq!(profile.unwrap_or_default().matches(&grant).count(), 1);
    }

    #[test]
    fn sbpl_strings_escape_quotes_and_backslashes() {
        assert_eq!(sbpl_string(r#"/a "b"\c"#), r#""/a \"b\"\\c""#);
    }

    #[cfg(unix)]
    #[test]
    fn seatbelt_refuses_a_root_it_cannot_spell() {
        use std::os::unix::ffi::OsStringExt;
        let root = PathBuf::from(OsString::from_vec(b"/ws-\xff".to_vec()));
        let policy = SandboxPolicy::new(SandboxMode::WorkspaceWrite, &root);
        assert_eq!(seatbelt_profile_args(&policy), Err(root));
    }
}
