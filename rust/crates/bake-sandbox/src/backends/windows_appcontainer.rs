//! The Windows AppContainer runner: an **unqualified prototype** of the
//! confinement D24 selects for Windows since its 2026-10-17 amendment
//! (AppContainer with explicit read and network grants). No default chain
//! selects it; [`LocalProvider::with_chain`](super::LocalProvider::with_chain)
//! reaches it for native tests, and it reports
//! [`Enforcement::Partial`](crate::Enforcement::Partial) until a native
//! qualification records otherwise.
//!
//! Pi has no sandbox; nothing here comes from Pi. The runner reuses the
//! retained restricted-token runner's seams ([`super::windows_acl`]: the CLI
//! shape, command-line quoting, the per-path DACL lock and merge, the
//! kill-on-close Job given at creation, the drain before cleanup, and the
//! private temp directory), which port Bake's TypeScript
//! `packages/sandbox/sandbox-windows-acl`. The AppContainer launch follows
//! Microsoft's "Launch an AppContainer" sequence and the mechanism probe in
//! `tests/enforcement/windows_mechanisms.rs`.
//!
//! ```text
//! <exe> __bake-windows-appcontainer-run --workspace <dir> --temp <dir>
//!     --mode <read-only|workspace-write> [--read <dir>]... [--network <grant>]... -- <argv>...
//! ```
//!
//! # Identities
//!
//! | Identity | Derived from | Granted | Lifetime |
//! |---|---|---|---|
//! | Package SID | AppContainer profile [`profile_name`] of the canonical workspace | [`READ_EXECUTE`] on the workspace | Standing profile and ACE |
//! | Workspace write capability | [`write_capability_name`] | [`WORKSPACE_WRITE_MASK`] on the workspace | Standing ACE; in the token only under `workspace-write` |
//! | Read capability, one per read root | [`read_capability_name`] of the canonical root | [`READ_EXECUTE`] on that root | Standing ACE; in the token only when the run grants that root |
//! | Temp capability | a random name per run | [`WORKSPACE_WRITE_MASK`] on the run's private directory | Revoked, and the directory removed, after the tree drains |
//! | Network capabilities | [`NetworkGrant`] | — | In the token only when granted |
//!
//! An AppContainer access check needs both the user's own ACEs and an ACE
//! for the package SID or one of the token's capability SIDs (Microsoft,
//! "Launch an AppContainer"). Another workspace's package and capabilities
//! are never in this token, so its standing ACEs do not apply here, and two
//! runs on one workspace hold different temp capabilities.
//!
//! Every grant merges one explicit ACE into the DACL read under the per-path
//! lock, keeping every other ACE; revocation removes only the run's temp
//! capability from the DACL read again under that lock. No stale DACL
//! snapshot is ever written back, and owner, group, and label stay as they
//! are. Each grant root's own owner must be the token's user or its default
//! owner, which on an elevated administrator token is `Administrators`.
//! That check covers the root only: the ACE then propagates to every
//! descendant, whatever its owner, and descendants are not checked for
//! reparse points or hard links. A grant root must also pass
//! [`AppContainerGrants::validate_shape`] and
//! [`AppContainerGrants::check_on_disk`].
//!
//! # What one run does
//!
//! 1. Validates the arguments and every root, before any grant.
//! 2. Creates the workspace's profile, or derives its SID when it exists.
//! 3. Grants the standing ACEs that are missing (an exact standing ACE skips
//!    the full-tree propagation), and under `workspace-write` creates and
//!    grants the private temp directory.
//! 4. Creates a kill-on-close Job without breakaway, starts the command
//!    suspended with `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES`, born in
//!    that Job (`PROC_THREAD_ATTRIBUTE_JOB_LIST`), inheriting only its
//!    standard handles, with `TMP` and `TEMP` set to the private directory
//!    under `workspace-write`, and resumes it.
//! 5. Waits for the command, terminates the Job, waits until it reports no
//!    active process, revokes the temp grant, removes the private directory,
//!    and exits with the command's status.
//!
//! A failure before the resume prints one `windows-appcontainer-run: ` line
//! and exits [`RUNNER_FAILURE_EXIT`] without running the command; there is
//! no unconfined or restricted-token fallback. When the drain cannot prove
//! the Job empty, the private directory and its grant stay in place.
//!
//! # Qualification blockers
//!
//! The runner must stay out of every default chain, and report
//! [`Enforcement::Partial`](crate::Enforcement::Partial), until native
//! evidence settles each of these:
//!
//! - **Standing host mutations.** Each workspace leaves a per-user profile
//!   (registry and `%LOCALAPPDATA%\Packages` state), and each workspace and
//!   read root leaves explicit ACEs propagated through its whole tree.
//!   Nothing removes them, and they are not inert while their workspace
//!   lives: any later run on that workspace holds the identities.
//! - **Hard-link alias widening.** A file under a granted root that is
//!   hard-linked to a path outside it when an ACE propagates receives that
//!   ACE on the one shared descriptor, so the outside path gains the
//!   workspace's write grant, or a read root's read grant. Nothing detects
//!   or refuses such links.
//! - **Descendant ownership and reparse points.** The owner and reparse
//!   checks apply to grant roots only. Whether propagation follows or
//!   stops at junctions, symbolic links, and other reparse points inside a
//!   root, and which descendants it rewrites, is unproven; a descendant
//!   owned by another principal still receives the inherited ACE.
//! - **Junction and hard-link behavior** must be observed on native
//!   Windows (`tests/enforcement/windows_appcontainer.rs`), not inferred
//!   from the root checks.
//! - **Capability grants.** That a capability SID's ACE satisfies the
//!   AppContainer check, as the package SID's did in the mechanism probe.
//! - **Drain completeness.** The shared `terminate_and_drain` returns as
//!   soon as the Job counts no active process; it waits on member handles
//!   only while that count is nonzero. Cleanup must not rely on that count
//!   alone until native evidence shows no member can outlive it.
//!
//! # Known limits and unverified behavior
//!
//! - **Not qualified.** No native Windows run of this runner is recorded.
//!   The mechanism probe measured AppContainer deletion, rename, and read
//!   confinement and Job containment for a test-built container, not this
//!   runner. Console attachment, inherited pipes, `cmd`, PowerShell, `git`,
//!   and Cargo inside the container are unverified.
//! - **Reads are not limited to the grants.** Windows grants
//!   `ALL APPLICATION PACKAGES` read and execute on most of `%SystemRoot%`
//!   and `Program Files`, and on any object whose DACL names it; those
//!   reads stay allowed. A program outside those paths, the workspace, and
//!   the read roots cannot be loaded.
//! - **Loopback is unsupported.** A container reaches the host's loopback
//!   only through a network isolation loopback exemption, a persistent host
//!   setting this runner never changes: [`NetworkGrant::Loopback`] fails
//!   validation. Microsoft documents loopback within one process as
//!   allowed; that is untested here. Whether the granted
//!   network capabilities behave as documented on every host (firewall
//!   service state, enterprise policy) is unverified.
//! - **Residue.** A renamed root leaves its ACEs behind. A killed runner
//!   leaves its private directory and its temp ACE.
//! - **Grant propagation is eager.** The first grant on a root walks the
//!   whole tree.
//! - **A FAT volume has no DACLs**: a grant root on one fails.
//! - **The runner ignores Ctrl+C** so it survives to clean up, and the
//!   command inherits that setting.
//! - **Path spellings.** Identities hash the canonical path as given; a
//!   different spelling of one directory derives another identity.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

