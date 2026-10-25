//! The Windows restricted-token runner: confine a command's file writes with
//! a `WRITE_RESTRICTED` token whose restricting SIDs carry a workspace
//! capability and a private-temp capability, granted on those directories'
//! DACLs.
//!
//! A port of Bake's retained `packages/sandbox/sandbox-windows-acl`
//! (`index.ts`, `runner.ts`, `token.ts`, `acl.ts`, `grant.ts`,
//! `workspace-sid.ts`, `path-boundary.ts`, `win32-abi.ts`), its process
//! primitives in `packages/subprocess/win32-process/src/process.ts`
//! (`quoteArg`, `buildCommandLine`, the suspended-create, Job-assignment, and
//! resume lifecycle), and the `windows-acl` rung of
//! `packages/sandbox/sandbox-local/src/index.ts`. The mechanism is the one
//! `huoyaoyuan/windows-acl-restrict-poc` (revision `10e4dfb`) demonstrates,
//! with every Win32 call checked: a failure never starts the command
//! unrestricted. Pi has no sandbox; nothing here comes from Pi.
//!
//! Bake serves as its own runner: `bake` dispatches a first
//! argument of [`HELPER_ARG`] to [`helper_main`] before anything else runs.
//!
//! ```text
//! <exe> __bake-windows-acl-run --workspace <dir> --temp <dir> --mode <read-only|workspace-write> -- <argv>...
//! ```
//!
//! # What one run does
//!
//! Under `workspace-write` the runner:
//!
//! 1. grants the workspace's capability SID ([`workspace_write_sid`], derived
//!    from the canonical workspace path) a [`GRANT_MASK`] ACE on the
//!    workspace root, inheritable by every descendant. The ACE stands after
//!    the run: it is the reuse cache, and a later grant that finds the exact
//!    ACE skips the full-tree propagation;
//! 2. creates a random private directory under `--temp` and grants its own
//!    capability SID ([`temp_write_sid`]) there, so two runs on one
//!    workspace cannot write each other's temporary files;
//! 3. creates the restricted token, whose restricting list is the logon SID,
//!    Everyone, and the two capability SIDs, and adds a full-access ACE for
//!    the temp SID to the token's default DACL, so objects the command
//!    creates (anonymous pipes, for one) pass the restricted check;
//! 4. starts the command suspended with that token, inside a new
//!    kill-on-close Job given at creation (`PROC_THREAD_ATTRIBUTE_JOB_LIST`,
//!    so no process of the command ever runs outside it), with the runner's
//!    standard handles as the only inherited handles, `TMP` and `TEMP`
//!    pointing at the private directory, and then resumes it;
//! 5. waits for the command, terminates whatever is left in the Job and
//!    waits until the Job's account proves every member ended, revokes the
//!    temp grant, removes the private directory, and exits with the
//!    command's exit code.
//!
//! Under `read-only` there are no grants and no private directory: the
//! restricting list is the logon SID and Everyone, the default DACL names
//! Everyone, and `TMP` and `TEMP` are left as they are.
//!
//! Every runner failure prints one `windows-acl-run: ` line on stderr and
//! exits [`RUNNER_FAILURE_EXIT`]; a cleanup failure after the command ran
//! prints a `windows-acl-run: cleanup: ` line and keeps the command's exit
//! code.
//!
//! # Differences from the TypeScript runner
//!
//! - The TypeScript provider kept one private temp directory per live
//!   session and workspace and passed its SIDs to the runner; this runner
//!   always takes the TypeScript runner's agentless path, a private
//!   directory per run, so the provider holds no grants and has nothing to
//!   revoke at shutdown. Files a command leaves in `TEMP` do not survive
//!   into the next command.
//! - The child is created inside its Job (`PROC_THREAD_ATTRIBUTE_JOB_LIST`)
//!   in addition to being created suspended, and inherits only its three
//!   standard handles (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`).
//! - The runner waits until its Job's members have all ended before it
//!   cleans up and exits, so the caller's wait on the runner covers the
//!   whole confined tree. The Job is `bake_process::windows_job::Job`, the
//!   one the process owner keeps its Windows trees in: created empty with a
//!   completion port associated before the command exists, so every
//!   process the Job gains is reported to the port. While the command runs,
//!   the runner's wait on it pumps that port in bounded steps, opening each
//!   reported process soon after it starts. After the command's exit it
//!   terminates the Job again on every poll until the account settles:
//!   every reported process held by handle until signalled, with their
//!   distinct number equal to the Job's lifetime count. The Job's active
//!   count alone is never read as the end: it reaches 0 before a terminated
//!   member's handle is signalled, which is how a count-only drain (the
//!   TypeScript `isJobEmpty` in `packages/subprocess/win32-process/src/
//!   process.ts`, and this runner's own until it shared the account) could
//!   report an empty tree with a member still in rundown. When the account
//!   does not settle within its bound (30 s) or contradicts itself, the
//!   runner reports a failure and leaves the private directory and its temp
//!   grant in place rather than remove them under a process that may still
//!   run; the Job's kill-on-close handle then closes with the runner.
//! - The environment is passed as an explicit block; TypeScript had to
//!   rewrite its own environment because Koffi could not pass one.
//!
//! # Deletion is not confined: the default provider refuses
//!
//! A `WRITE_RESTRICTED` token checks its restricting SIDs only for write
//! accesses, and the kernel does not count `DELETE` or `FILE_DELETE_CHILD`
//! as write accesses for files. The second, restricted access check is
//! skipped for them, so the user's own ACEs decide alone. Measured on
//! Windows (`deletion_escapes_the_write_restricted_token` in
//! `tests/enforcement/windows.rs`, private roots whose DACL grants the user
//! `FA`), a confined process:
//!
//! - is granted `DELETE` on a file and `FILE_DELETE_CHILD` on its directory
//!   while `FILE_WRITE_DATA`, `FILE_WRITE_ATTRIBUTES`, `WRITE_DAC`, and
//!   `FILE_ADD_FILE` are denied;
//! - deletes any such file under both modes with a plain `DeleteFileW`, an
//!   open for `DELETE` with either disposition, `std::fs::remove_file`, and
//!   PowerShell's `Remove-Item`;
//! - under `workspace-write`, moves an outside file into the workspace
//!   (the source needs only `DELETE`; the destination is writable).
//!
//! No change to this token closes that: removing the user's SID from the
//! normal check, or dropping `WRITE_RESTRICTED`, also removes the user's
//! reads, and a deny ACE would have to be written onto every path outside
//! the workspace. The Windows default chain therefore leaves the runner
//! out, and the default provider refuses every confined call there with
//! [`DEFAULT_REFUSAL`]. The runner stays
//! reachable through an explicit [`LocalProvider::with_chain`](super::LocalProvider::with_chain)
//! for its qualification tests; that chain reports
//! [`Enforcement::Partial`](crate::Enforcement::Partial), and deletion is
//! one more effect it leaves unconfined. On 2026-10-17 the owner selected
//! AppContainer with explicit read and network grants as the replacement
//! (D24); its unqualified runner is [`super::windows_appcontainer`], which
//! reuses this runner's DACL, Job, and process helpers. Whether a low level's no-write-up policy denies `DELETE` and the
//! parent's `FILE_DELETE_CHILD` is not settled: Microsoft defines that
//! policy over the generic-write mapping, which omits both. The mechanism
//! probe in `tests/enforcement/windows_mechanisms.rs` measures both
//! candidates against this token.
//!
//! # Known limits
//!
//! These are the TypeScript package's verified boundaries, and with the
//! deletion gap above the reason the rung reports
//! [`Enforcement::Partial`](crate::Enforcement::Partial):
//!
//! - **Everyone stays in the restricting list** (without it, early DLL
//!   initialization fails with `0xC0000142`), so a file or directory whose
//!   DACL grants Everyone write access stays writable under both modes. The
//!   `NUL` device is one.
//! - **Hard links alias a file object, not a path.** A workspace file that is
//!   hard-linked to a file outside the workspace when the grant propagates
//!   carries the ACE on the one shared security descriptor, so the outside
//!   path is writable too.
//! - **Only writes are restricted.** Reads, the network, and process
//!   visibility keep the user's access.
//! - **The console is shared.** A child created with `CREATE_NO_WINDOW` or
//!   `CREATE_NEW_CONSOLE` fails DLL initialization under the restricted
//!   token, so the command shares the runner's console.
//! - **A confined process cannot create piped standard handles for its own
//!   children.** Named pipes get the Win32 default security template, which
//!   names no restricting SID; inherited and anonymous-pipe standard handles
//!   work.
//! - **Workspace ACEs are standing directory mutations.** They stay on the
//!   workspace after the run, and a renamed workspace leaves inert ACEs
//!   behind; the workspace must be owned by the user (the owner's implicit
//!   `WRITE_DAC` is what lets the runner edit the DACL). A killed runner
//!   leaves its private directory and inert temp ACE behind.
//! - **Grant propagation is eager.** The first grant on a workspace walks
//!   the whole tree.
//! - **A FAT volume has no DACLs**: a grant root on one fails, and targets
//!   on one stay writable.
//! - **The Job's account can fall short.** A process the Job counted but
//!   never reported (a lost port message, a failed association counted by a
//!   nested job's limits, a pid reused inside the Job before it was opened),
//!   or a member the runner cannot open, leaves the account unsettled: the
//!   run then ends as a cleanup failure with its grant and directory kept,
//!   never as a false empty tree. A reported pid that no longer opens, or
//!   names a process outside the Job, is read as ended on the process-id
//!   lifetime inference `bake-process` documents for its account.
//! - **The runner ignores Ctrl+C** (`SetConsoleCtrlHandler(NULL, TRUE)`, as
//!   TypeScript did) so it survives to clean up, and the command inherits
//!   that setting.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::paths::{canonical_path, is_path_under};