use super::windows_acl::{self, GRANT_MASK, check_directory, sha256};
use crate::paths::{canonical_path, is_path_under};

/// The hidden first argument that turns a Bake executable into this runner.
pub const HELPER_ARG: &str = "__bake-windows-appcontainer-run";

/// The runner's name in its diagnostics.
pub const RUNNER_SIGNATURE: &str = "windows-appcontainer-run";

/// The exit status of every runner failure. The command's own status passes
/// through, 127 included, so a runner failure needs this status and a
/// `windows-appcontainer-run: ` line.
pub const RUNNER_FAILURE_EXIT: i32 = 127;

/// The case-insensitive stderr substrings a denied effect produces under
/// this runner: the restricted-token runner's Win32, .NET, and C runtime
/// texts.
pub const DENIAL_SIGNATURES: [&str; 4] = windows_acl::DENIAL_SIGNATURES;

/// `FILE_GENERIC_READ | FILE_GENERIC_EXECUTE` (`icacls` `RX`): what a read
/// grant gives.
pub const READ_EXECUTE: u32 = 0x0012_00A9;

/// [`READ_EXECUTE`] plus the restricted-token runner's [`GRANT_MASK`]: the
/// workspace-write and temp grants. `WRITE_DAC` and `WRITE_OWNER` stay out,
/// so a confined command cannot widen its own grant.
pub const WORKSPACE_WRITE_MASK: u32 = READ_EXECUTE | GRANT_MASK;