/// The hidden first argument that turns a Bake executable into the runner.
pub const HELPER_ARG: &str = "__bake-windows-acl-run";

/// The runner's name in its diagnostics (`RUNNER_SIGNATURE`).
pub const RUNNER_SIGNATURE: &str = "windows-acl-run";

/// Why the Windows default chain is empty: the detail of the default
/// provider's refusal there.
pub const DEFAULT_REFUSAL: &str = "the Windows restricted-token runner is not used: its token does not confine deletion (DELETE and FILE_DELETE_CHILD are not write accesses), so a confined command could delete or move files outside its writable roots";

/// The exit status of every runner failure (`RUNNER_FAILURE_EXIT`). The
/// command's own status passes through, 127 included, so a runner failure
/// needs this status and a `windows-acl-run: ` line.
pub const RUNNER_FAILURE_EXIT: i32 = 127;

/// The case-insensitive stderr substrings a denied write produces under
/// this runner (`DENIAL_SIGNATURES['windows-acl']`): Win32's text, .NET's,
/// and the C runtime's `EACCES` and `EPERM` texts.
pub const DENIAL_SIGNATURES: [&str; 4] = [
    "access is denied",
    "access to the path",
    "permission denied",
    "operation not permitted",
];

/// The capability ACE's access mask (`GRANT_MASK`): `FILE_GENERIC_WRITE`,
/// `DELETE`, and `FILE_DELETE_CHILD`, without `STANDARD_RIGHTS_WRITE`.
/// `WRITE_DAC` and `WRITE_OWNER` stay out, so a confined command cannot
/// rewrite a DACL or take ownership to widen its grant.
pub const GRANT_MASK: u32 =
    (FILE_GENERIC_WRITE | DELETE | FILE_DELETE_CHILD) & !STANDARD_RIGHTS_WRITE;