/// The prefix of each run's private temporary directory under `--temp`.
pub const PRIVATE_TEMP_PREFIX: &str = "bake-ac-";

/// The most read roots one run may grant.
pub const MAX_READ_ROOTS: usize = 64;

/// The file-effect mode a run confines under.
pub use windows_acl::RunnerMode;

/// A network capability a run may grant. Without one, the container has no
/// network access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum NetworkGrant {
    /// `internetClient` (`S-1-15-3-1`): outbound connections to the
    /// internet.
    InternetClient,
    /// `internetClientServer` (`S-1-15-3-2`): outbound and inbound internet
    /// connections.
    InternetClientServer,
    /// `privateNetworkClientServer` (`S-1-15-3-3`): connections within the
    /// home or work network.
    PrivateNetworkClientServer,
    /// The host's loopback interface. Unsupported: it needs a network
    /// isolation loopback exemption (`CheckNetIsolation LoopbackExempt`,
    /// `NetworkIsolationSetAppContainerConfig`), a persistent host setting
    /// the runner never changes, so validation refuses it.
    Loopback,
}

impl NetworkGrant {
    /// Every grant, in wire order.
    pub const ALL: [NetworkGrant; 4] = [
        NetworkGrant::InternetClient,
        NetworkGrant::InternetClientServer,
        NetworkGrant::PrivateNetworkClientServer,
        NetworkGrant::Loopback,
    ];

    /// The name on the runner's command line.
    pub fn as_str(self) -> &'static str {
        match self {
            NetworkGrant::InternetClient => "internet-client",
            NetworkGrant::InternetClientServer => "internet-client-server",
            NetworkGrant::PrivateNetworkClientServer => "private-network-client-server",
            NetworkGrant::Loopback => "loopback",
        }
    }

    /// The well-known capability SID, or `None` for [`NetworkGrant::Loopback`],
    /// which no capability grants.
    pub fn capability_sid(self) -> Option<&'static str> {
        match self {
            NetworkGrant::InternetClient => Some("S-1-15-3-1"),
            NetworkGrant::InternetClientServer => Some("S-1-15-3-2"),
            NetworkGrant::PrivateNetworkClientServer => Some("S-1-15-3-3"),
            NetworkGrant::Loopback => None,
        }
    }

    fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|grant| grant.as_str() == text)
    }
}

impl fmt::Display for NetworkGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A grant set the runner refuses, before any DACL changes.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum GrantError {
    /// More than [`MAX_READ_ROOTS`] read roots.
    TooManyReadRoots(usize),
    /// A read root that is not an absolute path.
    NotAbsolute(PathBuf),
    /// A read root that is a volume root, such as `C:\`.
    VolumeRoot(PathBuf),
    /// A read root that is not valid Unicode, so no identity can be derived.
    NotUnicode(PathBuf),
    /// A read root given twice.
    Duplicate(PathBuf),
    /// One read root inside another: the outer one's ACE already reaches
    /// the inner one, and revoking either later would be ambiguous.
    Nested {
        /// The enclosing root.
        outer: PathBuf,
        /// The root inside it.
        inner: PathBuf,
    },
    /// A read root equal to, inside, or containing the workspace or the temp
    /// root, whose ACEs the runner owns.
    Overlaps {
        /// The read root.
        root: PathBuf,
        /// What it overlaps: `workspace` or `temp root`.
        what: &'static str,
        /// That directory.
        other: PathBuf,
    },
    /// A read root equal to or containing a broad host location (the user
    /// profile, the users directory, `AppData`), whose whole tree a grant
    /// would rewrite.
    TooBroad {
        /// The read root.
        root: PathBuf,
        /// The host location it covers.
        covers: PathBuf,
    },
    /// A read root inside, equal to, or containing a system location
    /// (`%SystemRoot%`, `Program Files`, `ProgramData`). Those are not
    /// Bake's to change; `ALL APPLICATION PACKAGES` already reads most of
    /// them.
    System {
        /// The read root.
        root: PathBuf,
        /// The system location.
        system: PathBuf,
    },
    /// A read root that is missing, not a directory, a link or other reparse
    /// point, or not canonical.
    NotADirectory {
        /// The read root.
        root: PathBuf,
        /// Why.
        detail: String,
    },
    /// A network grant given twice.
    DuplicateNetwork(NetworkGrant),
    /// [`NetworkGrant::Loopback`], which needs a loopback exemption the
    /// runner never sets.
    LoopbackUnsupported,
}

impl fmt::Display for GrantError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GrantError::TooManyReadRoots(count) => write!(
                f,
                "{count} read roots exceed the limit of {MAX_READ_ROOTS}"
            ),
            GrantError::NotAbsolute(root) => {
                write!(f, "read root {} is not an absolute path", root.display())
            }
            GrantError::VolumeRoot(root) => write!(
                f,
                "read root {} is a volume root; grant a narrower directory",
                root.display()
            ),
            GrantError::NotUnicode(root) => {
                write!(f, "read root {} is not valid Unicode", root.display())
            }
            GrantError::Duplicate(root) => {
                write!(f, "read root {} is given more than once", root.display())
            }
            GrantError::Nested { outer, inner } => write!(
                f,
                "read root {} lies inside read root {}; grant only one of them",
                inner.display(),
                outer.display()
            ),
            GrantError::Overlaps { root, what, other } => write!(
                f,
                "read root {} overlaps the {what} {}; read roots must lie outside it and must not contain it",
                root.display(),
                other.display()
            ),
            GrantError::TooBroad { root, covers } => write!(
                f,
                "read root {} covers {}; grant a narrower directory",
                root.display(),
                covers.display()
            ),
            GrantError::System { root, system } => write!(
                f,
                "read root {} overlaps the system location {}, which Bake does not change",
                root.display(),
                system.display()
            ),
            GrantError::NotADirectory { root, detail } => {
                write!(f, "read root {} is unusable: {detail}", root.display())
            }
            GrantError::DuplicateNetwork(grant) => {
                write!(f, "network grant {grant} is given more than once")
            }
            GrantError::LoopbackUnsupported => f.write_str(
                "loopback network access is unsupported: an AppContainer reaches the host's loopback only through a network isolation loopback exemption, a persistent host setting Bake does not change",
            ),
        }
    }
}

impl std::error::Error for GrantError {}

/// The host locations read roots are checked against.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostRoots {
    /// Locations no read root may be inside, equal to, or contain.
    pub system: Vec<PathBuf>,
    /// Locations no read root may equal or contain.
    pub broad: Vec<PathBuf>,
}