const FILE_GENERIC_WRITE: u32 = 0x0012_0116;
const DELETE: u32 = 0x0001_0000;
const FILE_DELETE_CHILD: u32 = 0x0040;
const STANDARD_RIGHTS_WRITE: u32 = 0x0002_0000;

/// The prefix of each run's private temporary directory under `--temp`.
pub const PRIVATE_TEMP_PREFIX: &str = "bake-acl-";

/// The file-effect mode a run confines under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerMode {
    /// No capability SID: every ACL-governed write is denied.
    ReadOnly,
    /// The workspace and the run's private temp directory are writable.
    WorkspaceWrite,
}

impl RunnerMode {
    /// The mode's name on the runner's command line.
    pub fn as_str(self) -> &'static str {
        match self {
            RunnerMode::ReadOnly => "read-only",
            RunnerMode::WorkspaceWrite => "workspace-write",
        }
    }
}

/// The workspace's capability SID (`workspaceWriteSid`): `S-1-4-x-y` from
/// the SHA-256 of the canonical workspace path, so every run on one
/// workspace carries the same SID and finds its standing ACE. A different
/// spelling of the same directory derives a different SID; callers pass the
/// canonical path.
pub fn workspace_write_sid(workspace_root: &str) -> String {
    let digest = sha256(&[workspace_root.as_bytes()]);
    let (first, second) = sid_subauthorities(&digest);
    format!("S-1-4-{first}-{second}")
}