impl HostRoots {
    /// This host's locations from the environment: `SystemRoot`, `windir`,
    /// `ProgramFiles`, `ProgramFiles(x86)`, `ProgramW6432`, and
    /// `ProgramData` as system locations; `USERPROFILE`, its parent,
    /// `LOCALAPPDATA`, `APPDATA`, and `PUBLIC` as broad ones. Unset and
    /// relative values are skipped; each value is canonicalized.
    pub fn from_env() -> Self {
        let read = |names: &[&str]| -> Vec<PathBuf> {
            let mut out: Vec<PathBuf> = Vec::new();
            for name in names {
                if let Some(value) = std::env::var_os(name) {
                    let path = PathBuf::from(value);
                    if path.is_absolute() {
                        let path = canonical_path(&path);
                        if !out.contains(&path) {
                            out.push(path);
                        }
                    }
                }
            }
            out
        };
        let system = read(&[
            "SystemRoot",
            "windir",
            "ProgramFiles",
            "ProgramFiles(x86)",
            "ProgramW6432",
            "ProgramData",
        ]);
        let mut broad = read(&["USERPROFILE", "LOCALAPPDATA", "APPDATA", "PUBLIC"]);
        if let Some(parent) = std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .and_then(|path| canonical_path(&path).parent().map(Path::to_path_buf))
            && !broad.contains(&parent)
        {
            broad.push(parent);
        }
        Self { system, broad }
    }
}

/// The read roots and network capabilities a run grants beyond the
/// workspace and its private temp directory. Empty by default: no extra
/// reads and no network.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppContainerGrants {
    read_roots: Vec<PathBuf>,
    network: Vec<NetworkGrant>,
}

impl AppContainerGrants {
    /// No read roots and no network.
    pub fn new() -> Self {
        Self::default()
    }

    /// The same grants with `root` readable. The root must be the canonical
    /// path of a directory this user owns; see
    /// [`validate_shape`](Self::validate_shape) and
    /// [`check_on_disk`](Self::check_on_disk).
    pub fn with_read_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.read_roots.push(root.into());
        self
    }

    /// The same grants with `grant` added.
    pub fn with_network(mut self, grant: NetworkGrant) -> Self {
        self.network.push(grant);
        self
    }

    /// The read roots, in the order given.
    pub fn read_roots(&self) -> &[PathBuf] {
        &self.read_roots
    }

    /// The network grants, in the order given.
    pub fn network(&self) -> &[NetworkGrant] {
        &self.network
    }

    /// The path-only checks, which do no I/O: at most [`MAX_READ_ROOTS`]
    /// read roots, each absolute, valid Unicode, not a volume root, given
    /// once, not nested in another, outside and not containing `workspace`
    /// and `temp_root`, not equal to or containing a broad host location,
    /// and not overlapping a system location in either direction; network
    /// grants given once and without [`NetworkGrant::Loopback`].
    /// Comparisons ignore ASCII case on Windows.
    pub fn validate_shape(
        &self,
        workspace: &Path,
        temp_root: &Path,
        host: &HostRoots,
    ) -> Result<(), GrantError> {
        if self.read_roots.len() > MAX_READ_ROOTS {
            return Err(GrantError::TooManyReadRoots(self.read_roots.len()));
        }
        for (index, root) in self.read_roots.iter().enumerate() {
            if !root.is_absolute() {
                return Err(GrantError::NotAbsolute(root.clone()));
            }
            if root.to_str().is_none() {
                return Err(GrantError::NotUnicode(root.clone()));
            }
            if root.parent().is_none() {
                return Err(GrantError::VolumeRoot(root.clone()));
            }
            for earlier in &self.read_roots[..index] {
                if same_path(root, earlier) {
                    return Err(GrantError::Duplicate(root.clone()));
                }
                for (outer, inner) in [(earlier, root), (root, earlier)] {
                    if under(inner, outer) {
                        return Err(GrantError::Nested {
                            outer: outer.clone(),
                            inner: inner.clone(),
                        });
                    }
                }
            }
            for (what, other) in [("workspace", workspace), ("temp root", temp_root)] {
                if under(root, other) || under(other, root) {
                    return Err(GrantError::Overlaps {
                        root: root.clone(),
                        what,
                        other: other.to_path_buf(),
                    });
                }
            }
            if let Some(system) = host
                .system
                .iter()
                .find(|system| under(root, system) || under(system, root))
            {
                return Err(GrantError::System {
                    root: root.clone(),
                    system: system.clone(),
                });
            }
            if let Some(covers) = host.broad.iter().find(|broad| under(broad, root)) {
                return Err(GrantError::TooBroad {
                    root: root.clone(),
                    covers: covers.clone(),
                });
            }
        }
        for (index, grant) in self.network.iter().enumerate() {
            if *grant == NetworkGrant::Loopback {
                return Err(GrantError::LoopbackUnsupported);
            }
            if self.network[..index].contains(grant) {
                return Err(GrantError::DuplicateNetwork(*grant));
            }
        }
        Ok(())
    }

    /// The filesystem checks: each read root exists, is a directory, is not
    /// a symbolic link, junction, or other reparse point, and canonicalizes
    /// to itself, so no component is a link. The runner repeats them just
    /// before granting.
    pub fn check_on_disk(&self) -> Result<(), GrantError> {
        for root in &self.read_roots {
            check_directory("read root", root)
                .and_then(|()| check_not_reparse_point(root))
                .map_err(|detail| GrantError::NotADirectory {
                    root: root.clone(),
                    detail,
                })?;
        }
        Ok(())
    }

    /// The runner arguments for these grants.
    fn args(&self) -> Vec<OsString> {
        let mut args = Vec::new();
        for root in &self.read_roots {
            args.push("--read".into());
            args.push(root.as_os_str().to_owned());
        }
        for grant in &self.network {
            args.push("--network".into());
            args.push(grant.as_str().into());
        }
        args
    }
}