/// One private temp directory's capability SID (`tempWriteSid`): the third
/// subauthority separates it from every workspace SID.
pub fn temp_write_sid(temp_dir: &str) -> String {
    let digest = sha256(&[b"temp\0", temp_dir.as_bytes()]);
    let (first, second) = sid_subauthorities(&digest);
    format!("S-1-4-{first}-{second}-1")
}

pub(super) fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut context = ring::digest::Context::new(&ring::digest::SHA256);
    for part in parts {
        context.update(part);
    }
    let mut out = [0; 32];
    out.copy_from_slice(context.finish().as_ref());
    out
}

/// Two 30-bit subauthorities, each in `1..=2^30 - 1`, from the digest's first
/// two little-endian words.
fn sid_subauthorities(digest: &[u8; 32]) -> (u32, u32) {
    let word = |offset: usize| {
        u32::from_le_bytes([
            digest[offset],
            digest[offset + 1],
            digest[offset + 2],
            digest[offset + 3],
        ])
    };
    let modulus = (1u32 << 30) - 1;
    (word(0) % modulus + 1, word(4) % modulus + 1)
}

/// Appends `argument` to a command line as `CommandLineToArgvW` reads it back
/// (`quoteArg`), as UTF-16 code units.
pub fn quote_arg(argument: &[u16], out: &mut Vec<u16>) {
    const QUOTE: u16 = b'"' as u16;
    const BACKSLASH: u16 = b'\\' as u16;
    let needs_quotes = argument.is_empty()
        || argument
            .iter()
            .any(|&unit| matches!(unit, 0x20 | 0x09 | 0x0a | 0x0b | 0x0c | 0x0d | QUOTE));
    if !needs_quotes {
        out.extend_from_slice(argument);
        return;
    }
    out.push(QUOTE);
    let mut index = 0;
    loop {
        let mut backslashes = 0;
        while index < argument.len() && argument[index] == BACKSLASH {
            backslashes += 1;
            index += 1;
        }
        if index == argument.len() {
            out.extend(std::iter::repeat_n(BACKSLASH, backslashes * 2));
            break;
        }
        if argument[index] == QUOTE {
            out.extend(std::iter::repeat_n(BACKSLASH, backslashes * 2 + 1));
            out.push(QUOTE);
        } else {
            out.extend(std::iter::repeat_n(BACKSLASH, backslashes));
            out.push(argument[index]);
        }
        index += 1;
    }
    out.push(QUOTE);
}

/// The command line for `argv` (`buildCommandLine`): each entry quoted, one
/// space apart, without a terminating NUL.
pub fn build_command_line<A: AsRef<[u16]>>(argv: &[A]) -> Vec<u16> {
    let mut line = Vec::new();
    for (index, argument) in argv.iter().enumerate() {
        if index > 0 {
            line.push(u16::from(b' '));
        }
        quote_arg(argument.as_ref(), &mut line);
    }
    line
}

/// The runner arguments a provider passes before `--` for `mode`, the
/// canonical `workspace`, and the canonical `temp_root`.
pub fn runner_profile_args(mode: RunnerMode, workspace: &Path, temp_root: &Path) -> Vec<OsString> {
    vec![
        "--workspace".into(),
        workspace.as_os_str().to_owned(),
        "--temp".into(),
        temp_root.as_os_str().to_owned(),
        "--mode".into(),
        mode.as_str().into(),
    ]
}

/// Refuses a temp root equal to or inside `workspace`
/// (`assertTempRootOutsideWorkspace`): every private directory created
/// below it would inherit the standing workspace capability. Both paths are
/// compared canonically.
pub fn check_temp_root_outside_workspace(workspace: &Path, temp_root: &Path) -> Result<(), String> {
    if is_path_under(&canonical_path(temp_root), &canonical_path(workspace)) {
        return Err(format!(
            "Windows ACL temp root must be outside the workspace: workspace={}; temp={}",
            workspace.display(),
            temp_root.display()
        ));
    }
    Ok(())
}

/// Parsed runner arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Cli {
    workspace: PathBuf,
    temp: PathBuf,
    mode: RunnerMode,
    command: Vec<OsString>,
}

/// `parseArgs`: the run, or the detail of its usage error.
fn parse(args: Vec<OsString>) -> Result<Cli, String> {
    let mut workspace = None;
    let mut temp = None;
    let mut mode = None;
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
    Ok(Cli {
        workspace: workspace.into(),
        temp: temp.into(),
        mode,
        command,
    })
}

/// Checks that `path` is an existing, canonical directory reached through
/// no link, as the provider resolved it.
pub(super) fn check_directory(label: &str, path: &Path) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(path).map_err(|error| {
        format!(
            "{label} is not an existing directory: {} ({error})",
            path.display()
        )
    })?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(format!(
            "{label} is not an existing directory: {}",
            path.display()
        ));
    }
    if canonical_path(path) != path {
        return Err(format!(
            "{label} is not a canonical path: {} resolves to {}",
            path.display(),
            canonical_path(path).display()
        ));
    }
    Ok(())
}

/// Runs the runner over `args`, the arguments after [`HELPER_ARG`], and never
/// returns: it exits with the command's status, or [`RUNNER_FAILURE_EXIT`]
/// after a `windows-acl-run: ` line. Call it before anything else runs.
pub fn helper_main(args: impl IntoIterator<Item = OsString>) -> ! {
    let code = run(args.into_iter().collect());
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    std::process::exit(code)
}

fn fail(detail: &str) -> i32 {
    eprintln!("{RUNNER_SIGNATURE}: {detail}");
    RUNNER_FAILURE_EXIT
}

fn run(args: Vec<OsString>) -> i32 {
    let cli = match parse(args) {
        Ok(cli) => cli,
        Err(detail) => return fail(&detail),
    };
    // Both directories are checked in both modes: a provider bug that passes
    // a bogus root fails here, before any grant.
    if let Err(detail) = check_directory("--workspace", &cli.workspace)
        .and_then(|()| check_directory("--temp", &cli.temp))
    {
        return fail(&detail);
    }
    if cli.mode == RunnerMode::WorkspaceWrite
        && let Err(detail) = check_temp_root_outside_workspace(&cli.workspace, &cli.temp)
    {
        return fail(&detail);
    }
    run_confined(&cli)
}

#[cfg(windows)]
fn run_confined(cli: &Cli) -> i32 {
    sys::run(cli)
}

#[cfg(not(windows))]
fn run_confined(_cli: &Cli) -> i32 {
    fail("the Windows ACL runner runs only on Windows")
}

/// The number of allow ACEs in `path`'s DACL that name the SID `sid`
/// (SDDL form), explicit or inherited, for inspecting where a grant
/// reached. Windows only.
#[cfg(windows)]
pub fn allow_aces_naming(path: &Path, sid: &str) -> Result<usize, String> {
    sys::allow_aces_naming(path, sid)
}

#[cfg(windows)]
pub(in crate::backends) mod sys;

#[cfg(test)]
mod tests;