/// Refuses any reparse point, not only the name surrogates `std` reports as
/// links. Off Windows there are none to refuse.
pub(super) fn check_not_reparse_point(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        let meta = std::fs::symlink_metadata(path)
            .map_err(|error| format!("{} cannot be inspected: {error}", path.display()))?;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(format!("{} is a reparse point", path.display()));
        }
    }
    #[cfg(not(windows))]
    let _ = path;
    Ok(())
}

/// Path equality, ignoring ASCII case on Windows.
fn same_path(left: &Path, right: &Path) -> bool {
    folded(left) == folded(right)
}

/// Whether `path` is `root` or lies beneath it, ignoring ASCII case on
/// Windows.
fn under(path: &Path, root: &Path) -> bool {
    is_path_under(&folded(path), &folded(root))
}

fn folded(path: &Path) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(path.to_string_lossy().to_ascii_lowercase())
    } else {
        path.to_path_buf()
    }
}

fn hex_digest(domain: &str, value: &str) -> String {
    sha256(&[domain.as_bytes(), b"\0", value.as_bytes()])[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The AppContainer profile name of a canonical workspace path:
/// `bake.ws.` and 32 hex digits of a domain-separated SHA-256, within the
/// API's 64-character limit.
pub fn profile_name(workspace: &str) -> String {
    format!(
        "bake.ws.{}",
        hex_digest("bake.appcontainer.workspace", workspace)
    )
}

/// The capability name whose SID carries a workspace's write grant.
pub fn write_capability_name(workspace: &str) -> String {
    format!(
        "bake.ws-write.{}",
        hex_digest("bake.appcontainer.write", workspace)
    )
}

/// The capability name whose SID carries a read root's grant.
pub fn read_capability_name(root: &str) -> String {
    format!("bake.read.{}", hex_digest("bake.appcontainer.read", root))
}

/// The runner arguments a provider passes before `--`.
pub fn runner_profile_args(
    mode: RunnerMode,
    workspace: &Path,
    temp_root: &Path,
    grants: &AppContainerGrants,
) -> Vec<OsString> {
    let mut args = windows_acl::runner_profile_args(mode, workspace, temp_root);
    args.extend(grants.args());
    args
}

/// Parsed runner arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Cli {
    workspace: PathBuf,
    temp: PathBuf,
    mode: RunnerMode,
    grants: AppContainerGrants,
    command: Vec<OsString>,
}

/// The run, or the detail of its usage error.
fn parse(args: Vec<OsString>) -> Result<Cli, String> {
    let mut workspace = None;
    let mut temp = None;
    let mut mode = None;
    let mut grants = AppContainerGrants::new();
    let mut rest = args.into_iter();
    let mut command = None;
    while let Some(flag) = rest.next() {
        if flag == "--" {
            command = Some(rest.by_ref().collect::<Vec<_>>());
            break;
        }
        let Some(value) = rest.next() else {
            return Err(format!("missing value after {}", flag.to_string_lossy()));
        };
        let slot = match flag.to_str() {
            Some("--workspace") => &mut workspace,
            Some("--temp") => &mut temp,
            Some("--mode") => &mut mode,
            Some("--read") => {
                grants = grants.with_read_root(value);
                continue;
            }
            Some("--network") => {
                let grant = value
                    .to_str()
                    .and_then(NetworkGrant::parse)
                    .ok_or_else(|| format!("unknown network grant: {}", value.to_string_lossy()))?;
                grants = grants.with_network(grant);
                continue;
            }
            _ => return Err(format!("unknown argument: {}", flag.to_string_lossy())),
        };
        if slot.replace(value).is_some() {
            return Err(format!("duplicate argument: {}", flag.to_string_lossy()));
        }
    }
    let workspace = workspace.ok_or("missing --workspace")?;
    let temp = temp.ok_or("missing --temp")?;
    let mode = match mode.as_ref().and_then(|mode| mode.to_str()) {
        Some("read-only") => RunnerMode::ReadOnly,
        Some("workspace-write") => RunnerMode::WorkspaceWrite,
        _ => {
            let shown = mode.map_or_else(
                || "undefined".to_owned(),
                |mode| mode.to_string_lossy().into_owned(),
            );
            return Err(format!("unknown mode: {shown}"));
        }
    };
    let command = command.unwrap_or_default();
    if command.is_empty() {
        return Err("missing command after --".to_owned());
    }
    if command
        .iter()
        .any(|arg: &OsString| arg.as_encoded_bytes().contains(&0))
    {
        return Err("the command contains a NUL character".to_owned());
    }
    Ok(Cli {
        workspace: workspace.into(),
        temp: temp.into(),
        mode,
        grants,
        command,
    })
}

/// Every check before the first grant: the arguments, both directories,
/// the temp root outside the workspace, and the grants' shape and disk
/// state against `host`.
fn checked(args: Vec<OsString>, host: &HostRoots) -> Result<Cli, String> {
    let cli = parse(args)?;
    check_directory("--workspace", &cli.workspace)?;
    check_not_reparse_point(&cli.workspace)?;
    check_directory("--temp", &cli.temp)?;
    windows_acl::check_temp_root_outside_workspace(&cli.workspace, &cli.temp)?;
    cli.grants
        .validate_shape(&cli.workspace, &cli.temp, host)
        .and_then(|()| cli.grants.check_on_disk())
        .map_err(|error| error.to_string())?;
    if cli.workspace.to_str().is_none() {
        return Err(format!("{} is not valid Unicode", cli.workspace.display()));
    }
    Ok(cli)
}

/// Runs the runner over `args`, the arguments after [`HELPER_ARG`], and never
/// returns: it exits with the command's status, or [`RUNNER_FAILURE_EXIT`]
/// after a `windows-appcontainer-run: ` line. Call it before anything else
/// runs.
pub fn helper_main(args: impl IntoIterator<Item = OsString>) -> ! {
    let code = match checked(args.into_iter().collect(), &HostRoots::from_env()) {
        Ok(cli) => run_confined(&cli),
        Err(detail) => fail(&detail),
    };
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    std::process::exit(code)
}

fn fail(detail: &str) -> i32 {
    eprintln!("{RUNNER_SIGNATURE}: {detail}");
    RUNNER_FAILURE_EXIT
}

#[cfg(windows)]
fn run_confined(cli: &Cli) -> i32 {
    sys::run(cli)
}

#[cfg(not(windows))]
fn run_confined(_cli: &Cli) -> i32 {
    fail("the Windows AppContainer runner runs only on Windows")
}

#[cfg(windows)]
mod sys;

#[cfg(test)]
mod tests;
